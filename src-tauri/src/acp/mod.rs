pub mod autonomous_activity;
pub mod background_watch;
pub mod binary_cache;
pub mod bundled_agent;
pub mod chat_authoring;
pub mod codex_autonomous;
pub mod codex_catalog_source;
pub mod codex_cli;
pub mod codex_goal;
pub mod codex_model_catalog;
pub mod connection;
pub mod cursor_enrichment;
pub mod cursor_store;
pub mod custom_registry;
pub mod delegation;
#[cfg(feature = "tauri-runtime")]
pub mod desktop_event_batcher;
pub mod error;
pub mod event_stream;
pub mod feedback;
pub mod file_system_runtime;
pub mod fork;
pub mod grok_autonomous;
pub mod grok_retry;
pub mod host_tools_policy;
pub mod idle_sweep;
pub mod internal_bus;
pub mod lifecycle;
pub mod manager;
pub mod opencode_catalog;
pub mod opencode_plugins;
pub mod owner_rebind;
#[cfg(any(test, feature = "test-utils"))]
pub mod perf_fixture;
pub mod plan_approval;
pub mod preflight;
pub mod prompt_hydration;
pub mod question;
pub mod recovery_authorization;
pub mod registry;
pub mod remote_registry;
pub mod request_usage;
pub mod session_attach;
pub mod session_info;
pub mod session_state;
pub mod session_title;
pub mod shared_session;
pub mod stderr_tail;
pub mod streaming_performance;
pub mod terminal_adapter;
pub mod terminal_assoc;
pub mod terminal_context;
pub mod terminal_runtime;
pub mod termination;
pub mod tool_watchdog;
pub mod types;
pub mod work_task_tools;
pub mod xai_session_notification;

#[cfg(feature = "tauri-runtime")]
pub use desktop_event_batcher::{
    DesktopAcpDelivery, DesktopAcpEventBatch, DesktopConnectionSeqRange, DesktopDeliveryError,
    DesktopDeliveryFailure,
};
pub use idle_sweep::{idle_sweep_task, idle_timeout_from_env, SWEEP_INTERVAL_SECS};
pub use internal_bus::{
    EventBusMetrics, EventBusMetricsSnapshot, InternalEventBus, InternalEventEnvelope,
};
pub use lifecycle::lifecycle_subscriber_task;
pub use session_state::{LiveSessionSnapshot, SessionState};
pub use streaming_performance::{
    DesktopDeliveryCapabilities, DesktopDeliveryMode, StreamingPerformanceFlags,
};
// Re-export the inner types of LiveSessionSnapshot for downstream consumers; not all are
// directly named in Rust today (they ride along through the snapshot struct), so silence
// dead-import warnings rather than dropping them.
#[allow(unused_imports)]
pub use session_state::{
    LiveContentBlock, LiveMessage, PendingPermissionState, ToolCallOutput, ToolCallState,
    ToolCallStatus, ToolKind, UsageInfo,
};
pub use types::{
    user_blocks_from_prompt, AcpEvent, ConversationConnectionInfo, EventEnvelope, UserMessageBlock,
};

/// The session ids `session_id` carries forward — i.e. earlier sessions of the
/// SAME conversation, whose turns remain readable through `session_id`.
///
/// Feed this to [`crate::db::service::conversation_service::bind_external_id`]
/// so it can tell "this conversation continues under a new agent session" from
/// "an unrelated session landed on this row". The first is routine: when a
/// custom agent has forgotten a session, codeg opens a fresh one and links the
/// transcripts, and both the reader and the generic parser then treat the chain
/// as one conversation. Splitting there would clone the conversation in the
/// sidebar on every restart. The second is codeg#500, where the split is the
/// whole point.
///
/// Empty — and free — for every built-in agent: their history lives in the
/// agent's own store, codeg records no transcript, and so nothing can ever be
/// carried forward. Only custom agents can produce a non-empty answer.
pub fn continued_session_ids(
    agent_type: crate::models::AgentType,
    session_id: &str,
) -> Vec<String> {
    if agent_type.custom_id().is_none() {
        return Vec::new();
    }
    crate::acp_transcript::continuation_ancestors(registry::registry_id_for(agent_type), session_id)
}
