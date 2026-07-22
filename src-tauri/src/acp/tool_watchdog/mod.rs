//! Host-owned tool execution watchdog: contracts, progress fingerprints, lease registry.

pub mod progress;
pub mod registry;
pub mod types;

pub use progress::{apply_semantic_progress, ProgressFingerprint};
pub use registry::{
    fallback_eligible, CancelCause, CancellationClaim, RegisterTool, RegistryAction,
    SemanticProgress, StaleLease, ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressKey,
    TurnStamp, WatchdogInstant, FALLBACK_TOOL_CALL_ID,
};
pub use types::*;
