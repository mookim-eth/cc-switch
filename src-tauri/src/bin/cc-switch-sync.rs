//! Headless poller for the CC Switch local control API.
//!
//! Secrets are read only from the TOML configuration file and are never
//! accepted as command-line flags or persisted in the state file.

use chrono::{DateTime, Utc};
use reqwest::header::{AUTHORIZATION, ETAG, IF_NONE_MATCH};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

const DEFAULT_CC_SWITCH_URL: &str = "http://127.0.0.1:15721";
const DEFAULT_POLL_SECONDS: u64 = 30;
const MAX_BACKOFF_SECONDS: u64 = 300;
const MAX_REMOTE_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncConfig {
    remote_server_url: String,
    remote_access_token: String,
    cc_switch_control_token: String,
    #[serde(default = "default_cc_switch_url")]
    cc_switch_url: String,
    #[serde(default = "default_poll_seconds")]
    poll_interval_seconds: u64,
    apps: String,
    #[serde(default)]
    state_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteConfig {
    revision: String,
    routes: BTreeMap<String, RemoteRoute>,
    #[serde(default)]
    model_routes: BTreeMap<String, RemoteModelRoutes>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteRoute {
    endpoint: String,
    api_key: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteModelRouteRule {
    model: String,
    #[serde(default)]
    aliases: Vec<String>,
    providers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteModelRoutes {
    rules: Vec<RemoteModelRouteRule>,
    default_providers: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SyncState {
    #[serde(default)]
    last_applied_revision: Option<String>,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_success_at: Option<String>,
}

#[derive(Debug)]
enum SyncOutcome {
    Applied {
        revision: String,
        etag: Option<String>,
    },
    NotModified,
    Stale {
        etag: Option<String>,
    },
}

fn default_cc_switch_url() -> String {
    DEFAULT_CC_SWITCH_URL.to_string()
}

fn default_poll_seconds() -> u64 {
    DEFAULT_POLL_SECONDS
}

fn redact_origin(raw: &str) -> String {
    Url::parse(raw)
        .ok()
        .filter(|url| url.has_host())
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|| "[invalid endpoint]".to_string())
}

fn validate_config(config: &SyncConfig) -> Result<Vec<String>, String> {
    let remote = Url::parse(config.remote_server_url.trim())
        .map_err(|_| "remote_server_url must be an absolute HTTPS URL".to_string())?;
    if remote.scheme() != "https" || !remote.has_host() {
        return Err("remote_server_url must be an absolute HTTPS URL".to_string());
    }
    if !remote.username().is_empty() || remote.password().is_some() || remote.fragment().is_some() {
        return Err("remote_server_url must not contain credentials or a fragment".to_string());
    }
    let local = Url::parse(config.cc_switch_url.trim())
        .map_err(|_| "cc_switch_url must be an absolute loopback HTTP URL".to_string())?;
    let local_host = local.host_str().unwrap_or_default();
    let local_loopback = local_host.eq_ignore_ascii_case("localhost")
        || local_host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !matches!(local.scheme(), "http" | "https") || !local_loopback {
        return Err("cc_switch_url must use a loopback host".to_string());
    }
    if !local.username().is_empty()
        || local.password().is_some()
        || local.query().is_some()
        || local.fragment().is_some()
        || !matches!(local.path(), "" | "/")
    {
        return Err(
            "cc_switch_url must be a loopback origin without credentials or a path".to_string(),
        );
    }
    if config.remote_access_token.trim().is_empty()
        || config.cc_switch_control_token.trim().is_empty()
    {
        return Err("both access tokens are required".to_string());
    }
    if config.poll_interval_seconds == 0 {
        return Err("poll_interval_seconds must be greater than zero".to_string());
    }

    let mut apps = Vec::new();
    for app in config
        .apps
        .split(',')
        .map(str::trim)
        .filter(|app| !app.is_empty())
    {
        if !matches!(app, "claude" | "codex" | "gemini" | "grokbuild") {
            return Err(format!("unsupported app: {app}"));
        }
        if !apps.iter().any(|existing| existing == app) {
            apps.push(app.to_string());
        }
    }
    if apps.is_empty() {
        return Err("apps must contain at least one supported app".to_string());
    }
    Ok(apps)
}

fn validate_remote(remote: &RemoteConfig, apps: &[String]) -> Result<DateTime<Utc>, String> {
    let revision = DateTime::parse_from_rfc3339(remote.revision.trim())
        .map_err(|_| "remote revision must be an RFC 3339 timestamp".to_string())?
        .with_timezone(&Utc);
    for app in apps {
        let route = remote
            .routes
            .get(app)
            .ok_or_else(|| format!("remote configuration is missing route for {app}"))?;
        let endpoint = Url::parse(route.endpoint.trim())
            .map_err(|_| format!("route endpoint for {app} is invalid"))?;
        if endpoint.scheme() != "https" || !endpoint.has_host() {
            return Err(format!("route endpoint for {app} must use HTTPS"));
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(format!(
                "route endpoint for {app} contains forbidden URL components"
            ));
        }
        if route.api_key.trim().is_empty() {
            return Err(format!("route apiKey for {app} is empty"));
        }
    }
    Ok(revision)
}

fn read_state(path: &Path) -> SyncState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_state(path: &Path, state: &SyncState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("tmp");
    let contents = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, contents).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

async fn check_cc_switch(client: &reqwest::Client, base_url: &str) -> Result<(), String> {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "CC Switch is unavailable".to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "CC Switch health returned HTTP {}",
            response.status()
        ))
    }
}

async fn sync_once(
    client: &reqwest::Client,
    config: &SyncConfig,
    apps: &[String],
    state: &SyncState,
) -> Result<SyncOutcome, String> {
    check_cc_switch(client, &config.cc_switch_url).await?;

    let mut request = client.get(config.remote_server_url.trim()).header(
        AUTHORIZATION,
        format!("Bearer {}", config.remote_access_token.trim()),
    );
    if let Some(etag) = state.etag.as_deref() {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "remote configuration service is unavailable".to_string())?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(SyncOutcome::NotModified);
    }
    if !response.status().is_success() {
        return Err(format!(
            "remote configuration service returned HTTP {}",
            response.status()
        ));
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REMOTE_CONFIG_BYTES as u64)
    {
        return Err("remote configuration response is too large".to_string());
    }
    let mut response_bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "remote configuration response is invalid".to_string())?
    {
        if response_bytes.len().saturating_add(chunk.len()) > MAX_REMOTE_CONFIG_BYTES {
            return Err("remote configuration response is too large".to_string());
        }
        response_bytes.extend_from_slice(&chunk);
    }
    let remote: RemoteConfig = serde_json::from_slice(&response_bytes)
        .map_err(|_| "remote configuration response is invalid".to_string())?;
    let remote_revision = validate_remote(&remote, apps)?;

    if let Some(previous) = state.last_applied_revision.as_deref() {
        let previous_revision = DateTime::parse_from_rfc3339(previous)
            .map_err(|_| "saved revision is invalid; remove the state file".to_string())?
            .with_timezone(&Utc);
        if remote_revision <= previous_revision {
            return Ok(SyncOutcome::Stale { etag });
        }
    }

    for app in apps {
        let route = remote.routes.get(app).expect("validated route exists");
        let endpoint_origin = redact_origin(&route.endpoint);
        let payload = serde_json::json!({
            "revision": remote.revision,
            "name": route.name,
            "endpoint": route.endpoint,
            "apiKey": route.api_key,
            "model": route.model,
        });
        let control_url = format!(
            "{}/control/v1/routes/{app}",
            config.cc_switch_url.trim_end_matches('/')
        );
        let response = client
            .put(control_url)
            .header(
                AUTHORIZATION,
                format!("Bearer {}", config.cc_switch_control_token.trim()),
            )
            .json(&payload)
            .send()
            .await
            .map_err(|_| format!("CC Switch route apply failed for {app}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "CC Switch route apply failed: app={app}, revision={}, status={}, endpoint={endpoint_origin}",
                remote.revision,
                response.status()
            ));
        }
        eprintln!(
            "route applied: app={app}, revision={}, status={}, endpoint={endpoint_origin}",
            remote.revision,
            response.status()
        );
    }

    for app in apps {
        // The remote document is a full snapshot. Absence therefore clears
        // stale model-specific rules and restores normal app-level routing.
        let empty_model_routes = RemoteModelRoutes {
            rules: Vec::new(),
            default_providers: Vec::new(),
        };
        let model_routes = remote.model_routes.get(app).unwrap_or(&empty_model_routes);
        let control_url = format!(
            "{}/control/v1/model-routes/{app}",
            config.cc_switch_url.trim_end_matches('/')
        );
        let payload = serde_json::json!({
            "revision": remote.revision,
            "rules": model_routes.rules,
            "defaultProviders": model_routes.default_providers,
        });
        let response = client
            .put(control_url)
            .header(
                AUTHORIZATION,
                format!("Bearer {}", config.cc_switch_control_token.trim()),
            )
            .json(&payload)
            .send()
            .await
            .map_err(|_| format!("CC Switch model route apply failed for {app}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "CC Switch model route apply failed: app={app}, revision={}, status={}",
                remote.revision,
                response.status()
            ));
        }
        eprintln!(
            "model routes applied: app={app}, revision={}, status={}",
            remote.revision,
            response.status()
        );
    }

    Ok(SyncOutcome::Applied {
        revision: remote.revision,
        etag,
    })
}

