//! Host-owned tool execution watchdog: contracts, progress fingerprints, lease registry.

pub mod attribution;
pub mod progress;
pub mod registry;
pub mod types;

pub use attribution::{
    classify_tool_category, content_hash, status_fingerprint, tool_lease_key, tool_progress_key,
    turn_stamp, unambiguous_terminal_id, LeaseAttribution,
};
pub use progress::{apply_semantic_progress, ProgressFingerprint};
pub use registry::{
    fallback_eligible, CancelCause, CancellationClaim, RegisterTool, RegistryAction,
    SemanticProgress, StaleLease, ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressKey,
    TurnStamp, WatchdogInstant, FALLBACK_TOOL_CALL_ID,
};
pub use types::*;
