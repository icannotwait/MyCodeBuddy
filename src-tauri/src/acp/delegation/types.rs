//! Broker-facing request / outcome types.
//!
//! These cross two boundaries:
//! 1. The MCP companion serializes `DelegationRequest` → JSON-RPC params and
//!    deserializes `DelegationOutcome` → MCP `tool_result`.
//! 2. The broker emits a structured outcome the listener can persist and
//!    forward to the parent's tool_use_id.
//!
//! DB ids are `i32` to match the actual `conversation.id` / `conversation.parent_id`
//! column types — keeping them strongly typed here saves us a parse-or-die step
//! at every DB boundary.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::AgentType;

/// Soft-watchdog health for a **running** Broker task only. Terminal tasks
/// have no observation. Observe-only — never a lifecycle / terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskObservation {
    Active,
    Stalled,
    WaitingInput,
}

/// Snapshot published by the soft supervisor when observation or timestamps
/// change. `stalled_since` is `last_agent_activity_at + threshold` (not scan time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationSnapshot {
    pub observation: TaskObservation,
    pub last_agent_activity_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_since: Option<DateTime<Utc>>,
}

/// Per-agent defaults applied when codeg-mcp spawns a subagent on behalf of a
/// `delegate_to_agent` call. Mirrors the two knobs `ConnectionManager::spawn_agent`
/// already accepts:
///   * `mode_id` → forwarded as `preferred_mode_id`
///   * `config_values` → forwarded as `preferred_config_values`
///
/// All fields are optional / may be empty; an absent entry means "no override —
/// use whatever the agent advertises as the default."
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDelegationDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_values: BTreeMap<String, String>,
}

