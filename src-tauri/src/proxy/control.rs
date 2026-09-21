//! Narrow, authenticated control plane for applying remotely managed routes.
//!
//! This deliberately accepts only a small provider DTO. It never accepts an
//! internal Provider document and delegates all mutations to ProviderService.

use crate::app_config::AppType;
use crate::deeplink::{build_provider_from_request, DeepLinkImportRequest};
use crate::provider::Provider;
use crate::services::ProviderService;
use crate::store::AppState;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use url::Url;

use super::server::ProxyState;

pub const CONTROL_BODY_LIMIT: usize = 64 * 1024;
pub const REMOTE_MANAGED_PROVIDER_ID: &str = "remote-managed";

const CONTROL_ENABLED_KEY: &str = "local_control_enabled";
const CONTROL_TOKEN_KEY: &str = "local_control_token";
const CONTROL_ALLOW_HTTP_LOOPBACK_KEY: &str = "local_control_allow_http_loopback";
const MAX_REVISION_LEN: usize = 256;
const MAX_NAME_LEN: usize = 128;
const MAX_ENDPOINT_LEN: usize = 4096;
const MAX_API_KEY_LEN: usize = 16 * 1024;
const MAX_MODEL_LEN: usize = 512;

fn model_routes_key(app: &str) -> String {
    format!("model_routes_{app}")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelRouteRule {
    pub model: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub providers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelRoutesConfig {
    pub revision: String,
    pub rules: Vec<ModelRouteRule>,
    pub default_providers: Vec<String>,
}

impl ModelRoutesConfig {
    /// Resolve with the documented priority: exact, alias, wildcard, default.
    pub(crate) fn resolve(&self, model: &str) -> (&[String], String) {
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| !rule.model.contains('*') && rule.model == model)
        {
            return (&rule.providers, format!("exact:{}", rule.model));
        }
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| rule.aliases.iter().any(|alias| alias == model))
        {
            return (&rule.providers, format!("alias:{}", rule.model));
        }
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| rule.model.contains('*') && glob_matches(&rule.model, model))
        {
            return (&rule.providers, format!("wildcard:{}", rule.model));
        }
        (&self.default_providers, "default".to_string())
    }
}

pub(crate) fn load_model_routes(
    db: &crate::database::Database,
    app: &str,
) -> Result<Option<ModelRoutesConfig>, crate::error::AppError> {
    db.get_setting(&model_routes_key(app))?
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| {
                crate::error::AppError::Database(format!(
                    "Invalid saved model routes for {app}: {error}"
                ))
            })
        })
        .transpose()
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut reachable = vec![false; value.len() + 1];
    reachable[0] = true;
    for token in pattern {
        if token == '*' {
            for index in 1..=value.len() {
                reachable[index] |= reachable[index - 1];
            }
        } else {
            for index in (1..=value.len()).rev() {
                reachable[index] = reachable[index - 1] && value[index - 1] == token;
            }
            reachable[0] = false;
        }
    }
    reachable[value.len()]
}

/// Return true when two `*`-only glob languages overlap. This is a small NFA
/// product search; rejecting overlaps makes wildcard routing independent of
/// rule order.
fn globs_overlap(left: &str, right: &str) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut queue = VecDeque::from([(0_usize, 0_usize)]);
    let mut visited = HashSet::new();
    while let Some((i, j)) = queue.pop_front() {
        if !visited.insert((i, j)) {
            continue;
        }
        if i == left.len() && j == right.len() {
            return true;
        }
        let a = left.get(i).copied();
        let b = right.get(j).copied();
        if a == Some('*') {
            queue.push_back((i + 1, j));
        }
        if b == Some('*') {
            queue.push_back((i, j + 1));
        }
        match (a, b) {
            (Some('*'), Some('*')) => {
                // Consuming a character in both stars returns to this state;
                // the epsilon transitions above are sufficient for reachability.
            }
            (Some('*'), Some(_)) => queue.push_back((i, j + 1)),
            (Some(_), Some('*')) => queue.push_back((i + 1, j)),
            (Some(a), Some(b)) if a == b => queue.push_back((i + 1, j + 1)),
            _ => {}
        }
    }
    false
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalControlConfig {
    pub enabled: bool,
    pub token_configured: bool,
    pub allow_http_loopback: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyRouteRequest {
    revision: String,
    #[serde(default)]
    name: Option<String>,
    endpoint: String,
    api_key: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyRouteResponse {
    app: String,
    provider_id: String,
    revision: String,
    active: bool,
    takeover_active: bool,
}

#[derive(Debug)]
pub(crate) struct ControlError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ControlError {
    fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
        }
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "service_unavailable",
            message: "The local route could not be applied",
        }
    }
}

