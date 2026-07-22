//! Host-owned tool execution watchdog: contracts, progress fingerprints, lease registry.

pub mod attribution;
pub mod mcp_cancel;
pub mod progress;
pub mod registry;
pub mod supervisor;
#[cfg(test)]
mod terminal_cancel_tests;
pub mod types;

pub use attribution::{
    classify_tool_category, content_hash, status_fingerprint, tool_lease_key, tool_progress_key,
    turn_stamp, unambiguous_terminal_id, LeaseAttribution,
};
pub use mcp_cancel::McpCancelRegistry;
pub use progress::{apply_semantic_progress, ProgressFingerprint};
pub use registry::{
    fallback_eligible, CancelCause, CancellationClaim, RegisterTool, RegistryAction,
    SemanticProgress, StaleLease, ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressKey,
    TurnStamp, WatchdogInstant, FALLBACK_TOOL_CALL_ID,
};
pub use supervisor::{
    escalate_claimed_lease, error_code_for_cause, scope_for_capability, wait_stamp_from_lease,
    CancelHost, ConvergenceProbe, EscalationReport, EscalationStage, RegistryProbe,
    SpecificCancelOutcome, TERMINAL_ACK_TIMEOUT, TERMINAL_ADMIT_TIMEOUT,
    TERMINAL_KILL_EXECUTOR_TIMEOUT,
};
pub use types::*;
