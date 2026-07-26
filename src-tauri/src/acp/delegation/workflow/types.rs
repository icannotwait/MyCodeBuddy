//! Shared types for workflow key derivation and manifest validation.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Max length of a canonical `work_unit_key` after normalization (MCP + A1).
/// Counted as Unicode scalar values (`str::chars`), not UTF-8 bytes.
pub const MAX_WORK_UNIT_KEY_LEN: usize = 200;

/// Canonical phase ids for brainstorm-to-delivery work units.
pub const PHASE_DESIGN: &str = "design";
pub const PHASE_PLAN: &str = "plan";
pub const PHASE_TASKS: &str = "tasks";
pub const PHASE_FINAL: &str = "final";

/// A15.2 concrete v1 bounds (validator + UI agree).
pub const MAX_TASKS: usize = 100;
pub const MAX_NODES: usize = 400;
pub const MAX_EDGES: usize = 800;
pub const MAX_GATES: usize = 50;
pub const MAX_ADJUDICATION_SUMMARY_BYTES: usize = 4 * 1024;
pub const MAX_MANIFEST_JSON_BYTES: usize = 512 * 1024;

/// Fixed workflow kind for this feature.
pub const WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY: &str = "brainstorm_to_delivery";

/// Supported schema version for `ManifestDocument`.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Typed errors for key derivation and manifest validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowError {
    #[error("work unit key exceeds {MAX_WORK_UNIT_KEY_LEN} characters")]
    KeyTooLong,
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("dependency cycle detected")]
    Cycle,
    #[error("duplicate id: {0}")]
    DuplicateId(String),
    #[error("bounds exceeded: {0}")]
    BoundsExceeded(String),
    #[error("role mismatch: {0}")]
    RoleMismatch(String),
    #[error("invalid schema version: expected {MANIFEST_SCHEMA_VERSION}, got {0}")]
    InvalidSchemaVersion(u32),
    #[error("unsupported workflow kind: {0}")]
    UnsupportedWorkflowKind(String),
    #[error("invalid gate shape: {0}")]
    InvalidGateShape(String),
    #[error("work unit key mismatch for node {node_id}: expected {expected}, got {got}")]
    KeyMismatch {
        node_id: String,
        expected: String,
        got: String,
    },
    #[error("missing field: {0}")]
    MissingField(String),
    #[error("unknown reference: {0}")]
    UnknownReference(String),
    #[error("invalid field: {0}")]
    InvalidField(String),
    #[error("invalid agent type: {0}")]
    InvalidAgentType(String),
    #[error("invalid task index: {0}")]
    InvalidTaskIndex(String),
}

/// Materials used to build a canonical A1 `work_unit_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkUnitKeyParts<'a> {
    Design {
        rel_doc_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    Plan {
        rel_plan_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    TaskImplementer {
        task_index: u32,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    TaskReviewer {
        task_index: u32,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    FinalReviewer {
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    FinalFixer {
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
}

/// Result of parsing a recognized A1-grammar work unit key (A11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedWorkUnitKey {
    Design {
        rel_doc_path: String,
        agent_type: String,
        profile_id: Option<String>,
    },
    Plan {
        rel_plan_path: String,
        agent_type: String,
        profile_id: Option<String>,
    },
    TaskImplementer {
        task_index: u32,
        agent_type: String,
        profile_id: Option<String>,
    },
    TaskReviewer {
        task_index: u32,
        agent_type: String,
        profile_id: Option<String>,
    },
    FinalReviewer {
        agent_type: String,
        profile_id: Option<String>,
    },
    FinalFixer {
        agent_type: String,
        profile_id: Option<String>,
    },
}

/// Manifest publication / lifecycle state accepted on the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestWorkflowState {
    Skeleton,
    Estimated,
    Approved,
    Blocked,
}

/// Node kind on a published manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestNodeKind {
    Milestone,
    WorkUnit,
    Gate,
    Placeholder,
}

/// Work-unit role on a delegated node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestNodeRole {
    Reviewer,
    Implementer,
    Fixer,
}

/// Optional durable node outcome (B14.3 cancel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestNodeOutcome {
    Canceled,
}