impl IntoResponse for ControlError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CONTENT_TYPE, "application/json")],
            axum::Json(json!({
                "error": self.code,
                "message": self.message,
            })),
        )
            .into_response()
    }
}

pub fn get_local_control_config(state: &AppState) -> Result<LocalControlConfig, String> {
    Ok(LocalControlConfig {
        enabled: state
            .db
            .get_bool_flag(CONTROL_ENABLED_KEY)
            .map_err(|error| error.to_string())?,
        token_configured: state
            .db
            .get_setting(CONTROL_TOKEN_KEY)
            .map_err(|error| error.to_string())?
            .is_some_and(|token| !token.is_empty()),
        allow_http_loopback: state
            .db
            .get_bool_flag(CONTROL_ALLOW_HTTP_LOOPBACK_KEY)
            .map_err(|error| error.to_string())?,
    })
}

/// Enable or disable the control plane. The token is returned only when this
/// call has to create one, so ordinary status reads cannot expose it.
pub fn set_local_control_enabled(
    state: &AppState,
    enabled: bool,
) -> Result<Option<String>, String> {
    let generated = if enabled
        && state
            .db
            .get_setting(CONTROL_TOKEN_KEY)
            .map_err(|error| error.to_string())?
            .is_none_or(|token| token.is_empty())
    {
        let token = generate_control_token();
        state
            .db
            .set_setting(CONTROL_TOKEN_KEY, &token)
            .map_err(|error| error.to_string())?;
        Some(token)
    } else {
        None
    };

    state
        .db
        .set_setting(CONTROL_ENABLED_KEY, if enabled { "true" } else { "false" })
        .map_err(|error| error.to_string())?;
    Ok(generated)
}

pub fn rotate_local_control_token(state: &AppState) -> Result<String, String> {
    let token = generate_control_token();
    state
        .db
        .set_setting(CONTROL_TOKEN_KEY, &token)
        .map_err(|error| error.to_string())?;
    Ok(token)
}

pub fn set_allow_http_loopback(state: &AppState, allowed: bool) -> Result<(), String> {
    state
        .db
        .set_setting(
            CONTROL_ALLOW_HTTP_LOOPBACK_KEY,
            if allowed { "true" } else { "false" },
        )
        .map_err(|error| error.to_string())
}

fn generate_control_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn authenticate(
    db: &crate::database::Database,
    remote_addr: SocketAddr,
    headers: &HeaderMap,
) -> Result<(), ControlError> {
    if !remote_addr.ip().is_loopback() {
        return Err(ControlError {
            status: StatusCode::FORBIDDEN,
            code: "loopback_required",
            message: "The control API is available only from the local machine",
        });
    }
    if headers.contains_key(header::ORIGIN) {
        return Err(ControlError {
            status: StatusCode::FORBIDDEN,
            code: "browser_origin_forbidden",
            message: "Browser-originated control requests are not allowed",
        });
    }
    if !db
        .get_bool_flag(CONTROL_ENABLED_KEY)
        .map_err(|_| ControlError::unavailable())?
    {
        return Err(ControlError {
            status: StatusCode::FORBIDDEN,
            code: "control_disabled",
            message: "The local control API is disabled",
        });
    }

    let expected = db
        .get_setting(CONTROL_TOKEN_KEY)
        .map_err(|_| ControlError::unavailable())?
        .filter(|token| !token.is_empty())
        .ok_or_else(ControlError::unavailable)?;
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or(ControlError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "A valid bearer token is required",
        })?;

    // HMAC verification gives us a constant-time comparison without ever
    // including either token in diagnostics.
    let mut expected_mac = Hmac::<Sha256>::new_from_slice(expected.as_bytes())
        .map_err(|_| ControlError::unavailable())?;
    expected_mac.update(b"cc-switch-local-control");
    let expected_tag = expected_mac.finalize().into_bytes();
    let mut supplied_mac = Hmac::<Sha256>::new_from_slice(supplied.as_bytes())
        .map_err(|_| ControlError::unavailable())?;
    supplied_mac.update(b"cc-switch-local-control");
    supplied_mac
        .verify_slice(&expected_tag)
        .map_err(|_| ControlError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "A valid bearer token is required",
        })
}

