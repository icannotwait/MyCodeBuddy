pub mod background_watch;
pub mod binary_cache;
pub mod bundled_agent;
pub mod codex_cli;
pub mod codex_goal;
pub mod connection;
pub mod delegation;
#[cfg(feature = "tauri-runtime")]
pub mod desktop_event_batcher;
pub mod error;
pub mod event_stream;
pub mod feedback;
pub mod file_system_runtime;
pub mod fork;
pub mod idle_sweep;
pub mod internal_bus;
pub mod lifecycle;
pub mod manager;
pub mod opencode_catalog;
pub mod opencode_plugins;
#[cfg(any(test, feature = "test-utils"))]
pub mod perf_fixture;
pub mod preflight;
pub mod question;
pub mod registry;
pub mod session_info;
pub mod session_state;
pub mod streaming_performance;
pub mod terminal_adapter;
pub mod terminal_assoc;
pub mod terminal_context;
pub mod terminal_runtime;
pub mod types;

#[cfg(feature = "tauri-runtime")]
pub use desktop_event_batcher::{
    DesktopAcpDelivery, DesktopAcpEventBatch, DesktopConnectionSeqRange, DesktopDeliveryError,
    DesktopDeliveryFailure,
};
pub use idle_sweep::{idle_sweep_task, idle_timeout_from_env, SWEEP_INTERVAL_SECS};
pub use internal_bus::{EventBusMetrics, EventBusMetricsSnapshot, InternalEventBus};
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