/// Document-gate resolution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMode {
    /// Concurrent/multi-reviewer parent adjudication (default for Plan).
    ParentAdjudication,
    /// Zero-reviewer Design self-review (A12 only).
    SelfReview,
}

/// Document gate kind (Design / Plan only).
/// Optional on the wire; inferred fail-closed from reviewers when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentGateKind {
    Design,
    Plan,
}

impl DocumentGateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Design => PHASE_DESIGN,
            Self::Plan => PHASE_PLAN,
        }
    }

    pub const fn expected_reviewer_phase(self) -> &'static str {
        self.as_str()
    }
}

/// Workspace-relative document identity (path + digest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRef {
    pub rel_path: String,
    pub digest: String,
}

/// Phase entry in a manifest document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPhase {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Node entry in a manifest document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestNode {
    pub id: String,
    pub kind: ManifestNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ManifestNodeRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_key: Option<String>,
    /// Required on the wire (no serde default). Empty list is allowed.
    pub deps: Vec<String>,
    /// Document reviewers default `true` when the field is present as null/absent
    /// only via Option; callers constructing in Rust should set explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_outcome: Option<ManifestNodeOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Directed edge between stable node ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEdge {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub from: String,
    pub to: String,
}

/// Document gate definition (Design / Plan).
///
/// Frozen wire fields: `id`, `required_reviewer_node_ids`, `resolution_mode`.
/// `gate_kind` is optional; when absent the validator infers it fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestGate {
    pub id: String,
    /// Required on the wire (no serde default). Empty only for Design self_review.
    pub required_reviewer_node_ids: Vec<String>,
    pub resolution_mode: ResolutionMode,
    /// Optional `design` | `plan`. Omitted → inferred from reviewer set / mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_kind: Option<DocumentGateKind>,
}

/// Raw published manifest document (Task 2 freeze).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDocument {
    pub schema_version: u32,
    pub workflow_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_manifest_revision: Option<u64>,
    pub publication_token: String,
    pub workflow_state: ManifestWorkflowState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<DocumentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<DocumentRef>,
    /// Required on the wire (no serde default).
    pub phases: Vec<ManifestPhase>,
    /// Required on the wire (no serde default).
    pub nodes: Vec<ManifestNode>,
    /// Required on the wire (no serde default).
    pub edges: Vec<ManifestEdge>,
    /// Required on the wire (no serde default).
    pub gates: Vec<ManifestGate>,
}

/// Normalized, validated work-unit node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedNode {
    pub id: String,
    pub kind: ManifestNodeKind,
    pub phase_id: Option<String>,
    pub role: Option<ManifestNodeRole>,
    pub agent_type: Option<String>,
    pub profile_id: Option<String>,
    pub task_index: Option<u32>,
    pub work_unit_key: Option<String>,
    pub deps: Vec<String>,
    pub required: bool,
    pub node_outcome: Option<ManifestNodeOutcome>,
    pub title: Option<String>,
}

/// Normalized, validated gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedGate {
    pub id: String,
    pub required_reviewer_node_ids: Vec<String>,
    pub resolution_mode: ResolutionMode,
    pub gate_kind: DocumentGateKind,
}

/// Output of `validate_manifest_document`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedManifest {
    pub schema_version: u32,
    pub workflow_kind: String,
    pub workflow_id: Option<String>,
    pub expected_manifest_revision: Option<u64>,
    pub publication_token: String,
    pub workflow_state: ManifestWorkflowState,
    pub design: Option<DocumentRef>,
    pub plan: Option<DocumentRef>,
    pub phases: Vec<ManifestPhase>,
    pub nodes: Vec<NormalizedNode>,
    pub edges: Vec<ManifestEdge>,
    pub gates: Vec<NormalizedGate>,
    /// Distinct Task indices present on work units (1-based).
    pub task_count: usize,
}