fn validate_content_type(headers: &HeaderMap) -> Result<(), ControlError> {
    let valid = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
    if valid {
        Ok(())
    } else {
        Err(ControlError::bad_request(
            "Content-Type must be application/json",
        ))
    }
}

fn validate_text(
    value: &str,
    max_len: usize,
    empty_message: &'static str,
    long_message: &'static str,
) -> Result<String, ControlError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ControlError::bad_request(empty_message));
    }
    if value.len() > max_len
        || value
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
    {
        return Err(ControlError::bad_request(long_message));
    }
    Ok(value.to_string())
}

fn validate_endpoint(endpoint: &str, allow_http_loopback: bool) -> Result<String, ControlError> {
    let endpoint = validate_text(
        endpoint,
        MAX_ENDPOINT_LEN,
        "endpoint is required",
        "endpoint is invalid",
    )?;
    let parsed = Url::parse(&endpoint)
        .map_err(|_| ControlError::bad_request("endpoint must be an absolute URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ControlError::bad_request(
            "endpoint must not contain user information",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ControlError::bad_request(
            "endpoint must not contain a query or fragment",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ControlError::bad_request("endpoint host is required"))?;
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match parsed.scheme() {
        "https" => {}
        "http" if allow_http_loopback && is_loopback => {}
        _ => {
            return Err(ControlError::bad_request(
                "endpoint must use HTTPS (HTTP is allowed only for explicitly enabled loopback development)",
            ))
        }
    }
    Ok(endpoint.trim_end_matches('/').to_string())
}

fn build_provider(
    app: &AppType,
    request: &ApplyRouteRequest,
    allow_http_loopback: bool,
) -> Result<(Provider, String), ControlError> {
    let revision = validate_text(
        &request.revision,
        MAX_REVISION_LEN,
        "revision is required",
        "revision is invalid",
    )?;
    let endpoint = validate_endpoint(&request.endpoint, allow_http_loopback)?;
    let api_key = validate_text(
        &request.api_key,
        MAX_API_KEY_LEN,
        "apiKey is required",
        "apiKey is invalid",
    )?;
    let name = match request.name.as_deref() {
        Some(name) => validate_text(name, MAX_NAME_LEN, "name is invalid", "name is invalid")?,
        None => "Remote Managed".to_string(),
    };
    let model = request
        .model
        .as_deref()
        .map(|model| validate_text(model, MAX_MODEL_LEN, "model is invalid", "model is invalid"))
        .transpose()?;
    let homepage = Url::parse(&endpoint)
        .ok()
        .map(|url| url.origin().ascii_serialization());
    let dto = DeepLinkImportRequest {
        version: "v1".to_string(),
        resource: "provider".to_string(),
        app: Some(app.as_str().to_string()),
        name: Some(name),
        homepage,
        endpoint: Some(endpoint),
        api_key: Some(api_key),
        model,
        ..Default::default()
    };
    let mut provider = build_provider_from_request(app, &dto)
        .map_err(|_| ControlError::bad_request("route configuration is invalid"))?;
    provider.id = REMOTE_MANAGED_PROVIDER_ID.to_string();
    Ok((provider, revision))
}

fn rollback_provider(
    state: &AppState,
    app: AppType,
    previous: Option<Provider>,
    previous_current: Option<String>,
) -> Result<(), String> {
    match previous {
        Some(provider) => ProviderService::update(state, app.clone(), None, provider)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        None => ProviderService::delete(state, app.clone(), REMOTE_MANAGED_PROVIDER_ID)
            .map_err(|error| error.to_string()),
    }?;
    if let Some(provider_id) = previous_current {
        ProviderService::switch(state, app, &provider_id).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub async fn apply_remote_route(
    State(state): State<ProxyState>,
    Extension(remote_addr): Extension<SocketAddr>,
    Path(app_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ControlError> {
    authenticate(&state.db, remote_addr, &headers)?;
    validate_content_type(&headers)?;

    let app = AppType::from_str(&app_name)
        .map_err(|_| ControlError::bad_request("unsupported application"))?;
    if !app.supports_local_proxy() {
        return Err(ControlError::bad_request("unsupported application"));
    }
    let takeover_active = state
        .db
        .get_proxy_config_for_app(app.as_str())
        .await
        .map_err(|_| ControlError::unavailable())?
        .enabled;
    if !takeover_active {
        return Err(ControlError::unavailable());
    }
    if crate::settings::get_effective_current_provider(&state.db, &app)
        .map_err(|_| ControlError::unavailable())?
        .is_none()
    {
        return Err(ControlError::unavailable());
    }
    let allow_http_loopback = state
        .db
        .get_bool_flag(CONTROL_ALLOW_HTTP_LOOPBACK_KEY)
        .map_err(|_| ControlError::unavailable())?;
    let request: ApplyRouteRequest = serde_json::from_slice(&body)
        .map_err(|_| ControlError::bad_request("request body must be valid JSON"))?;
    let (provider, revision) = build_provider(&app, &request, allow_http_loopback)?;

    let control_state = state.control_state.clone();
    let app_for_commit = app.clone();
    let commit = tokio::task::spawn_blocking(move || {
        let previous = control_state
            .db
            .get_provider_by_id(REMOTE_MANAGED_PROVIDER_ID, app_for_commit.as_str())
            .map_err(|error| error.to_string())?;
        let previous_current =
            crate::settings::get_effective_current_provider(&control_state.db, &app_for_commit)
                .map_err(|error| error.to_string())?;
        let mutation = if previous.is_some() {
            ProviderService::update(&control_state, app_for_commit.clone(), None, provider)
        } else {
            ProviderService::add(&control_state, app_for_commit.clone(), provider, false)
        };
        if let Err(error) = mutation {
            if rollback_provider(&control_state, app_for_commit, previous, previous_current)
                .is_err()
            {
                log::error!("Local control route mutation rollback failed (details suppressed)");
            }
            return Err(error.to_string());
        }

        if let Err(error) = ProviderService::switch(
            &control_state,
            app_for_commit.clone(),
            REMOTE_MANAGED_PROVIDER_ID,
        ) {
            if rollback_provider(&control_state, app_for_commit, previous, previous_current)
                .is_err()
            {
                log::error!("Local control switch rollback failed (details suppressed)");
            }
            return Err(error.to_string());
        }
        Ok(())
    })
    .await
    .map_err(|_| ControlError::unavailable())?;

    if let Err(error) = commit {
        // The underlying message can contain provider configuration details;
        // never return it to the caller or copy it into ordinary logs.
        log::warn!("Local control route apply failed (details suppressed)");
        return Err(if error.contains("official") || error.contains("官方") {
            ControlError {
                status: StatusCode::CONFLICT,
                code: "provider_conflict",
                message: "The provider cannot be used during proxy takeover",
            }
        } else {
            ControlError::unavailable()
        });
    }

    log::info!(
        "Applied local control route: app={}, revision={}, endpoint={}",
        app.as_str(),
        revision,
        crate::redact_url_origin_for_log(&request.endpoint)
    );
    Ok((
        StatusCode::OK,
        axum::Json(ApplyRouteResponse {
            app: app.as_str().to_string(),
            provider_id: REMOTE_MANAGED_PROVIDER_ID.to_string(),
            revision,
            active: true,
            takeover_active,
        }),
    ))
}

fn validate_provider_queue(
    state: &ProxyState,
    app: &AppType,
    providers: &[String],
) -> Result<(), ControlError> {
    if providers.is_empty() {
        return Err(ControlError::bad_request(
            "provider candidate queues must not be empty",
        ));
    }
    let mut unique = HashSet::new();
    for provider_id in providers {
        let provider_id = validate_text(
            provider_id,
            256,
            "provider IDs must not be empty",
            "provider ID is invalid",
        )?;
        if !unique.insert(provider_id.clone()) {
            return Err(ControlError::bad_request(
                "provider candidate queues must not contain duplicates",
            ));
        }
        let provider = state
            .db
            .get_provider_by_id(&provider_id, app.as_str())
            .map_err(|_| ControlError::unavailable())?
            .ok_or_else(|| ControlError::bad_request("model route provider does not exist"))?;
        if provider.category.as_deref() == Some("official")
            && !crate::services::provider::official_provider_supports_proxy_takeover(app, &provider)
        {
            return Err(ControlError {
                status: StatusCode::CONFLICT,
                code: "provider_conflict",
                message: "A model route provider cannot be used during proxy takeover",
            });
        }
    }
    Ok(())
}

fn validate_model_routes(
    state: &ProxyState,
    app: &AppType,
    config: &mut ModelRoutesConfig,
) -> Result<(), ControlError> {
    config.revision = validate_text(
        &config.revision,
        MAX_REVISION_LEN,
        "revision is required",
        "revision is invalid",
    )?;
    if config.rules.len() > 256 {
        return Err(ControlError::bad_request("too many model route rules"));
    }
    // An empty default queue deliberately means “fall back to the existing
    // app-level current/failover route” when no model rule matches.
    if !config.default_providers.is_empty() {
        validate_provider_queue(state, app, &config.default_providers)?;
    }

    let mut exact_models = HashSet::new();
    let mut aliases = HashSet::new();
    let mut wildcard_patterns: Vec<String> = Vec::new();
    for rule in &mut config.rules {
        rule.model = validate_text(
            &rule.model,
            MAX_MODEL_LEN,
            "model pattern is required",
            "model pattern is invalid",
        )?;
        if rule.model.contains("**") {
            return Err(ControlError::bad_request(
                "model patterns must not contain adjacent wildcards",
            ));
        }
        validate_provider_queue(state, app, &rule.providers)?;
        if rule.model.contains('*') {
            if wildcard_patterns
                .iter()
                .any(|existing| globs_overlap(existing, &rule.model))
            {
                return Err(ControlError::bad_request(
                    "wildcard model route rules overlap",
                ));
            }
            wildcard_patterns.push(rule.model.clone());
        } else if !exact_models.insert(rule.model.clone()) {
            return Err(ControlError::bad_request(
                "duplicate exact model route rule",
            ));
        }

        if rule.aliases.len() > 256 {
            return Err(ControlError::bad_request("too many model aliases"));
        }
        for alias in &mut rule.aliases {
            *alias = validate_text(
                alias,
                MAX_MODEL_LEN,
                "model alias is invalid",
                "model alias is invalid",
            )?;
            if alias.contains('*') || !aliases.insert(alias.clone()) {
                return Err(ControlError::bad_request(
                    "model aliases must be unique and cannot contain wildcards",
                ));
            }
        }
    }
    Ok(())
}

pub async fn apply_model_routes(
    State(state): State<ProxyState>,
    Extension(remote_addr): Extension<SocketAddr>,
    Path(app_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ControlError> {
    authenticate(&state.db, remote_addr, &headers)?;
    validate_content_type(&headers)?;
    let app = AppType::from_str(&app_name)
        .map_err(|_| ControlError::bad_request("unsupported application"))?;
    if !app.supports_local_proxy() {
        return Err(ControlError::bad_request("unsupported application"));
    }
    let takeover_active = state
        .db
        .get_proxy_config_for_app(app.as_str())
        .await
        .map_err(|_| ControlError::unavailable())?
        .enabled;
    if !takeover_active {
        return Err(ControlError::unavailable());
    }

    let mut config: ModelRoutesConfig = serde_json::from_slice(&body)
        .map_err(|_| ControlError::bad_request("request body must be valid JSON"))?;
    validate_model_routes(&state, &app, &mut config)?;
    let serialized = serde_json::to_string(&config).map_err(|_| ControlError::unavailable())?;
    state
        .db
        .set_setting(&model_routes_key(app.as_str()), &serialized)
        .map_err(|_| ControlError::unavailable())?;

    log::info!(
        "Applied model routes: app={}, revision={}, rules={}",
        app.as_str(),
        config.revision,
        config.rules.len()
    );
    Ok((
        StatusCode::OK,
        axum::Json(json!({
            "app": app.as_str(),
            "revision": config.revision,
            "ruleCount": config.rules.len(),
            "takeoverActive": true,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use std::sync::Arc;

    #[test]
    fn token_generation_has_at_least_32_random_bytes_of_output() {
        let first = generate_control_token();
        let second = generate_control_token();
        assert_eq!(first.len(), 43);
        assert!(first
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')));
        assert_ne!(first, second);
    }

    #[test]
    fn control_is_disabled_by_default_and_token_is_not_exposed() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let config = get_local_control_config(&state).unwrap();
        assert!(!config.enabled);
        assert!(!config.token_configured);
    }

    #[test]
    fn enabling_creates_one_token_and_rotation_invalidates_it() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let initial = set_local_control_enabled(&state, true)
            .unwrap()
            .expect("first enable creates token");
        assert!(set_local_control_enabled(&state, true).unwrap().is_none());
        let rotated = rotate_local_control_token(&state).unwrap();
        assert_ne!(initial, rotated);
        assert_eq!(
            state.db.get_setting(CONTROL_TOKEN_KEY).unwrap().as_deref(),
            Some(rotated.as_str())
        );
    }

    #[test]
    fn authentication_rejects_non_loopback_and_wrong_tokens() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let token = set_local_control_enabled(&state, true)
            .unwrap()
            .expect("token");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let non_loopback: SocketAddr = "192.0.2.10:42000".parse().unwrap();
        assert_eq!(
            authenticate(&state.db, non_loopback, &headers)
                .unwrap_err()
                .status,
            StatusCode::FORBIDDEN
        );

        headers.insert(header::AUTHORIZATION, "Bearer wrong-token".parse().unwrap());
        let loopback: SocketAddr = "127.0.0.1:42000".parse().unwrap();
        assert_eq!(
            authenticate(&state.db, loopback, &headers)
                .unwrap_err()
                .status,
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn endpoint_policy_requires_https_except_opted_in_loopback() {
        assert!(validate_endpoint("https://api.example.com/v1", false).is_ok());
        assert!(validate_endpoint("http://api.example.com/v1", true).is_err());
        assert!(validate_endpoint("http://127.0.0.1:8000/v1", false).is_err());
        assert!(validate_endpoint("http://127.0.0.1:8000/v1", true).is_ok());
        assert!(validate_endpoint("https://user:secret@example.com", false).is_err());
        assert!(validate_endpoint("https://example.com?v=secret", false).is_err());
    }

    #[test]
    fn model_route_priority_is_exact_then_alias_then_wildcard_then_default() {
        let routes = ModelRoutesConfig {
            revision: "r1".to_string(),
            rules: vec![
                ModelRouteRule {
                    model: "gpt-*".to_string(),
                    aliases: vec![],
                    providers: vec!["wildcard".to_string()],
                },
                ModelRouteRule {
                    model: "gpt-5.4".to_string(),
                    aliases: vec!["fast".to_string()],
                    providers: vec!["exact".to_string()],
                },
            ],
            default_providers: vec!["default".to_string()],
        };
        assert_eq!(routes.resolve("gpt-5.4").0, ["exact"]);
        assert_eq!(routes.resolve("fast").0, ["exact"]);
        assert_eq!(routes.resolve("gpt-4.1").0, ["wildcard"]);
        assert_eq!(routes.resolve("claude").0, ["default"]);
    }

    #[test]
    fn wildcard_overlap_detection_is_order_independent() {
        assert!(globs_overlap("gpt-*", "*-5.4"));
        assert!(globs_overlap("claude-*", "claude-sonnet-*"));
        assert!(!globs_overlap("gpt-*", "claude-*"));
        assert!(!globs_overlap("gpt-5", "gpt-4"));
    }
}
