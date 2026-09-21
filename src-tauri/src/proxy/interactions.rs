//! Best-effort proxy interaction recording.
//!
//! Metadata is stored independently from usage accounting. Full bodies remain
//! off until the user explicitly enables a scoped recording policy.

use crate::{database::Database, provider::Provider};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

const SETTINGS_KEY: &str = "proxy_interaction_recording_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InteractionRecordingConfig {
    #[serde(default)]
    pub record_bodies: bool,
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default = "default_body_limit")]
    pub max_body_bytes: usize,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_quota_mb")]
    pub quota_mb: u32,
    #[serde(default)]
    pub record_raw_sse: bool,
}

fn default_body_limit() -> usize {
    256 * 1024
}
fn default_retention_days() -> u32 {
    7
}
fn default_quota_mb() -> u32 {
    100
}

impl Default for InteractionRecordingConfig {
    fn default() -> Self {
        Self {
            record_bodies: false,
            apps: Vec::new(),
            models: Vec::new(),
            providers: Vec::new(),
            max_body_bytes: default_body_limit(),
            retention_days: default_retention_days(),
            quota_mb: default_quota_mb(),
            record_raw_sse: false,
        }
    }
}

pub fn get_config(db: &Database) -> Result<InteractionRecordingConfig, String> {
    let Some(raw) = db.get_setting(SETTINGS_KEY).map_err(|e| e.to_string())? else {
        return Ok(InteractionRecordingConfig::default());
    };
    serde_json::from_str(&raw).map_err(|_| "交互记录配置损坏".to_string())
}

pub fn save_config(db: &Database, config: InteractionRecordingConfig) -> Result<(), String> {
    if !(1024..=4 * 1024 * 1024).contains(&config.max_body_bytes)
        || !(1..=365).contains(&config.retention_days)
        || !(10..=10_240).contains(&config.quota_mb)
    {
        return Err("交互记录限制超出允许范围".to_string());
    }
    let raw = serde_json::to_string(&config).map_err(|_| "交互记录配置序列化失败".to_string())?;
    db.set_setting(SETTINGS_KEY, &raw)
        .map_err(|e| e.to_string())?;
    let _ = db.prune_proxy_interactions(u64::from(config.quota_mb) * 1024 * 1024);
    Ok(())
}

pub fn should_record_body(
    config: &InteractionRecordingConfig,
    app: &str,
    model: &str,
    provider_id: &str,
) -> bool {
    config.record_bodies
        && (config.apps.is_empty() || config.apps.iter().any(|value| value == app))
        && (config.models.is_empty()
            || config
                .models
                .iter()
                .any(|value| wildcard_match(value, model)))
        && (config.providers.is_empty()
            || config.providers.iter().any(|value| value == provider_id))
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return pattern == value;
    };
    value.starts_with(prefix)
        && value.ends_with(suffix)
        && value.len() >= prefix.len() + suffix.len()
}

pub fn redacted_json_for_storage(db: &Database, value: &Value, max_bytes: usize) -> Option<String> {
    let sensitive = super::hooks::get_config(db)
        .map(|config| config.sensitive_strings)
        .unwrap_or_default();
    let redacted = super::hooks::redact_value(value, &sensitive);
    let bytes = serde_json::to_vec(&redacted).ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub fn begin(
    db: Arc<Database>,
    request_id: &str,
    session_id: &str,
    app: &str,
    model: &str,
    provider: &Provider,
    payload: &Value,
) {
    let Ok(config) = get_config(db.as_ref()) else {
        return;
    };
    let body = should_record_body(&config, app, model, &provider.id)
        .then(|| redacted_json_for_storage(db.as_ref(), payload, config.max_body_bytes))
        .flatten();
    let retention_until = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::days(i64::from(config.retention_days)))
        .unwrap_or_else(chrono::Utc::now)
        .timestamp_millis();
    if let Err(error) = db.begin_proxy_interaction(
        request_id,
        session_id,
        app,
        model,
        body.as_deref(),
        retention_until,
    ) {
        log::warn!("[Interactions] metadata write failed: {error}");
    }
}

pub fn complete(
    db: Arc<Database>,
    request_id: &str,
    app: &str,
    model: &str,
    outbound_model: Option<&str>,
    provider: &Provider,
    status: u16,
    is_streaming: bool,
    upstream_request: Option<&Value>,
    response: Option<&Value>,
) {
    let Ok(config) = get_config(db.as_ref()) else {
        return;
    };
    let record = should_record_body(&config, app, model, &provider.id);
    let upstream = record
        .then(|| {
            upstream_request.and_then(|value| {
                redacted_json_for_storage(db.as_ref(), value, config.max_body_bytes)
            })
        })
        .flatten();
    let response = record
        .then(|| {
            response.and_then(|value| {
                redacted_json_for_storage(db.as_ref(), value, config.max_body_bytes)
            })
        })
        .flatten();
    if let Err(error) = db.complete_proxy_interaction(
        request_id,
        outbound_model,
        &provider.id,
        status,
        is_streaming,
        upstream.as_deref(),
        response.as_deref(),
    ) {
        log::warn!("[Interactions] completion write failed: {error}");
    }
    let _ = db.prune_proxy_interactions(u64::from(config.quota_mb) * 1024 * 1024);
}

pub fn record_upstream_request(
    db: &Database,
    request_id: &str,
    app: &str,
    model: &str,
    provider_id: &str,
    payload: &Value,
) {
    let Ok(config) = get_config(db) else { return };
    if !should_record_body(&config, app, model, provider_id) {
        return;
    }
    let Some(payload) = redacted_json_for_storage(db, payload, config.max_body_bytes) else {
        return;
    };
    if let Err(error) = db.set_proxy_interaction_upstream_request(request_id, &payload) {
        log::warn!("[Interactions] upstream request write failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_recording_is_off_by_default_and_scoped() {
        let config = InteractionRecordingConfig::default();
        assert!(!should_record_body(&config, "codex", "gpt-5", "p1"));
        let config = InteractionRecordingConfig {
            record_bodies: true,
            apps: vec!["codex".into()],
            models: vec!["gpt-*".into()],
            providers: vec!["p1".into()],
            ..config
        };
        assert!(should_record_body(&config, "codex", "gpt-5", "p1"));
        assert!(!should_record_body(&config, "claude", "gpt-5", "p1"));
    }
}