fn config_path() -> Result<PathBuf, String> {
    let mut args = std::env::args_os();
    let _binary = args.next();
    let path = args
        .next()
        .ok_or_else(|| "usage: cc-switch-sync <config.toml>".to_string())?;
    if args.next().is_some() {
        return Err("usage: cc-switch-sync <config.toml>".to_string());
    }
    Ok(PathBuf::from(path))
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("cc-switch-sync: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let config_path = config_path()?;
    let contents = std::fs::read_to_string(&config_path)
        .map_err(|error| format!("failed to read config file: {error}"))?;
    let config: SyncConfig =
        toml::from_str(&contents).map_err(|_| "invalid config file".to_string())?;
    let apps = validate_config(&config)?;
    let state_path = config
        .state_file
        .clone()
        .unwrap_or_else(|| config_path.with_extension("state.json"));
    let mut state = read_state(&state_path);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let normal_delay = Duration::from_secs(config.poll_interval_seconds);
    let mut failures = 0_u32;

    loop {
        let delay = match sync_once(&client, &config, &apps, &state).await {
            Ok(SyncOutcome::Applied { revision, etag }) => {
                state.last_applied_revision = Some(revision);
                state.etag = etag;
                state.last_success_at = Some(Utc::now().to_rfc3339());
                save_state(&state_path, &state)?;
                failures = 0;
                normal_delay
            }
            Ok(SyncOutcome::NotModified) => {
                failures = 0;
                normal_delay
            }
            Ok(SyncOutcome::Stale { etag }) => {
                eprintln!("ignored unchanged or older remote configuration");
                state.etag = etag;
                save_state(&state_path, &state)?;
                failures = 0;
                normal_delay
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                let exponent = failures.saturating_sub(1).min(6);
                let seconds = config
                    .poll_interval_seconds
                    .saturating_mul(1_u64 << exponent)
                    .min(MAX_BACKOFF_SECONDS);
                eprintln!("sync failed: {error}; retrying in {seconds}s");
                Duration::from_secs(seconds)
            }
        };

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            result = shutdown_signal() => {
                result?;
                eprintln!("shutdown requested");
                return Ok(());
            }
        }
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), String> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "failed to listen for shutdown".to_string())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.map_err(|_| "failed to listen for shutdown".to_string())
        }
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), String> {
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| "failed to listen for shutdown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_state_contains_no_credentials() {
        let state = SyncState {
            last_applied_revision: Some("2026-09-21T10:30:00Z".to_string()),
            etag: Some("\"revision-1\"".to_string()),
            last_success_at: Some("2026-09-21T10:30:01Z".to_string()),
        };
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(!serialized.contains("apiKey"));
        assert!(!serialized.contains("token"));
    }

    #[test]
    fn endpoint_logs_only_origin() {
        assert_eq!(
            redact_origin("https://example.com/secret/path?token=hidden"),
            "https://example.com"
        );
    }
}
