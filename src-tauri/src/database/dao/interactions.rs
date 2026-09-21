use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInteractionSummary {
    pub request_id: String,
    pub session_id: Option<String>,
    pub app_type: String,
    pub client_model: String,
    pub outbound_model: Option<String>,
    pub final_provider_id: Option<String>,
    pub status_code: Option<i64>,
    pub is_streaming: bool,
    pub hook_hit: bool,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub retention_until: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInteractionDetail {
    #[serde(flatten)]
    pub summary: ProxyInteractionSummary,
    pub request_payload_redacted: Option<String>,
    pub upstream_request_payload_redacted: Option<String>,
    pub response_payload_redacted: Option<String>,
    pub hook_events_json: String,
    pub redaction_applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInteractionAttempt {
    pub request_id: String,
    pub attempt_index: i64,
    pub provider_id: String,
    pub endpoint_origin: Option<String>,
    pub status_code: Option<i64>,
    pub error_code: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxyInteractionFilters {
    pub app_type: Option<String>,
    pub model: Option<String>,
    pub provider_id: Option<String>,
    pub status_code: Option<i64>,
    pub hook_hit: Option<bool>,
    pub request_id: Option<String>,
    pub created_after: Option<i64>,
    pub created_before: Option<i64>,
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn begin_proxy_interaction(
        &self,
        request_id: &str,
        session_id: &str,
        app_type: &str,
        client_model: &str,
        request_payload: Option<&str>,
        retention_until: i64,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR IGNORE INTO proxy_interactions
             (request_id, session_id, app_type, client_model,
              request_payload_redacted, created_at, retention_until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                request_id,
                session_id,
                app_type,
                client_model,
                request_payload,
                chrono::Utc::now().timestamp_millis(),
                retention_until
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn record_proxy_interaction_attempt(
        &self,
        attempt: &ProxyInteractionAttempt,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO proxy_interaction_attempts
             (request_id, attempt_index, provider_id, endpoint_origin, status_code,
              error_code, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                attempt.request_id,
                attempt.attempt_index,
                attempt.provider_id,
                attempt.endpoint_origin,
                attempt.status_code,
                attempt.error_code,
                attempt.started_at,
                attempt.completed_at
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_proxy_interaction_upstream_request(
        &self,
        request_id: &str,
        payload: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE proxy_interactions SET upstream_request_payload_redacted=?2 WHERE request_id=?1",
            params![request_id, payload],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_proxy_interaction(
        &self,
        request_id: &str,
        outbound_model: Option<&str>,
        provider_id: &str,
        status_code: u16,
        is_streaming: bool,
        upstream_request: Option<&str>,
        response_payload: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE proxy_interactions SET outbound_model=?2, final_provider_id=?3,
             status_code=?4, is_streaming=?5, upstream_request_payload_redacted=COALESCE(?6, upstream_request_payload_redacted),
             response_payload_redacted=COALESCE(?7, response_payload_redacted), completed_at=?8
             WHERE request_id=?1",
            params![
                request_id,
                outbound_model,
                provider_id,
                i64::from(status_code),
                is_streaming,
                upstream_request,
                response_payload,
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn append_proxy_hook_event(
        &self,
        request_id: &str,
        event: &serde_json::Value,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let existing: Option<String> = conn
            .query_row(
                "SELECT hook_events_json FROM proxy_interactions WHERE request_id=?1",
                params![request_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let Some(existing) = existing else {
            return Ok(());
        };
        let mut events: Vec<serde_json::Value> =
            serde_json::from_str(&existing).unwrap_or_default();
        events.push(event.clone());
        let serialized =
            serde_json::to_string(&events).map_err(|e| AppError::Config(e.to_string()))?;
        conn.execute(
            "UPDATE proxy_interactions SET hook_hit=1, hook_events_json=?2 WHERE request_id=?1",
            params![request_id, serialized],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn list_proxy_interactions(
        &self,
        filters: &ProxyInteractionFilters,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<ProxyInteractionSummary>, AppError> {
        let conn = lock_conn!(self.conn);
        let page_size = page_size.clamp(1, 100);
        let offset = page.saturating_mul(page_size);
        let mut stmt = conn
            .prepare(
                "SELECT request_id, session_id, app_type, client_model, outbound_model,
             final_provider_id, status_code, is_streaming, hook_hit, created_at,
             completed_at, retention_until FROM proxy_interactions
             WHERE (?1 IS NULL OR app_type=?1)
               AND (?2 IS NULL OR client_model=?2 OR outbound_model=?2)
               AND (?3 IS NULL OR final_provider_id=?3)
               AND (?4 IS NULL OR status_code=?4)
               AND (?5 IS NULL OR hook_hit=?5)
               AND (?6 IS NULL OR request_id=?6)
               AND (?7 IS NULL OR created_at>=?7)
               AND (?8 IS NULL OR created_at<=?8)
             ORDER BY created_at DESC LIMIT ?9 OFFSET ?10",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    filters.app_type,
                    filters.model,
                    filters.provider_id,
                    filters.status_code,
                    filters.hook_hit.map(i64::from),
                    filters.request_id,
                    filters.created_after,
                    filters.created_before,
                    i64::from(page_size),
                    i64::from(offset)
                ],
                |row| {
                    Ok(ProxyInteractionSummary {
                        request_id: row.get(0)?,
                        session_id: row.get(1)?,
                        app_type: row.get(2)?,
                        client_model: row.get(3)?,
                        outbound_model: row.get(4)?,
                        final_provider_id: row.get(5)?,
                        status_code: row.get(6)?,
                        is_streaming: row.get::<_, i64>(7)? != 0,
                        hook_hit: row.get::<_, i64>(8)? != 0,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                        retention_until: row.get(11)?,
                    })
                },
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn get_proxy_interaction(
        &self,
        request_id: &str,
    ) -> Result<Option<ProxyInteractionDetail>, AppError> {
        let conn = lock_conn!(self.conn);
        conn.query_row(
            "SELECT request_id, session_id, app_type, client_model, outbound_model,
             final_provider_id, status_code, is_streaming, hook_hit, created_at, completed_at,
             retention_until, request_payload_redacted, upstream_request_payload_redacted,
             response_payload_redacted, hook_events_json, redaction_applied
             FROM proxy_interactions WHERE request_id=?1",
            params![request_id],
            |row| {
                Ok(ProxyInteractionDetail {
                    summary: ProxyInteractionSummary {
                        request_id: row.get(0)?,
                        session_id: row.get(1)?,
                        app_type: row.get(2)?,
                        client_model: row.get(3)?,
                        outbound_model: row.get(4)?,
                        final_provider_id: row.get(5)?,
                        status_code: row.get(6)?,
                        is_streaming: row.get::<_, i64>(7)? != 0,
                        hook_hit: row.get::<_, i64>(8)? != 0,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                        retention_until: row.get(11)?,
                    },
                    request_payload_redacted: row.get(12)?,
                    upstream_request_payload_redacted: row.get(13)?,
                    response_payload_redacted: row.get(14)?,
                    hook_events_json: row.get(15)?,
                    redaction_applied: row.get::<_, i64>(16)? != 0,
                })
            },
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn list_proxy_interaction_attempts(
        &self,
        request_id: &str,
    ) -> Result<Vec<ProxyInteractionAttempt>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT request_id, attempt_index, provider_id, endpoint_origin, status_code,
             error_code, started_at, completed_at FROM proxy_interaction_attempts
             WHERE request_id=?1 ORDER BY attempt_index",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![request_id], |row| {
                Ok(ProxyInteractionAttempt {
                    request_id: row.get(0)?,
                    attempt_index: row.get(1)?,
                    provider_id: row.get(2)?,
                    endpoint_origin: row.get(3)?,
                    status_code: row.get(4)?,
                    error_code: row.get(5)?,
                    started_at: row.get(6)?,
                    completed_at: row.get(7)?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))
    }

    pub fn prune_proxy_interactions(&self, quota_bytes: u64) -> Result<usize, AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp_millis();
        let mut deleted = conn
            .execute(
                "DELETE FROM proxy_interactions WHERE retention_until < ?1",
                params![now],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut estimated: i64 = conn.query_row(
            "SELECT COALESCE(SUM(512 + LENGTH(request_id) + LENGTH(app_type) + LENGTH(client_model)
             + COALESCE(LENGTH(session_id),0) + COALESCE(LENGTH(outbound_model),0)
             + COALESCE(LENGTH(final_provider_id),0) + COALESCE(LENGTH(request_payload_redacted),0)
             + COALESCE(LENGTH(upstream_request_payload_redacted),0)
             + COALESCE(LENGTH(response_payload_redacted),0) + LENGTH(hook_events_json)
             + 256 * (SELECT COUNT(*) FROM proxy_interaction_attempts a WHERE a.request_id=proxy_interactions.request_id)),0)
             FROM proxy_interactions",
            [], |row| row.get(0)
        ).map_err(|e| AppError::Database(e.to_string()))?;
        while estimated > quota_bytes as i64 {
            let changed = conn.execute(
                "DELETE FROM proxy_interactions WHERE request_id IN (SELECT request_id FROM proxy_interactions ORDER BY created_at ASC LIMIT 100)", []
            ).map_err(|e| AppError::Database(e.to_string()))?;
            if changed == 0 {
                break;
            }
            deleted += changed;
            estimated = conn.query_row(
                "SELECT COALESCE(SUM(512 + LENGTH(request_id) + LENGTH(app_type) + LENGTH(client_model)
                 + COALESCE(LENGTH(session_id),0) + COALESCE(LENGTH(outbound_model),0)
                 + COALESCE(LENGTH(final_provider_id),0) + COALESCE(LENGTH(request_payload_redacted),0)
                 + COALESCE(LENGTH(upstream_request_payload_redacted),0)
                 + COALESCE(LENGTH(response_payload_redacted),0) + LENGTH(hook_events_json)
                 + 256 * (SELECT COUNT(*) FROM proxy_interaction_attempts a WHERE a.request_id=proxy_interactions.request_id)),0)
                 FROM proxy_interactions",
                [], |row| row.get(0)
            ).map_err(|e| AppError::Database(e.to_string()))?;
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_body_and_attempts_are_independent_from_usage_logs() {
        let db = Database::memory().unwrap();
        db.begin_proxy_interaction(
            "req-1",
            "session-1",
            "codex",
            "gpt-client",
            None,
            chrono::Utc::now().timestamp_millis() + 60_000,
        )
        .unwrap();
        db.record_proxy_interaction_attempt(&ProxyInteractionAttempt {
            request_id: "req-1".into(),
            attempt_index: 1,
            provider_id: "a".into(),
            endpoint_origin: Some("https://api.example.com".into()),
            status_code: Some(502),
            error_code: Some("upstream_error".into()),
            started_at: 1,
            completed_at: Some(2),
        })
        .unwrap();
        db.complete_proxy_interaction(
            "req-1",
            Some("gpt-upstream"),
            "b",
            200,
            false,
            None,
            Some(r#"{"output":"[REDACTED]"}"#),
        )
        .unwrap();

        let detail = db.get_proxy_interaction("req-1").unwrap().unwrap();
        assert!(detail.request_payload_redacted.is_none());
        assert_eq!(detail.summary.final_provider_id.as_deref(), Some("b"));
        assert_eq!(
            detail.summary.outbound_model.as_deref(),
            Some("gpt-upstream")
        );
        assert_eq!(
            db.list_proxy_interaction_attempts("req-1").unwrap().len(),
            1
        );
    }

    #[test]
    fn expired_interactions_are_pruned_with_attempts() {
        let db = Database::memory().unwrap();
        db.begin_proxy_interaction("expired", "s", "claude", "m", None, 0)
            .unwrap();
        db.record_proxy_interaction_attempt(&ProxyInteractionAttempt {
            request_id: "expired".into(),
            attempt_index: 1,
            provider_id: "p".into(),
            endpoint_origin: None,
            status_code: None,
            error_code: None,
            started_at: 1,
            completed_at: None,
        })
        .unwrap();
        assert_eq!(db.prune_proxy_interactions(1024).unwrap(), 1);
        assert!(db.get_proxy_interaction("expired").unwrap().is_none());
        assert!(db
            .list_proxy_interaction_attempts("expired")
            .unwrap()
            .is_empty());
    }
}
