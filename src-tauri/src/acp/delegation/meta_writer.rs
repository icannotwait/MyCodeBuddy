//! `DelegationMetaWriter` — broker capability that attaches the live
//! delegation state onto the parent's active `delegate_to_agent`
//! tool-call. The shape written under `meta["codeg.delegation"]`
//! follows the convention documented at
//! [`crate::acp::session_state::ToolCallState::meta`].
//!
//! The broker calls this at three lifecycle points (always **after** the
//! durable store CAS in `settle_task` has elected a winner — losers must not
//! write a second terminal meta):
//!
//! 1. After accepted start publication — sets `status: "running"` with the
//!    child's connection / conversation ids, authoritative timestamps, and
//!    optional open attention / runtime snapshot.
//! 2. On durable terminal win (completion) — sets `status: "completed"` (ok)
//!    or `status: "failed"` + `error_code` (err) with final runtime stats.
//! 3. On durable terminal win (cancel) — sets `status: "failed"` +
//!    `error_code: "canceled"`.
//!
//! Writes are skipped when the broker is operating on a synthetic
//! `parent_tool_use_id` (the `"delegation-*"` UUID fallback) because
//! there's no matching ACP `tool_call_id` to attach meta to. The
//! frontend's snapshot path will still recover via `parseInput(input)`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::Arc;

use crate::acp::delegation::attention::AttentionRequestSummary;
use crate::acp::delegation::runtime_stats::DelegationRuntimeStats;
use crate::acp::manager::ConnectionManager;
use crate::acp::types::AcpEvent;
use crate::web::event_bridge::emit_with_state;

/// Top-level key under which delegation state lives on a tool call's
/// `meta` object. Single source of truth — both the writer and the
/// frontend reader must spell it the same way.
pub const DELEGATION_META_KEY: &str = "codeg.delegation";

/// Canonical typed snapshot for `meta["codeg.delegation"]` — one shape drives
/// live broker writes and cold-load reconstruction.
#[derive(Debug, Clone, Serialize)]
pub struct DelegationMetaSnapshot {
    pub status: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_connection_id: Option<String>,
    pub child_conversation_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_preview: Option<String>,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// Optional only for pre-feature cold history. Every newly accepted live
    /// task supplies it (never fabricate zero counts for historical nulls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_stats: Option<DelegationRuntimeStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_request: Option<AttentionRequestSummary>,
}

/// Capability the broker uses to patch `meta["codeg.delegation"]` on
/// the parent connection's active `delegate_to_agent` tool call.
///
/// Errors are swallowed at the impl boundary: a missing parent
/// connection (e.g. user disconnected mid-delegation) or a stale
/// tool_use_id (e.g. parent turn already wrapped up) must not derail
/// the rest of the broker lifecycle, which still has to disconnect the
/// child and resolve the pending call.
#[async_trait]
pub trait DelegationMetaWriter: Send + Sync {
    async fn write_meta(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        meta: serde_json::Value,
    );
}

/// Default writer used when the broker is constructed via the
/// short-form `DelegationBroker::new` (most test callsites). Silently
/// drops every write — the broker's correctness is observable through
/// its outcomes and pending-call accounting, not through meta emits.
#[derive(Default, Clone)]
pub struct NoopMetaWriter;

#[async_trait]
impl DelegationMetaWriter for NoopMetaWriter {
    async fn write_meta(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _meta: serde_json::Value,
    ) {
    }
}

/// Production impl backed by `ConnectionManager`. Emits an
/// `AcpEvent::ToolCallUpdate` carrying only the `meta` field so the
/// existing `apply_tool_call_update` merge path (partial-update
/// preservation of locations / images / content / etc.) is reused
/// without duplicating the patch logic.
#[derive(Clone)]
pub struct ConnectionManagerMetaWriter {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait]
impl DelegationMetaWriter for ConnectionManagerMetaWriter {
    async fn write_meta(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        meta: serde_json::Value,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::ToolCallUpdate {
                tool_call_id: parent_tool_use_id.to_string(),
                title: None,
                status: None,
                content: None,
                raw_input: None,
                raw_output: None,
                raw_output_append: None,
                locations: None,
                meta: Some(meta),
                images: None,
            },
        )
        .await;
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use super::*;
    use tokio::sync::Mutex;

    /// Records every call so broker tests can assert the meta lifecycle
    /// (running → completed/failed) was driven correctly. No-op on the
    /// emit side — the broker is the unit under test, not the event
    /// fanout.
    #[derive(Default)]
    pub struct MockMetaWriter {
        pub calls: Mutex<Vec<MetaWriteCall>>,
    }

