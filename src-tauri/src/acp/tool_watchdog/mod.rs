//! Host-owned tool execution watchdog: contracts, progress fingerprints, lease registry.

pub mod attribution;
pub mod mcp_cancel;
pub mod metrics;
pub mod progress;
pub mod registry;
pub mod supervisor;
pub mod types;

pub use attribution::{
    classify_tool_category, content_hash, status_fingerprint, tool_lease_key, tool_progress_key,
    turn_stamp, unambiguous_terminal_id, LeaseAttribution,
};
pub use mcp_cancel::McpCancelRegistry;
pub use metrics::{ToolWatchdogMetrics, ToolWatchdogMetricsSnapshot, WatchdogMetricLabel};
pub use progress::{apply_semantic_progress, ProgressFingerprint};
pub use registry::{
    fallback_eligible, CancellationClaim, RegisterTool, RegisterToolOutcome, RegistryAction,
    SemanticProgress, StaleLease, ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressApply,
    ToolProgressKey, TurnStamp, WatchdogInstant, FALLBACK_TOOL_CALL_ID,
};
pub use supervisor::{
    error_code_for_cause, escalate_claimed_lease, scope_for_capability, wait_stamp_from_lease,
    CancelHost, ConvergenceProbe, EscalationReport, EscalationStage, RegistryProbe,
    SpecificCancelOutcome, CONTROL_LANE_ADMIT_TIMEOUT, TERMINAL_ACK_TIMEOUT,
    TERMINAL_ADMIT_TIMEOUT, TERMINAL_KILL_EXECUTOR_TIMEOUT,
};
pub use types::*;