impl AgentDelegationDefaults {
    pub fn is_empty(&self) -> bool {
        self.mode_id.is_none() && self.config_values.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfile {
    pub id: String,
    pub agent_type: AgentType,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_values: BTreeMap<String, String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfileDocument {
    #[serde(default)]
    pub profiles: Vec<DelegationProfile>,
}

/// Everything the broker needs to dispatch a single delegation call.
///
/// `parent_connection_id` is the codeg-internal ACP connection UUID for the
/// parent session (NOT the agent-assigned ACP session id). The broker uses it
/// to inherit the parent's EventEmitter/working_dir and to scope
/// `cancel_by_parent`.
///
/// `external_handle` is a companion-minted opaque token (per MCP `tools/call`)
/// that the broker stores alongside the pending entry so an MCP-side
/// `notifications/cancelled` can target this specific delegation without the
/// companion having to know the broker-internal `call_id`. `None` for non-MCP
/// callers and tests that don't exercise the cancel path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRequest {
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: String,
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub task: String,
    pub working_dir: Option<String>,
    /// The `working_dir` exactly as the LLM passed it in the
    /// `delegate_to_agent` arguments, BEFORE the listener defaults a missing
    /// value to the parent's launch directory. Used only as part of the
    /// `(agent_type, task, requested_working_dir)` correlation key so two
    /// parallel calls sharing an agent and task but targeting different
    /// explicit directories don't bind to each other's `tool_call_id`.
    /// `None` when the LLM omitted it — symmetric with the ACP `raw_input`,
    /// which also omits it then. Distinct from `working_dir` above, which is
    /// the defaulted value the child is actually spawned in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_handle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationSuccess {
    pub text: String,
    pub child_conversation_id: i32,
    pub child_agent_type: AgentType,
    pub turn_count: u32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
}

/// Stable English guidance returned for `cancel_delegation` with `reason=timeout`.
/// Intentionally not localized: the companion speaks to the LLM, not the UI.
pub const TIMEOUT_CANCEL_GUIDANCE: &str =
    "Do not cancel a still-running sub-agent; keep polling get_delegation_status.";

/// Drop fenced code blocks (``` or ~~~) so pasted docs/examples cannot
/// install mandatory routes from illustrative links or directives.
///
/// CommonMark rules (simplified):
/// - open/close by the same run character (`` ` `` or `~`)
/// - a closing fence must be at least as long as the opener
///
/// so `~~~` inside a ``` block, or ``` inside ````, does not end it early.
fn text_outside_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // None = outside; Some((char, min_len)) = open fence.
    let mut open_fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = {
            let bytes = trimmed.as_bytes();
            if bytes.len() >= 3 && (bytes[0] == b'`' || bytes[0] == b'~') {
                let ch = bytes[0] as char;
                let mut n = 0usize;
                while n < bytes.len() && bytes[n] == bytes[0] {
                    n += 1;
                }
                // Info string after backticks must not contain more backticks
                // for a valid open fence; we only need a conservative filter
                // for route extraction, so any run of ≥3 counts.
                if n >= 3 {
                    Some((ch, n))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((ch, n)) = fence {
            match open_fence {
                None => open_fence = Some((ch, n)),
                Some((open_ch, open_n)) if open_ch == ch && n >= open_n => {
                    open_fence = None;
                }
                Some(_) => {
                    // Wrong char or too-short closer — still inside the block.
                }
            }
            continue;
        }
        if open_fence.is_none() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Extract immutable profile UUIDs from a user prompt using only structured
/// forms the composer emits — not free-form prose or pasted examples.
///
/// Accepted:
/// - A whole line that *starts* with `Codeg mandatory delegation route:` and
///   contains `profile_id="<uuid>"` (composer directive; column 0 only)
/// - Closed markdown links: `[label](codeg://delegation-profile/<uuid>)`
///   or `[label](codeg://delegation-profile/<agent_type>/<uuid>)`
///
/// Rejected: bare URIs, prose `profile_id=`, unterminated/malformed links,
/// indented/buried directive phrases, and content inside fenced code blocks.
pub fn extract_mandatory_profile_ids(text: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let text = text_outside_code_fences(text);
    let mut out = BTreeSet::new();
    let uuid = |s: &str| -> Option<String> {
        let s = s.trim();
        if uuid::Uuid::parse_str(s).is_ok() {
            Some(s.to_string())
        } else {
            None
        }
    };

    const DIRECTIVE_PREFIX: &str = "Codeg mandatory delegation route:";
    for line in text.lines() {
        // Composer-injected directive lines only (column 0 — no leading spaces).
        let Some(mut rest) = line.strip_prefix(DIRECTIVE_PREFIX) else {
            continue;
        };
        while let Some(idx) = rest.find("profile_id=\"") {
            rest = &rest[idx + "profile_id=\"".len()..];
            if let Some(end) = rest.find('"') {
                if let Some(id) = uuid(&rest[..end]) {
                    out.insert(id);
                }
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
    }

    // Markdown link destinations only: [label](codeg://delegation-profile/...)
    // Require a matching `[` before the `]` and a closing `)`.
    const LINK_PREFIX: &str = "](codeg://delegation-profile/";
    let lower = text.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find(LINK_PREFIX) {
        let close_bracket = search_from + rel; // index of ']'
        search_from = close_bracket + LINK_PREFIX.len();
        // Valid markdown: a '[' before this ']' with no intervening ']'.
        let prefix = &text[..close_bracket];
        let Some(open) = prefix.rfind('[') else {
            continue;
        };
        if prefix[open + 1..].contains(']') {
            continue;
        }
        let after = &text[search_from..];
        // Require a real closing ')' — unterminated destinations are not links.
        let Some(token_end) = after.find(')') else {
            continue;
        };
        let token = after[..token_end].trim();
        let path = token
            .split([' ', '"', '\''])
            .next()
            .unwrap_or("");
        let candidate = path.rsplit_once('/').map(|(_, id)| id).unwrap_or(path);
        if let Some(id) = uuid(candidate) {
            out.insert(id);
        }
    }
    out.into_iter().collect()
}

/// Broker-internal failure modes. Serialized via the wrapping
/// [`DelegationOutcome::Err`] variant — the broker maps each into a stable
/// `code` string so the frontend / MCP consumer can pattern-match without
/// caring about the inner shape.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum DelegationError {
    #[error("depth limit exceeded ({current_depth} >= {limit})")]
    DepthLimitExceeded { current_depth: u32, limit: u32 },
    #[error("invalid agent type")]
    InvalidAgentType,
    #[error("invalid delegation profile: {0}")]
    InvalidDelegationProfile(String),
    #[error("delegation profile is disabled: {0}")]
    DelegationProfileDisabled(String),
    #[error("delegation profile agent does not match request: {0}")]
    DelegationProfileAgentMismatch(String),
    /// Parent user prompt mentioned one or more profiles for this request's
    /// agent_type (`M_T`), but this call did not supply a usable `profile_id`
    /// (and auto-fill could not uniquely resolve), or supplied one outside `M_T`.
    /// The `{0}` payload is **detail-only** (no second "mandatory profile_id
    /// required" prefix); full wire text is this Display template + detail.
    #[error("mandatory profile_id required: {0}")]
    MandatoryProfileRequired(String),
    #[error("invalid working dir: {0}")]
    InvalidWorkingDir(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("subagent runtime error: {0}")]
    SubagentRuntimeError(String),
    /// Child agent ended its turn via `refusal`. Often a backend / gateway
    /// error masquerading as a refusal per the ACP spec gap.
    #[error("subagent refused to continue")]
    ChildRefusal,
    #[error("subagent reached max token budget")]
    ChildMaxTokens,
    #[error("subagent reached max turn request budget")]
    ChildMaxTurnRequests,
    /// Child reported `end_turn` without producing any output (synthesized
    /// as `empty` by the connection loop's "silent EndTurn" guard).
    #[error("subagent produced no output")]
    ChildEmpty,
    #[error("subagent ended with unrecognized stop reason: {0}")]
    ChildUnknown(String),
    #[error("canceled: {reason}")]
    Canceled { reason: String },
    #[error("parent session is gone")]
    ParentSessionGone,
}

/// The single value the broker hands back to the listener / MCP companion.
/// `child_conversation_id` on the `Err` arm is best-effort — it's `Some` once
/// the broker successfully created the child DB row, even if the run later
/// fails or times out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DelegationOutcome {
    Ok(DelegationSuccess),
    Err {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        child_conversation_id: Option<i32>,
    },
}

/// Lifecycle status of an asynchronous delegation task. Surfaced by the
/// three delegation tools — `delegate_to_agent` (returns a `Running` ack, or
/// a terminal status when the child finished during setup / setup failed),
/// `get_delegation_status`, and `cancel_delegation`. Wire-stable snake_case
/// strings: they ship to LLM context and to the frontend, so don't rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Child is running in the background; no terminal result yet.
    Running,
    /// Child ended its turn cleanly; `text` carries the result (possibly
    /// truncated — open the child session for the full output).
    Completed,
    /// Child ended in a non-cancel failure; `error_code` / `message` describe it.
    Failed,
    /// Task was canceled (by `cancel_delegation`, parent teardown, or a
    /// non-`end_turn` parent turn end).
    Canceled,
    /// Task id is not known to this parent — never existed, belonged to a
    /// different parent, or its result was evicted from the cache and no DB
    /// row backs it.
    Unknown,
}

/// Unified response the broker hands the listener for every delegation tool
/// (`delegate_to_agent` / `get_delegation_status` / `cancel_delegation`). The
/// listener serializes it into `BrokerResponse.outcome`; the companion renders
/// it into the MCP `CallToolResult` (with `structuredContent` carrying this
/// whole shape so the frontend can read `status` and distinguish a running ack
/// from a terminal outcome).
///
/// Fields are all optional except `status` so one type can describe a running
/// ack (ids + `Running`), a completed result (`text` + `duration_ms`), a
/// failure (`error_code` + `message`), and a setup failure (`task_id: None`).
///
/// Soft-watchdog fields (`observation`, `last_agent_activity_at`,
/// `stalled_since`) appear **only** on `Running` reports when the supervisor
/// has published a snapshot; terminal and unknown reports omit them on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationTaskReport {
    /// Broker `call_id` (UUID) identifying the task. `None` only when setup
    /// failed before a task was registered (no id to track).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_conversation_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<AgentType>,
    /// Completed result text (capped; open the child session for the full
    /// output). Only set for `Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Wire-stable error code for `Failed` / `Canceled` (mirrors
    /// `DelegationOutcome::Err.code`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Human-readable note: the failure message, or a hint like
    /// "running in background" / "result not cached; open child session N".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Soft-watchdog health. Present only on `Running` when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<TaskObservation>,
    /// Last child agent activity timestamp from the observation cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_activity_at: Option<DateTime<Utc>>,
    /// Stall start (`last_agent_activity_at + threshold`); only when stalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_since: Option<DateTime<Utc>>,
}

impl DelegationOutcome {
    /// Project a `DelegationError` onto the wire-stable `code` string used by
    /// the frontend and MCP companion. Keep these strings stable — they ship
    /// to LLM context.
    pub fn from_err(err: DelegationError, child_conversation_id: Option<i32>) -> Self {
        let code = match &err {
            DelegationError::DepthLimitExceeded { .. } => "depth_limit",
            DelegationError::InvalidAgentType => "invalid_agent_type",
            DelegationError::InvalidDelegationProfile(_) => "invalid_delegation_profile",
            DelegationError::DelegationProfileDisabled(_) => "delegation_profile_disabled",
            DelegationError::DelegationProfileAgentMismatch(_) => {
                "delegation_profile_agent_mismatch"
            }
            DelegationError::MandatoryProfileRequired(_) => "mandatory_profile_required",
            DelegationError::InvalidWorkingDir(_) => "invalid_working_dir",
            DelegationError::SpawnFailed(_) => "spawn_failed",
            DelegationError::SubagentRuntimeError(_) => "subagent_error",
            DelegationError::ChildRefusal => "child_refusal",
            DelegationError::ChildMaxTokens => "child_max_tokens",
            DelegationError::ChildMaxTurnRequests => "child_max_turn_requests",
            DelegationError::ChildEmpty => "child_empty",
            DelegationError::ChildUnknown(_) => "child_unknown",
            DelegationError::Canceled { .. } => "canceled",
            DelegationError::ParentSessionGone => "canceled",
        };
        DelegationOutcome::Err {
            code: code.to_string(),
            message: err.to_string(),
            child_conversation_id,
        }
    }
}

#[cfg(test)]
mod extract_tests {
    use super::extract_mandatory_profile_ids;

    #[test]
    fn extracts_profile_id_directive_and_uri_forms() {
        let a = "11111111-1111-4111-8111-111111111111";
        let b = "22222222-2222-4222-8222-222222222222";
        let noise = "33333333-3333-4333-8333-333333333333";
        let fenced = "44444444-4444-4444-8444-444444444444";
        let tilde = "55555555-5555-4555-8555-555555555555";
        let buried = "66666666-6666-4666-8666-666666666666";
        let open_link = "77777777-7777-4777-8777-777777777777";
        // Note: Rust `\` string continuations strip leading whitespace on the
        // next physical line, so leading-space cases must embed `\x20` explicitly.
        let text = format!(
            "Codeg mandatory delegation route: profile_id=\"{a}\" for @X\n\
also see [y](codeg://delegation-profile/code_buddy/{b})\n\
ignore bare codeg://delegation-profile/{noise}\n\
and prose profile_id=\"{noise}\" not on a directive line\n\
and malformed ](codeg://delegation-profile/{noise})\n\
and [broken] label](codeg://delegation-profile/{noise})\n\
and unterminated [open](codeg://delegation-profile/{open_link}\n\
see docs about Codeg mandatory delegation route profile_id=\"{buried}\" in prose\n\
\x20Codeg mandatory delegation route: profile_id=\"{buried}\" indented\n\
```\n\
[doc](codeg://delegation-profile/{fenced})\n\
Codeg mandatory delegation route: profile_id=\"{fenced}\"\n\
~~~\n\
still inside backtick fence: profile_id=\"{fenced}\"\n\
```\n\
~~~\n\
[tilde](codeg://delegation-profile/{tilde})\n\
~~~\n\
````\n\
```\n\
Codeg mandatory delegation route: profile_id=\"{fenced}\"\n\
```\n\
````\n"
        );
        let ids = extract_mandatory_profile_ids(&text);
        assert_eq!(ids, vec![a.to_string(), b.to_string()]);
    }
}