    #[derive(Debug, Clone)]
    pub struct MetaWriteCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub meta: serde_json::Value,
    }

    impl MockMetaWriter {
        pub fn new() -> Self {
            Self::default()
        }

        pub async fn snapshot(&self) -> Vec<MetaWriteCall> {
            self.calls.lock().await.clone()
        }
    }

    #[async_trait]
    impl DelegationMetaWriter for MockMetaWriter {
        async fn write_meta(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            meta: serde_json::Value,
        ) {
            self.calls.lock().await.push(MetaWriteCall {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                meta,
            });
        }
    }
}

/// Helper to construct the canonical `meta["codeg.delegation"]` value from
/// the typed snapshot. Keeps the schema in one place so the writer impls,
/// broker callsites, and cold-load reconstruction cannot drift.
pub fn build_delegation_meta(snapshot: &DelegationMetaSnapshot) -> serde_json::Value {
    let mut outer = serde_json::Map::new();
    outer.insert(
        DELEGATION_META_KEY.to_string(),
        serde_json::to_value(snapshot).expect("delegation meta is serializable"),
    );
    serde_json::Value::Object(outer)
}

/// True when the broker handed out a synthetic placeholder
/// `parent_tool_use_id` (no matching ACP tool_call_id exists). Skipping
/// meta writes for these avoids spamming `ToolCallUpdate` events with a
/// tool_call_id that no live `ToolCallState` will ever match.
pub fn is_synthetic_parent_tool_use_id(id: &str) -> bool {
    id.starts_with("delegation-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        status: &str,
        task_id: &str,
        child_connection_id: Option<&str>,
        child_conversation_id: i32,
        error_code: Option<&str>,
        runtime_stats: Option<DelegationRuntimeStats>,
    ) -> DelegationMetaSnapshot {
        DelegationMetaSnapshot {
            status: status.into(),
            task_id: task_id.into(),
            child_connection_id: child_connection_id.map(str::to_string),
            child_conversation_id,
            error_code: error_code.map(str::to_string),
            text_preview: None,
            started_at: DateTime::parse_from_rfc3339("2026-07-17T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            finished_at: None,
            runtime_stats,
            attention_request: None,
        }
    }

    #[test]
    fn build_meta_includes_provided_fields() {
        let stats = DelegationRuntimeStats::empty(
            DateTime::parse_from_rfc3339("2026-07-17T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let v = build_delegation_meta(&snap(
            "running",
            "task-1",
            Some("conn-1"),
            42,
            None,
            Some(stats),
        ));
        let inner = v.get(DELEGATION_META_KEY).unwrap().as_object().unwrap();
        assert_eq!(inner.get("status").unwrap().as_str().unwrap(), "running");
        assert_eq!(inner.get("task_id").unwrap().as_str().unwrap(), "task-1");
        assert_eq!(
            inner.get("child_connection_id").unwrap().as_str().unwrap(),
            "conn-1"
        );
        assert_eq!(
            inner
                .get("child_conversation_id")
                .unwrap()
                .as_i64()
                .unwrap(),
            42
        );
        assert!(inner.get("error_code").is_none());
        assert!(inner.get("runtime_stats").is_some());
        assert!(inner.get("duration_ms").is_none());
    }

    #[test]
    fn build_meta_with_error_code() {
        let v = build_delegation_meta(&snap(
            "failed",
            "task-7",
            None,
            7,
            Some("timeout"),
            None,
        ));
        let inner = v.get(DELEGATION_META_KEY).unwrap().as_object().unwrap();
        assert_eq!(inner.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            inner.get("error_code").unwrap().as_str().unwrap(),
            "timeout"
        );
        assert!(inner.get("child_connection_id").is_none());
        assert!(
            inner.get("runtime_stats").is_none(),
            "pre-feature / absent stats must omit the field"
        );
    }

    #[test]
    fn build_meta_serializes_finished_at_and_preview() {
        let mut snapshot = snap("completed", "task-1", Some("conn-1"), 42, None, None);
        snapshot.finished_at = Some(
            DateTime::parse_from_rfc3339("2026-07-17T10:05:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        snapshot.text_preview = Some("done".into());
        let v = build_delegation_meta(&snapshot);
        let inner = v.get(DELEGATION_META_KEY).unwrap().as_object().unwrap();
        assert!(inner.get("finished_at").unwrap().is_string());
        assert_eq!(inner.get("text_preview").unwrap().as_str().unwrap(), "done");
    }

    #[test]
    fn synthetic_id_detection() {
        assert!(is_synthetic_parent_tool_use_id(
            "delegation-3b4a5c6d-7e8f-90ab-cdef-1234567890ab"
        ));
        assert!(!is_synthetic_parent_tool_use_id("tu_real_acp_id"));
        assert!(!is_synthetic_parent_tool_use_id(""));
    }
}
