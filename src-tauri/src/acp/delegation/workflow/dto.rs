//! Redacted frontend DTO for the workflow graph (`WorkflowGraphSnapshot`).
//!
//! **Never** includes `work_unit_key`. Free-text fields pass through A17
//! redaction. Distinct from agent-facing `WorkflowStateDto` (state_dto.rs).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use super::types::{
    AdmissionCompletionContextV2, CompletionEvidenceV2, EvidenceScopeInputV2,
    EvidenceValidationContext, InstructionBlockV1, RequirementsIdentityV1, RoleReviewScopeV2,
};

/// Frontend DTO schema version for `WorkflowGraphSnapshot`.
pub const WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Compatibility mode for a projected snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCompatibility {
    /// Active durable manifest + bindings overlaid with runs/gates.
    Manifest,
    /// Plan/progress locator plus durable delegation lifecycle.
    Simple,
    /// Recognized A1 keys only; no durable workflow header.
    ObservedOnly,
}

/// High-level projected overall state for the graph chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOverallState {
    Pending,
    Skeleton,
    Estimated,
    Approved,
    Blocked,
    InProgress,
    Completed,
    ObservedOnly,
}

/// Projected per-node lifecycle for UI (B12 vocabulary companion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectedNodeStatus {
    Pending,
    InProgress,
    Estimated,
    Reserving,
    Running,
    Completed,
    Failed,
    Canceled,
    Blocked,
    MissingSummary,
    WaitingReview,
    WaitingAdjudication,
    Superseded,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeSyncState {
    #[default]
    InSync,
    OutOfSync,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleWorkflowLocatorSnapshot {
    pub plan_rel_path: String,
    pub progress_rel_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedWorkflowNavigationSnapshot {
    pub source_conversation_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_rel_path: Option<String>,
    #[serde(default)]
    pub successor_conversation_id: Option<i32>,
    pub can_create_simple_successor: bool,
}

/// Safe, redacted graph snapshot attached to conversation detail / live reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowGraphSnapshot {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    pub workflow_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_protocol: Option<super::types::CompletionProtocolWorkflowProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<super::completion_projection::CompletionProjectionV2>,
    pub compatibility: WorkflowCompatibility,
    pub overall_state: WorkflowOverallState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simple: Option<SimpleWorkflowLocatorSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<ArchivedWorkflowNavigationSnapshot>,
    #[serde(default)]
    pub projection_warning_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_phase_id: Option<String>,
    pub current_node_ids: Vec<String>,
    pub phases: Vec<WorkflowPhaseSnapshot>,
    pub nodes: Vec<WorkflowNodeSnapshot>,
    pub edges: Vec<WorkflowEdgeSnapshot>,
    pub gates: Vec<WorkflowGateSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPhaseSnapshot {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Work-unit / milestone / gate node on the redacted graph.
///
/// **No `work_unit_key` field** — keys stay backend / agent-facing only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNodeSnapshot {
    pub node_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Launch model id from the latest run's allowlisted `config_values_json`
    /// (`model` / `model_id` / `modelId`). Absent when no run or unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning / thinking effort from the latest run's
    /// `config_values_json` (`effort` / `reasoning` / `thinking`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_risk_level: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_risk_reason_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_reviewer_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_reviewer_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: ProjectedNodeStatus,
    #[serde(default)]
    pub sync_state: WorkflowNodeSyncState,
    #[serde(default)]
    pub projection_warning_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    /// B12: all generations for this work unit.
    pub run_count: u64,
    /// B12: generation of the latest/active child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child_generation: Option<i64>,
    /// B12: number of replacement links on the lineage.
    pub replacement_count: u64,
    /// B12: document gate cycle when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_cycle: Option<i64>,
    /// B12: continue rounds on the active child (`generation - 1` when gen ≥ 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_child_conversation_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_status: Option<String>,
    /// Latest-run start time (RFC3339) for the live portion of elapsed display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Latest-run finish time (RFC3339); absent while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Sum of **finished** run durations (ms) across the work-unit lineage.
    /// Excludes an in-flight latest run so the UI can add `now - started_at`.
    /// When the latest run is terminal this includes its full duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_completed_ms: Option<u64>,
    /// Latest-run tool call count (delegation runtime stats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_count: Option<u64>,
    /// Latest-run edit-tool call count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_tool_call_count: Option<u64>,
    /// Count of touched files from latest-run runtime stats (paths not exposed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touched_file_count: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub touched_files_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_counts_complete: Option<bool>,
    /// Bounded, redacted card-summary text when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<super::completion_projection::CompletionProjectionV2>,
    pub is_observed: bool,
    pub retained_observed: bool,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_outcome: Option<String>,
    pub deps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEdgeSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowGateSnapshot {
    pub gate_id: String,
    pub gate_kind: String,
    pub resolution_mode: String,
    pub required_reviewer_node_ids: Vec<String>,
    pub required_count: u64,
    pub returned_count: u64,
    pub running_count: u64,
    pub blocked_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_gate_cycle: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_summary: Option<String>,
}

// ---------------------------------------------------------------------------
// A17 display-string redaction
// ---------------------------------------------------------------------------

const REDACTED: &str = "[redacted]";

/// Redact free-text for frontend display (A17).
///
/// Fails closed on absolute paths, `work_unit_key`-shaped tokens, and
/// prompt-like fences — those spans become `[redacted]`.
pub fn redact_display_string(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let mut out = input.to_string();

    // Prompt-like fences: strip fenced blocks entirely.
    out = strip_fenced_blocks(&out);

    // Absolute paths (Windows drive, UNC, POSIX absolute).
    out = scrub_absolute_paths(&out);

    // work_unit_key-shaped tokens (A1 prefixes).
    out = scrub_work_unit_key_tokens(&out);

    // If anything remains that still looks like a raw key or absolute path,
    // fail closed for the whole string.
    if looks_like_work_unit_key(&out) || contains_absolute_path_hint(&out) {
        return REDACTED.to_string();
    }

    out
}

/// Optional helper: redact or drop entirely.
pub fn redact_optional_display(input: Option<&str>) -> Option<String> {
    input.map(redact_display_string).filter(|s| !s.is_empty())
}

/// Map a wire id/label to a safe public form (A17 on free-text ids).
///
/// Safe opaque ids (ascii alnum / `_` / `-`, not `pub_`-prefixed) pass through.
/// Unsafe strings become `pub_<sha256-hex-prefix>` (16 hex chars of SHA-256).
/// Prefer [`PublicIdAllocator`] when issuing many ids so reverse-map collisions
/// are rejected.
pub fn safe_public_id(raw: &str) -> String {
    if is_passthrough_public_id(raw) {
        return raw.to_string();
    }
    opaque_hashed_id(raw, 0)
}

/// True when `s` may pass through as a public id without hashing.
///
/// Deliberately rejects the `pub_` namespace so raw strings that look like
/// generated hashes cannot collide with hashed unsafe ids.
pub fn is_opaque_safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !s.contains("..")
}

pub fn is_passthrough_public_id(s: &str) -> bool {
    is_opaque_safe_id(s) && !s.starts_with("pub_")
}

/// SHA-256 hex of `raw`, used for opaque public ids.
pub fn sha256_hex_str(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `pub_<16 hex of SHA-256>` optionally with a collision counter suffix.
pub fn opaque_hashed_id(raw: &str, collision_n: u64) -> String {
    let hex = sha256_hex_str(raw);
    let prefix = &hex[..16.min(hex.len())];
    if collision_n == 0 {
        format!("pub_{prefix}")
    } else {
        format!("pub_{prefix}_{collision_n}")
    }
}

/// Allocates public ids with reverse-map collision rejection.
#[derive(Debug, Default)]
pub struct PublicIdAllocator {
    /// raw → public
    forward: std::collections::HashMap<String, String>,
    /// public → raw
    reverse: std::collections::HashMap<String, String>,
}

impl PublicIdAllocator {
    pub fn map_id(&mut self, raw: &str) -> String {
        if let Some(p) = self.forward.get(raw) {
            return p.clone();
        }
        let candidate = if is_passthrough_public_id(raw) {
            if let Some(owner) = self.reverse.get(raw) {
                if owner == raw {
                    raw.to_string()
                } else {
                    // Collides with an already-issued public id → force hash.
                    self.allocate_opaque(raw)
                }
            } else {
                raw.to_string()
            }
        } else {
            self.allocate_opaque(raw)
        };
        self.forward.insert(raw.to_string(), candidate.clone());
        self.reverse.insert(candidate.clone(), raw.to_string());
        candidate
    }

    fn allocate_opaque(&mut self, raw: &str) -> String {
        let mut n = 0u64;
        loop {
            let c = opaque_hashed_id(raw, n);
            match self.reverse.get(&c) {
                None => return c,
                Some(owner) if owner == raw => return c,
                Some(_) => n = n.saturating_add(1),
            }
        }
    }
}

fn strip_fenced_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        out.push_str(REDACTED);
        let after = &rest[start + 3..];
        if let Some(end) = after.find("```") {
            rest = &after[end + 3..];
        } else {
            // Unclosed fence: redact remainder.
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn scrub_absolute_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        // Quoted string: if contents look path/key-shaped, redact whole quote.
        if chars[i] == '"' || chars[i] == '\'' {
            let q = chars[i];
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == q) {
                let inner: String = chars[i + 1..i + 1 + end].iter().collect();
                if contains_absolute_path_hint(&inner) || looks_like_work_unit_key(&inner) {
                    out.push_str(REDACTED);
                    i = i + 1 + end + 1;
                    continue;
                }
            }
        }
        if let Some(len) = absolute_path_len(&chars[i..]) {
            out.push_str(REDACTED);
            i += len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Return length (in chars) of an absolute path starting at `chars[0]`, if any.
/// Path tokens are whole-token: spaces inside the path are kept (not terminators).
fn absolute_path_len(chars: &[char]) -> Option<usize> {
    if chars.is_empty() {
        return None;
    }

    // Windows drive: C:\ or C:/
    if chars.len() >= 3
        && chars[0].is_ascii_alphabetic()
        && chars[1] == ':'
        && (chars[2] == '\\' || chars[2] == '/')
    {
        return Some(consume_path_chars(chars, 3));
    }

    // UNC: \\server\share
    if chars.len() >= 2 && chars[0] == '\\' && chars[1] == '\\' {
        return Some(consume_path_chars(chars, 2));
    }

    // POSIX absolute: leading `/` with at least one path segment (fail-closed).
    if chars[0] == '/' {
        let end = consume_path_chars(chars, 1);
        // Need a non-empty segment after the slash.
        if end > 1 {
            return Some(end);
        }
    }

    None
}

/// Consume path characters; do **not** stop at space (space-containing paths).
/// Terminators: quote, pipe, backtick, brackets/parens, control, comma/semicolon.
fn consume_path_chars(chars: &[char], start: usize) -> usize {
    let mut i = start;
    while i < chars.len() {
        let c = chars[i];
        if c == '|'
            || c == '"'
            || c == '\''
            || c == '`'
            || c == ')'
            || c == ']'
            || c == '('
            || c == '['
            || c == ','
            || c == ';'
            || c.is_control()
        {
            break;
        }
        i += 1;
    }
    // Trim trailing punctuation that is unlikely path content.
    while i > start {
        let c = chars[i - 1];
        if c == '.' || c == ',' || c == ';' || c == ':' {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

fn scrub_work_unit_key_tokens(s: &str) -> String {
    // Match A1 prefixes: design| plan| task| final_review|
    // Whole-token: do not stop at space (keys rarely have spaces, but fail-closed).
    let prefixes = ["design|", "plan|", "task|", "final_review|"];
    let mut out = s.to_string();
    for prefix in prefixes {
        while let Some(idx) = out.find(prefix) {
            let rest = &out[idx..];
            let end = rest
                .char_indices()
                .find(|(_, c)| {
                    *c == '"' || *c == '\'' || *c == '`' || *c == ')' || *c == ']' || c.is_control()
                })
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let mut replaced = String::with_capacity(out.len());
            replaced.push_str(&out[..idx]);
            replaced.push_str(REDACTED);
            replaced.push_str(&out[idx + end..]);
            out = replaced;
        }
    }
    out
}

fn looks_like_work_unit_key(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("design|")
        || t.starts_with("plan|")
        || t.starts_with("task|")
        || t.starts_with("final_review|")
}

fn contains_absolute_path_hint(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if absolute_path_len(&chars[i..]).is_some() {
            return true;
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_any_posix_absolute_with_segments_not_allowlist() {
        // Fail-closed: not limited to known roots like /home or /tmp.
        let s = redact_display_string("leaked /secret/project/keys.pem here");
        assert!(!s.contains("/secret/project"));
        assert!(s.contains(REDACTED));
    }

    #[test]
    fn redacts_windows_absolute_path() {
        let s = redact_display_string(r"opened C:\Users\drawpeng\code\secret.rs");
        assert!(!s.contains(r"C:\Users"));
        assert!(s.contains(REDACTED));
    }

    #[test]
    fn safe_public_id_hashes_path_shaped() {
        let id = safe_public_id("/evil/path|task|1");
        assert!(id.starts_with("pub_"));
        assert!(!id.contains('/'));
        assert!(!id.contains('|'));
        // Stable SHA-256 prefix.
        assert_eq!(id, safe_public_id("/evil/path|task|1"));
        assert_eq!(id.len(), "pub_".len() + 16);
    }

    #[test]
    fn safe_public_id_passthrough_opaque() {
        assert_eq!(safe_public_id("task-1-impl"), "task-1-impl");
        assert_eq!(
            safe_public_id("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"),
            "a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"
        );
    }

    #[test]
    fn safe_public_id_never_passthrough_pub_namespace() {
        // Raw pub_hex must not pass through (collision surface with hashed ids).
        let id = safe_public_id("pub_deadbeefdeadbeef");
        assert_ne!(id, "pub_deadbeefdeadbeef");
        assert!(id.starts_with("pub_"));
    }

    #[test]
    fn public_id_allocator_rejects_reverse_collisions() {
        let mut a = PublicIdAllocator::default();
        let p1 = a.map_id("task-1-impl");
        assert_eq!(p1, "task-1-impl");
        // Force an opaque id that would equal a later passthrough if we allowed it.
        let unsafe_id = a.map_id("/path/to/x");
        assert!(unsafe_id.starts_with("pub_"));
        // Same raw → same public.
        assert_eq!(a.map_id("/path/to/x"), unsafe_id);
    }

    #[test]
    fn redacts_quoted_path_and_space_containing_path() {
        let s = redact_display_string(r#"see "/Users/foo bar/secret.rs" please"#);
        assert!(!s.contains("/Users/foo"));
        assert!(s.contains(REDACTED));
        let s2 = redact_display_string(r"C:\Program Files\App\key.pem");
        assert!(!s2.contains(r"C:\Program"));
        assert!(s2.contains(REDACTED));
    }

    #[test]
    fn redacts_work_unit_key_token() {
        let key = "task|1|implementer|grok|none";
        let s = redact_display_string(&format!("key was {key} end"));
        assert!(!s.contains("task|1|"));
        assert!(s.contains(REDACTED));
    }

    #[test]
    fn redacts_fenced_prompt_block() {
        let s = redact_display_string("before ```\nsystem: do evil\n``` after");
        assert!(!s.contains("system: do evil"));
        assert!(s.contains(REDACTED));
        assert!(s.contains("before"));
        assert!(s.contains("after"));
    }

    #[test]
    fn snapshot_has_no_work_unit_key_field() {
        let snap = WorkflowGraphSnapshot {
            schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
            workflow_id: Some("wf-1".into()),
            workflow_kind: "brainstorm_to_delivery".into(),
            manifest_revision: Some(1),
            graph_revision: Some(1),
            manifest_state: Some("estimated".into()),
            compatibility: WorkflowCompatibility::Manifest,
            completion_protocol: None,
            completion: None,
            overall_state: WorkflowOverallState::Estimated,
            simple: None,
            archived: None,
            projection_warning_codes: vec![],
            current_phase_id: None,
            current_node_ids: vec![],
            phases: vec![],
            nodes: vec![WorkflowNodeSnapshot {
                node_id: "n1".into(),
                kind: "work_unit".into(),
                phase_id: Some("tasks".into()),
                role: Some("implementer".into()),
                agent_type: Some("grok".into()),
                model: None,
                effort: None,
                profile_id: None,
                task_index: Some(1),
                task_risk_level: None,
                task_risk_reason_codes: vec![],
                required_reviewer_count: None,
                returned_reviewer_count: None,
                title: None,
                status: ProjectedNodeStatus::Estimated,
                sync_state: WorkflowNodeSyncState::InSync,
                projection_warning_codes: vec![],
                status_reason: None,
                run_count: 0,
                active_child_generation: None,
                replacement_count: 0,
                gate_cycle: None,
                round_count: None,
                latest_task_id: None,
                latest_child_conversation_id: None,
                latest_run_status: None,
                started_at: None,
                finished_at: None,
                elapsed_completed_ms: None,
                tool_call_count: None,
                edit_tool_call_count: None,
                touched_file_count: None,
                touched_files_truncated: false,
                additions: None,
                deletions: None,
                line_counts_complete: None,
                summary: None,
                completion: None,
                is_observed: false,
                retained_observed: false,
                required: true,
                node_outcome: None,
                deps: vec![],
            }],
            edges: vec![],
            gates: vec![],
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(
            !json.contains("work_unit_key"),
            "redacted snapshot must not serialize work_unit_key: {json}"
        );
        let value = serde_json::to_value(&snap).expect("serialize snapshot value");
        assert_eq!(value["nodes"][0]["sync_state"], "in_sync");
        assert_eq!(value["nodes"][0]["projection_warning_codes"], serde_json::json!([]));
    }

    #[test]
    fn simple_and_archived_snapshots_have_stable_navigation_wire_shapes() {
        let simple = WorkflowGraphSnapshot {
            schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
            workflow_id: None,
            workflow_kind: "brainstorm_to_delivery".into(),
            manifest_revision: None,
            graph_revision: None,
            manifest_state: None,
            completion_protocol: None,
            completion: None,
            compatibility: WorkflowCompatibility::Simple,
            overall_state: WorkflowOverallState::Pending,
            simple: Some(SimpleWorkflowLocatorSnapshot {
                plan_rel_path: "docs/superpowers/plans/plan.md".into(),
                progress_rel_path: ".superpowers/sdd/42/progress.md".into(),
            }),
            archived: None,
            projection_warning_codes: vec!["simple_progress_block_missing".into()],
            current_phase_id: Some("tasks".into()),
            current_node_ids: vec!["simple-task-1".into()],
            phases: vec![],
            nodes: vec![],
            edges: vec![],
            gates: vec![],
        };
        assert_eq!(
            serde_json::to_value(simple).expect("serialize Simple snapshot"),
            serde_json::json!({
                "schema_version": 1,
                "workflow_kind": "brainstorm_to_delivery",
                "compatibility": "simple",
                "overall_state": "pending",
                "simple": {
                    "plan_rel_path": "docs/superpowers/plans/plan.md",
                    "progress_rel_path": ".superpowers/sdd/42/progress.md",
                },
                "projection_warning_codes": ["simple_progress_block_missing"],
                "current_phase_id": "tasks",
                "current_node_ids": ["simple-task-1"],
                "phases": [],
                "nodes": [],
                "edges": [],
                "gates": [],
            })
        );

        let archived = WorkflowGraphSnapshot {
            schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
            workflow_id: Some("pub_workflow".into()),
            workflow_kind: "brainstorm_to_delivery".into(),
            manifest_revision: Some(3),
            graph_revision: Some(5),
            manifest_state: Some("approved".into()),
            completion_protocol: None,
            completion: None,
            compatibility: WorkflowCompatibility::Manifest,
            overall_state: WorkflowOverallState::Approved,
            simple: None,
            archived: Some(ArchivedWorkflowNavigationSnapshot {
                source_conversation_id: 7,
                plan_rel_path: Some("docs/superpowers/plans/plan.md".into()),
                successor_conversation_id: None,
                can_create_simple_successor: false,
            }),
            projection_warning_codes: vec![],
            current_phase_id: None,
            current_node_ids: vec![],
            phases: vec![],
            nodes: vec![],
            edges: vec![],
            gates: vec![],
        };
        let archived_json = serde_json::to_value(archived).expect("serialize archived snapshot");
        assert_eq!(archived_json["compatibility"], "manifest");
        assert_eq!(
            archived_json["archived"],
            serde_json::json!({
                "source_conversation_id": 7,
                "plan_rel_path": "docs/superpowers/plans/plan.md",
                "successor_conversation_id": null,
                "can_create_simple_successor": false,
            })
        );
        assert_eq!(archived_json["projection_warning_codes"], serde_json::json!([]));
        assert!(archived_json.get("simple").is_none());
    }
}
