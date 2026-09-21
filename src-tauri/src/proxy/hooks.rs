//! Optional loopback HTTP hooks for proxy lifecycle decisions.
//!
//! Hook configuration is local-only and disabled by default.  Authentication
//! material is never included in hook payloads or diagnostic messages.

use crate::database::Database;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc, time::Duration};

const SETTINGS_KEY: &str = "proxy_hook_config_v1";
const DEFAULT_MAX_PAYLOAD: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookConfig {
    #[serde(default)]
    pub enabled: bool,
    pub endpoint: String,
    pub bearer_token: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_payload")]
    pub max_payload_bytes: usize,
    #[serde(default)]
    pub fail_closed: bool,
    #[serde(default)]
    pub allow_request_replace: bool,
    #[serde(default)]
    pub sensitive_strings: Vec<String>,
}

fn default_timeout_ms() -> u64 {
    1500
}

fn default_max_payload() -> usize {
    DEFAULT_MAX_PAYLOAD
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            bearer_token: String::new(),
            timeout_ms: default_timeout_ms(),
            max_payload_bytes: default_max_payload(),
            fail_closed: false,
            allow_request_replace: false,
            sensitive_strings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    BeforeRequest,
    AfterResponse,
    BeforeToolCall,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInput<'a> {
    pub request_id: &'a str,
    pub phase: HookPhase,
    pub app: &'a str,
    pub client_model: &'a str,
    pub outbound_model: Option<&'a str>,
    pub provider_id: &'a str,
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum WireDecision {
    Allow,
    Block {
        reason: Option<String>,
        rule_id: Option<String>,
    },
    Replace {
        payload: Value,
        rule_id: Option<String>,
    },
    Audit {
        event: String,
        rule_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum HookDecision {
    Allow,
    Block {
        reason: String,
        rule_id: Option<String>,
    },
    Replace {
        payload: Value,
        rule_id: Option<String>,
    },
    Audit {
        event: String,
        rule_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct HookFailure {
    pub fail_closed: bool,
    pub message: &'static str,
}

pub fn get_config(db: &Database) -> Result<HookConfig, String> {
    let Some(raw) = db.get_setting(SETTINGS_KEY).map_err(|e| e.to_string())? else {
        return Ok(HookConfig::default());
    };
    serde_json::from_str(&raw).map_err(|_| "Hook 配置损坏".to_string())
}

pub fn save_config(db: &Database, mut config: HookConfig) -> Result<(), String> {
    validate_config(&config)?;
    config.sensitive_strings = normalized_sensitive_strings(&config.sensitive_strings);
    let raw = serde_json::to_string(&config).map_err(|_| "Hook 配置序列化失败".to_string())?;
    db.set_setting(SETTINGS_KEY, &raw)
        .map_err(|e| e.to_string())
}

fn validate_config(config: &HookConfig) -> Result<(), String> {
    if !config.enabled {
        return Ok(());
    }
    let url = url::Url::parse(&config.endpoint).map_err(|_| "Hook 地址无效".to_string())?;
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return Err("Hook 只允许不含用户信息的回环 HTTP 地址".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Hook 地址不能包含 query 或 fragment".to_string());
    }
    let is_loopback = url
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if !is_loopback {
        return Err("Hook 地址必须是回环地址".to_string());
    }
    if config.bearer_token.trim().len() < 16 {
        return Err("Hook Bearer Token 至少需要 16 个字符".to_string());
    }
    if !(50..=10_000).contains(&config.timeout_ms) {
        return Err("Hook 超时必须在 50-10000ms 之间".to_string());
    }
    if !(1024..=1024 * 1024).contains(&config.max_payload_bytes) {
        return Err("Hook payload 上限必须在 1KiB-1MiB 之间".to_string());
    }
    Ok(())
}

fn normalized_sensitive_strings(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| value.len() >= 3)
        .filter(|value| seen.insert((*value).to_string()))
        .take(128)
        .map(str::to_string)
        .collect()
}

/// Remove credentials and user-configured sensitive literals before data is
/// sent to a hook or persisted as an interaction.
pub fn redact_value(value: &Value, sensitive_strings: &[String]) -> Value {
    fn walk(value: &Value, sensitive_strings: &[String]) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let lower = key.to_ascii_lowercase().replace(['-', '_'], "");
                        let redacted = matches!(
                            lower.as_str(),
                            "authorization"
                                | "proxyauthorization"
                                | "apikey"
                                | "xapikey"
                                | "cookie"
                                | "setcookie"
                                | "accesstoken"
                                | "refreshtoken"
                        );
                        (
                            key.clone(),
                            if redacted {
                                Value::String("[REDACTED]".to_string())
                            } else {
                                walk(value, sensitive_strings)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| walk(value, sensitive_strings))
                    .collect(),
            ),
            Value::String(text) => {
                let mut redacted = text.clone();
                for sensitive in sensitive_strings {
                    if !sensitive.is_empty() {
                        redacted = redacted.replace(sensitive, "[REDACTED]");
                    }
                }
                Value::String(redacted)
            }
            _ => value.clone(),
        }
    }
    walk(value, sensitive_strings)
}

pub async fn invoke(db: Arc<Database>, input: HookInput<'_>) -> Result<HookDecision, HookFailure> {
    let config = get_config(db.as_ref()).map_err(|_| HookFailure {
        fail_closed: false,
        message: "hook_config_unavailable",
    })?;
    if !config.enabled {
        return Ok(HookDecision::Allow);
    }
    let mut sensitive_strings = config.sensitive_strings.clone();
    sensitive_strings.push(config.bearer_token.clone());
    let request_replace_allowed =
        config.allow_request_replace || !matches!(input.phase, HookPhase::BeforeRequest);

    let payload = HookInput {
        payload: redact_value(&input.payload, &sensitive_strings),
        ..input
    };
    let encoded = serde_json::to_vec(&payload).map_err(|_| HookFailure {
        fail_closed: config.fail_closed,
        message: "hook_payload_invalid",
    })?;
    if encoded.len() > config.max_payload_bytes {
        return Err(HookFailure {
            fail_closed: config.fail_closed,
            message: "hook_payload_too_large",
        });
    }

    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_millis(config.timeout_ms))
        .no_proxy()
        .build()
        .map_err(|_| HookFailure {
            fail_closed: config.fail_closed,
            message: "hook_client_unavailable",
        })?;
    let mut response = client
        .post(&config.endpoint)
        .bearer_auth(&config.bearer_token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(encoded)
        .send()
        .await
        .map_err(|_| HookFailure {
            fail_closed: config.fail_closed,
            message: "hook_unavailable",
        })?;
    if !response.status().is_success() {
        return Err(HookFailure {
            fail_closed: config.fail_closed,
            message: "hook_rejected_request",
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > config.max_payload_bytes as u64)
    {
        return Err(HookFailure {
            fail_closed: config.fail_closed,
            message: "hook_response_too_large",
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| HookFailure {
        fail_closed: config.fail_closed,
        message: "hook_response_unreadable",
    })? {
        if bytes.len().saturating_add(chunk.len()) > config.max_payload_bytes {
            return Err(HookFailure {
                fail_closed: config.fail_closed,
                message: "hook_response_too_large",
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    let wire: WireDecision = serde_json::from_slice(&bytes).map_err(|_| HookFailure {
        fail_closed: config.fail_closed,
        message: "hook_response_invalid",
    })?;
    let result = match wire {
        WireDecision::Allow => HookDecision::Allow,
        WireDecision::Block { reason, rule_id } => HookDecision::Block {
            reason: sanitize_hook_label(
                reason.as_deref().unwrap_or("blocked_by_hook"),
                &sensitive_strings,
            ),
            rule_id: rule_id.map(|value| sanitize_hook_label(&value, &sensitive_strings)),
        },
        WireDecision::Replace { payload, rule_id } => {
            if !request_replace_allowed {
                return Err(HookFailure {
                    fail_closed: config.fail_closed,
                    message: "hook_request_replace_disabled",
                });
            }
            if matches!(payload, Value::Null) {
                return Err(HookFailure {
                    fail_closed: config.fail_closed,
                    message: "hook_replacement_invalid",
                });
            }
            if contains_credential_fields(&payload) {
                return Err(HookFailure {
                    fail_closed: config.fail_closed,
                    message: "hook_replacement_contains_credentials",
                });
            }
            HookDecision::Replace {
                payload,
                rule_id: rule_id.map(|value| sanitize_hook_label(&value, &sensitive_strings)),
            }
        }
        WireDecision::Audit { event, rule_id } => HookDecision::Audit {
            event: sanitize_hook_label(&event, &sensitive_strings),
            rule_id: rule_id.map(|value| sanitize_hook_label(&value, &sensitive_strings)),
        },
    };
    Ok(result)
}

fn contains_credential_fields(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase().replace(['-', '_'], "");
            matches!(
                key.as_str(),
                "authorization"
                    | "proxyauthorization"
                    | "apikey"
                    | "xapikey"
                    | "accesstoken"
                    | "refreshtoken"
                    | "cookie"
                    | "setcookie"
                    | "controltoken"
            ) || contains_credential_fields(value)
        }),
        Value::Array(values) => values.iter().any(contains_credential_fields),
        _ => false,
    }
}

pub fn hook_error(failure: HookFailure) -> Result<HookDecision, crate::proxy::ProxyError> {
    log::warn!("[Hook] {}", failure.message);
    if failure.fail_closed {
        Err(crate::proxy::ProxyError::InvalidRequest(
            "Request blocked because the local policy hook was unavailable".to_string(),
        ))
    } else {
        Ok(HookDecision::Allow)
    }
}

pub fn resolve_failure(
    db: &Database,
    request_id: &str,
    phase: &str,
    failure: HookFailure,
) -> Result<HookDecision, crate::proxy::ProxyError> {
    record_event(
        db,
        request_id,
        phase,
        if failure.fail_closed {
            "error_block"
        } else {
            "error_allow"
        },
        None,
        Some(failure.message),
    );
    hook_error(failure)
}

pub fn record_event(
    db: &Database,
    request_id: &str,
    phase: &str,
    action: &str,
    rule_id: Option<&str>,
    detail: Option<&str>,
) {
    let event = json!({
        "phase": phase,
        "action": action,
        "ruleId": rule_id,
        "detail": detail.map(sanitize_label),
        "createdAt": chrono::Utc::now().timestamp_millis(),
    });
    if let Err(error) = db.append_proxy_hook_event(request_id, &event) {
        log::warn!("[Hook] audit persistence failed: {error}");
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_response_hooks(
    db: Arc<Database>,
    request_id: &str,
    app: &str,
    client_model: &str,
    outbound_model: Option<&str>,
    provider_id: &str,
    mut payload: Value,
) -> Result<Value, crate::proxy::ProxyError> {
    let assembled_stream_tools = inspect_anthropic_stream_tool_calls(
        db.clone(),
        request_id,
        app,
        client_model,
        outbound_model,
        provider_id,
        &mut payload,
    )
    .await?;
    let assembled_chat_tools = inspect_openai_chat_stream_tool_calls(
        db.clone(),
        request_id,
        app,
        client_model,
        outbound_model,
        provider_id,
        &mut payload,
    )
    .await?;
    if !assembled_stream_tools && !assembled_chat_tools {
        inspect_tool_calls(
            db.clone(),
            request_id,
            app,
            client_model,
            outbound_model,
            provider_id,
            &mut payload,
        )
        .await?;
    }

    let input = HookInput {
        request_id,
        phase: HookPhase::AfterResponse,
        app,
        client_model,
        outbound_model,
        provider_id,
        payload: payload.clone(),
    };
    match invoke(db.clone(), input)
        .await
        .or_else(|failure| resolve_failure(db.as_ref(), request_id, "after_response", failure))?
    {
        HookDecision::Allow => Ok(payload),
        HookDecision::Block { reason, rule_id } => {
            record_event(
                db.as_ref(),
                request_id,
                "after_response",
                "block",
                rule_id.as_deref(),
                Some(&reason),
            );
            Err(crate::proxy::ProxyError::InvalidRequest(format!(
                "Response blocked by local policy: {reason}"
            )))
        }
        HookDecision::Replace { payload, rule_id } => {
            record_event(
                db.as_ref(),
                request_id,
                "after_response",
                "replace",
                rule_id.as_deref(),
                None,
            );
            Ok(payload)
        }
        HookDecision::Audit { event, rule_id } => {
            record_event(
                db.as_ref(),
                request_id,
                "after_response",
                "audit",
                rule_id.as_deref(),
                Some(&event),
            );
            Ok(payload)
        }
    }
}

#[derive(Default)]
struct ChatToolAssembly {
    positions: Vec<(usize, usize, usize)>,
    id: String,
    name: String,
    arguments: String,
}

#[allow(clippy::too_many_arguments)]
async fn inspect_openai_chat_stream_tool_calls(
    db: Arc<Database>,
    request_id: &str,
    app: &str,
    client_model: &str,
    outbound_model: Option<&str>,
    provider_id: &str,
    payload: &mut Value,
) -> Result<bool, crate::proxy::ProxyError> {
    let Some(events) = payload.as_array_mut() else {
        return Ok(false);
    };
    let mut assemblies = std::collections::BTreeMap::<(i64, i64), ChatToolAssembly>::new();
    let mut completed = false;
    for (event_position, event) in events.iter().enumerate() {
        let Some(choices) = event.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for (choice_position, choice) in choices.iter().enumerate() {
            let choice_index = choice
                .get("index")
                .and_then(Value::as_i64)
                .unwrap_or(choice_position as i64);
            if matches!(
                choice.get("finish_reason").and_then(Value::as_str),
                Some("tool_calls" | "function_call")
            ) {
                completed = true;
            }
            let Some(calls) = choice
                .pointer("/delta/tool_calls")
                .and_then(Value::as_array)
            else {
                continue;
            };
            for (call_position, call) in calls.iter().enumerate() {
                let call_index = call
                    .get("index")
                    .and_then(Value::as_i64)
                    .unwrap_or(call_position as i64);
                let assembly = assemblies.entry((choice_index, call_index)).or_default();
                assembly
                    .positions
                    .push((event_position, choice_position, call_position));
                if let Some(id) = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    assembly.id = id.to_string();
                }
                if let Some(name) = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                {
                    assembly.name.push_str(name);
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    assembly.arguments.push_str(arguments);
                }
            }
        }
    }
    if !completed || assemblies.is_empty() {
        return Ok(false);
    }

    for ((_choice_index, _call_index), assembly) in assemblies {
        let arguments = serde_json::from_str::<Value>(&assembly.arguments)
            .unwrap_or_else(|_| Value::String(assembly.arguments.clone()));
        let mut tool = json!({
            "type": "function_call",
            "id": assembly.id,
            "name": assembly.name,
            "arguments": arguments,
        });
        let original = tool.clone();
        inspect_tool_calls(
            db.clone(),
            request_id,
            app,
            client_model,
            outbound_model,
            provider_id,
            &mut tool,
        )
        .await?;
        if tool == original || assembly.positions.is_empty() {
            continue;
        }
        let replacement_name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let replacement_arguments = match tool.get("arguments") {
            Some(Value::String(value)) => value.clone(),
            Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
            None => "{}".to_string(),
        };
        for (offset, (event_position, choice_position, call_position)) in
            assembly.positions.iter().enumerate()
        {
            let Some(call) = events
                .get_mut(*event_position)
                .and_then(|event| event.get_mut("choices"))
                .and_then(Value::as_array_mut)
                .and_then(|choices| choices.get_mut(*choice_position))
                .and_then(|choice| choice.pointer_mut("/delta/tool_calls"))
                .and_then(Value::as_array_mut)
                .and_then(|calls| calls.get_mut(*call_position))
            else {
                continue;
            };
            call["function"]["name"] = Value::String(if offset == 0 {
                replacement_name.clone()
            } else {
                String::new()
            });
            call["function"]["arguments"] = Value::String(if offset == 0 {
                replacement_arguments.clone()
            } else {
                String::new()
            });
        }
    }
    Ok(true)
}

#[derive(Clone)]
struct AnthropicToolAssembly {
    index: i64,
    start_position: usize,
    delta_positions: Vec<usize>,
    tool: Value,
    arguments: String,
}

#[allow(clippy::too_many_arguments)]
async fn inspect_anthropic_stream_tool_calls(
    db: Arc<Database>,
    request_id: &str,
    app: &str,
    client_model: &str,
    outbound_model: Option<&str>,
    provider_id: &str,
    payload: &mut Value,
) -> Result<bool, crate::proxy::ProxyError> {
    let Some(events) = payload.as_array_mut() else {
        return Ok(false);
    };
    let mut active = std::collections::HashMap::<i64, AnthropicToolAssembly>::new();
    let mut complete = Vec::new();
    for (position, event) in events.iter().enumerate() {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index = event.get("index").and_then(Value::as_i64).unwrap_or(-1);
        match kind {
            "content_block_start"
                if event.pointer("/content_block/type").and_then(Value::as_str)
                    == Some("tool_use") =>
            {
                active.insert(
                    index,
                    AnthropicToolAssembly {
                        index,
                        start_position: position,
                        delta_positions: Vec::new(),
                        tool: event["content_block"].clone(),
                        arguments: String::new(),
                    },
                );
            }
            "content_block_delta"
                if event.pointer("/delta/type").and_then(Value::as_str)
                    == Some("input_json_delta") =>
            {
                if let Some(assembly) = active.get_mut(&index) {
                    assembly.delta_positions.push(position);
                    if let Some(fragment) =
                        event.pointer("/delta/partial_json").and_then(Value::as_str)
                    {
                        assembly.arguments.push_str(fragment);
                    }
                }
            }
            "content_block_stop" => {
                if let Some(mut assembly) = active.remove(&index) {
                    if !assembly.arguments.is_empty() {
                        if let Ok(input) = serde_json::from_str::<Value>(&assembly.arguments) {
                            assembly.tool["input"] = input;
                        }
                    }
                    complete.push(assembly);
                }
            }
            _ => {}
        }
    }
    let had_complete = !complete.is_empty();
    for mut assembly in complete {
        let original = assembly.tool.clone();
        inspect_tool_calls(
            db.clone(),
            request_id,
            app,
            client_model,
            outbound_model,
            provider_id,
            &mut assembly.tool,
        )
        .await?;
        if assembly.tool == original {
            continue;
        }
        if let Some(start) = events.get_mut(assembly.start_position) {
            start["content_block"] = assembly.tool.clone();
            // Keep the streaming shape valid: emit replacement arguments in
            // the first input-json delta and blank the remaining fragments.
            if let Some(input) = assembly.tool.get("input") {
                start["content_block"]["input"] = json!({});
                let replacement = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
                for (offset, position) in assembly.delta_positions.iter().enumerate() {
                    if let Some(delta) = events.get_mut(*position) {
                        delta["delta"]["partial_json"] = if offset == 0 {
                            Value::String(replacement.clone())
                        } else {
                            Value::String(String::new())
                        };
                    }
                }
            }
        }
        log::info!(
            "[Hook] replaced assembled tool call request_id={request_id} index={}",
            assembly.index
        );
    }
    Ok(had_complete)
}

fn inspect_tool_calls<'a>(
    db: Arc<Database>,
    request_id: &'a str,
    app: &'a str,
    client_model: &'a str,
    outbound_model: Option<&'a str>,
    provider_id: &'a str,
    value: &'a mut Value,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), crate::proxy::ProxyError>> + Send + 'a>,
> {
    Box::pin(async move {
        let is_tool_call = value.as_object().is_some_and(|object| {
            let kind = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            matches!(kind, "tool_use" | "function_call")
                || object.contains_key("tool_call")
                || (object.contains_key("name")
                    && (object.contains_key("arguments") || object.contains_key("input")))
        });
        if is_tool_call {
            let input = HookInput {
                request_id,
                phase: HookPhase::BeforeToolCall,
                app,
                client_model,
                outbound_model,
                provider_id,
                payload: value.clone(),
            };
            match invoke(db.clone(), input).await.or_else(|failure| {
                resolve_failure(db.as_ref(), request_id, "before_tool_call", failure)
            })? {
                HookDecision::Allow => {}
                HookDecision::Block { reason, rule_id } => {
                    record_event(
                        db.as_ref(),
                        request_id,
                        "before_tool_call",
                        "block",
                        rule_id.as_deref(),
                        Some(&reason),
                    );
                    return Err(crate::proxy::ProxyError::InvalidRequest(format!(
                        "Tool call blocked by local policy: {reason}"
                    )));
                }
                HookDecision::Replace { payload, rule_id } => {
                    record_event(
                        db.as_ref(),
                        request_id,
                        "before_tool_call",
                        "replace",
                        rule_id.as_deref(),
                        None,
                    );
                    *value = payload;
                    return Ok(());
                }
                HookDecision::Audit { event, rule_id } => {
                    record_event(
                        db.as_ref(),
                        request_id,
                        "before_tool_call",
                        "audit",
                        rule_id.as_deref(),
                        Some(&event),
                    );
                }
            }
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    inspect_tool_calls(
                        db.clone(),
                        request_id,
                        app,
                        client_model,
                        outbound_model,
                        provider_id,
                        value,
                    )
                    .await?;
                }
            }
            Value::Object(object) => {
                for value in object.values_mut() {
                    inspect_tool_calls(
                        db.clone(),
                        request_id,
                        app,
                        client_model,
                        outbound_model,
                        provider_id,
                        value,
                    )
                    .await?;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect::<String>()
}

fn sanitize_hook_label(value: &str, sensitive_strings: &[String]) -> String {
    let mut value = sanitize_label(value);
    for sensitive in sensitive_strings {
        if !sensitive.is_empty() {
            value = value.replace(sensitive, "[REDACTED]");
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    #[test]
    fn redacts_keys_and_custom_literals() {
        let redacted = redact_value(
            &json!({"authorization": "Bearer abc", "nested": {"api_key": "key"}, "text": "hello secret"}),
            &["secret".to_string()],
        );
        assert_eq!(redacted["authorization"], "[REDACTED]");
        assert_eq!(redacted["nested"]["api_key"], "[REDACTED]");
        assert_eq!(redacted["text"], "hello [REDACTED]");
    }

    #[test]
    fn rejects_non_loopback_hook() {
        let config = HookConfig {
            enabled: true,
            endpoint: "http://example.com/hook".to_string(),
            bearer_token: "0123456789abcdef".to_string(),
            ..HookConfig::default()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn credential_field_detection_is_recursive() {
        assert!(contains_credential_fields(&json!({
            "nested": [{"apiKey": "must-not-leave-hook"}]
        })));
        assert!(!contains_credential_fields(&json!({"message": "ordinary"})));
    }

    #[tokio::test]
    async fn loopback_hook_blocks_and_redacts_returned_reason() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(|| async {
                axum::Json(json!({"action":"block", "reason":"contains secret"}))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let db = Arc::new(Database::memory().unwrap());
        save_config(
            db.as_ref(),
            HookConfig {
                enabled: true,
                endpoint: format!("http://{address}/hook"),
                bearer_token: "0123456789abcdef".into(),
                sensitive_strings: vec!["secret".into()],
                ..HookConfig::default()
            },
        )
        .unwrap();
        let decision = invoke(
            db,
            HookInput {
                request_id: "r1",
                phase: HookPhase::BeforeRequest,
                app: "codex",
                client_model: "gpt",
                outbound_model: Some("gpt"),
                provider_id: "p1",
                payload: json!({"authorization":"Bearer hidden"}),
            },
        )
        .await
        .unwrap();
        match decision {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "contains [REDACTED]"),
            _ => panic!("expected block"),
        }
        server.abort();
    }
}
