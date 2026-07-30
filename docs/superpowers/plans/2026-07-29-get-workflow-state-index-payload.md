# get_workflow_state Index Payload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the full `get_workflow_state` recovery dump with a deterministic navigation index whose actual newline-terminated companion JSON-RPC response is never larger than 7,680 UTF-8 bytes.

**Architecture:** The workflow store will continue loading one transactionally consistent durable snapshot, then project it into a compact index plus private omission metadata. The companion will validate the tool-specific input and request-id boundary before inflight registration, render only `content[0].text`, apply the workflow-owned omission ladder against the real request id, and serialize the exact bytes later written to stdout. The frontend graph projection and publish/settle contracts remain independent and unchanged.

**Tech Stack:** Rust 2021, SeaORM + SQLite, serde/serde_json, Tokio, length-prefixed companion broker transport, MCP JSON-RPC over newline-delimited stdio, Node.js contract tests for the bundled B2D Skill.

## Global Constraints

- Approved Design baseline: `docs/superpowers/specs/2026-07-29-get-workflow-state-index-payload-design.md`, SHA-256 `32cc07a239e5deec70b3f24454f6b6dbc77c7e910641b1abc4b7701cab06558b`. Do not modify that Design.
- Workflow id is `068d06a4-e4b5-4b70-9c29-4ff176a67746`. The configured independent Plan reviewer cohort is Codex profile `none` plus Grok profile `none`; both review this Plan before delivery execution.
- Keep Task execution serial. Task 1 defines projection interfaces, Task 2 activates them in the store/broker path, Task 3 owns companion framing, and Task 4 updates the public catalog and B2D recovery instructions.
- Follow RED-GREEN-REFACTOR. Observe every focused test fail for the intended missing behavior before adding production code.
- `GET_WORKFLOW_STATE_MAX_RESULT_BYTES` is exactly `7_680`; measure compact `serde_json::to_vec(JsonRpcResponse)` plus exactly one trailing `b'\n'`.
- `GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES` is exactly `256`; measure compact `serde_json::to_vec(&id)` after identifying `tools/call` + `get_workflow_state` and before inflight registration, cancellation metadata, or broker contact.
- A request id larger than 256 serialized bytes returns JSON-RPC `-32600` with `id: null`, never echoes the oversized id, never mutates inflight state, and produces a newline-terminated line no larger than 7,680 bytes.
- `detail` is optional and defaults to `"index"`. The only accepted explicit value is the string `"index"`; explicit `null`, another string, an array, object, number, or boolean returns JSON-RPC `-32602` without broker mutation.
- Successful `get_workflow_state` results contain the compact index JSON in `content[0].text`, set `isError: false`, and omit `structuredContent`. Other tools retain their existing renderers and structured results.
- Finding bodies and `summary` fields are never present in the index. Recovery prose comes from workspace-relative `report_file`/document paths; transcript and run detail come from `get_session_info` and `get_delegation_status`.
- Pre-cap at `INDEX_MAX_FINDING_STUBS = 4` and `INDEX_MAX_NODES = 12`. These are deterministic selection bounds, not substitutes for the 7,680-byte response limit.
- Node pre-cap ordering is exactly `(!required_for_gate, !in_actionable_route, terminal, !active_manifest_work_unit, task_index_or_u32_max, Reverse(evidence_time_or_min), node_id)` in ascending tuple order.
- For both pre-cap ranking and omission, `terminal` means exactly `latest_status` is one of the serialized strings `"completed"`, `"failed"`, or `"canceled"`; every other value, including `None`, is non-terminal.
- Compact `actionable_task_routes` are navigation/review metadata in both `ManifestWorkflowState::Estimated` and `ManifestWorkflowState::Approved`. Their presence does not admit Task execution: the existing approval, gate, and serial-admission rules remain unchanged. `Skeleton` and `Blocked` responses have no actionable routes.
- Omission tokens are unique and ordered exactly: `plan_findings`, `terminal_node_evidence`, `task_policies`, `full_digests`, `evidence_refs`, `non_required_work_unit_keys`, `non_actionable_node_index`, `node_index`.
- Digest shortening uses exactly the first 16 hexadecimal characters of the digest payload while preserving an existing `sha256:` prefix. Never shorten, absolutize, or silently truncate workspace-relative paths or identifiers.
- When open Plan findings exist, a successful response retains at least one usable `report_file` or `latest_task_id`. Protected gates, full Plan reviewer cohort, Plan counts/`next_action`, recovery sources, and actionable routes are never removed; if the protected minimum cannot fit, return a typed MCP tool result with `error.code = "payload_too_large"`.
- Keep `publish_workflow_manifest`, `settle_workflow_gate`, their admission rules, `BrokerGetWorkflowStateRequest`, the 16 MiB broker frame ceiling, and the frontend `WorkflowGraphSnapshot` contract unchanged.
- Repeated projection and rendering of the same durable state and request id must be byte-for-byte deterministic. Truncation must not alter CAS revisions, gate cycles, gate outcomes, task routes, or reviewer cohorts.
- The bundled `.agents/skills/brainstorm-to-delivery/SKILL.md` remains below 500 lines and must teach index recovery in the same change set. It must not assume inline finding summaries, full Task risk evidence, replacement chains, or all historical nodes.
- Use the existing Rust 2021 style and run Rust commands from `src-tauri/`. Use PowerShell syntax for all command examples. Stage only files owned by the current Task and create local commits only; do not merge, push, or open a PR.

## File Structure

| File | Responsibility in this change |
| --- | --- |
| `src-tauri/src/acp/delegation/workflow/state_dto.rs` | Public compact index types, broker-private omission metadata, deterministic pre-cap projection, protected-minimum validation, and one-step omission operations. |
| `src-tauri/src/acp/delegation/workflow/mod.rs` | Re-export only the index/projection interfaces needed by store, listener, and companion. |
| `src-tauri/src/acp/delegation/workflow/store.rs` | Load the durable full snapshot, evaluate authoritative Task routes from manifest policies and run bindings, build recovery sources, and return one pre-capped preferred index. |
| `src-tauri/src/acp/delegation/listener.rs` | Preserve typed store errors while serializing that single preferred index for `GetWorkflowState`. |
| `src-tauri/src/acp/delegation/companion.rs` | Validate `detail` and request-id size, render the tool-specific result, measure the actual response line, drive omission, and emit bounded typed errors. |
| `src-tauri/src/bin/codeg_mcp.rs` | Use the same compact JSON-RPC-line serializer as budget enforcement for the actual stdout write. |
| `src-tauri/src/acp/delegation/tool_schema.json` | Advertise index-only input and concise secondary-fetch guidance without breaking the existing tools/list budget. |
| `.agents/skills/brainstorm-to-delivery/SKILL.md` | Make counts, routes, gates, and file/tool pointers authoritative during recovery. |
| `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs` | Enforce the new recovery vocabulary structurally. |
| `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs` | Mutation tests proving the validator rejects stale full-payload recovery instructions. |

## Task Routing Matrix

| Task index | title | files/modules | hard-trigger evidence | soft-signal evidence with distinct scores | soft total | final risk level and reason | implementer agent | reviewer agents | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Define the deterministic index and omission contract | `workflow/state_dto.rs`, `workflow/mod.rs` | `public_compatibility`: introduces the exact serialized replacement DTO, finding stubs, omission tokens, and absence rules that supersede the full agent recovery shape | `shared_interface=1`: the typed projection is consumed by both the store and companion; no other soft signal is counted | 1 | **high** because the `public_compatibility` hard trigger fixes a breaking serialized contract before activation | Codex implementer, profile `none` | independent Codex reviewer, profile `none`; independent Grok reviewer, profile `none` | `b2d_task_risk_v1` |
| 2 | Project durable store state into the authoritative index | `workflow/store.rs`, `workflow/listener.rs` | `public_compatibility`: changes `get_workflow_state_core` and the broker outcome from full policies/findings/node history to the compact index contract | `cross_runtime_or_process=2`: the listener serializes the projection across the companion broker; `broad_production_surface=1`: every v2 workflow recovery call changes shape; `multiple_ownership_modules=1`: workflow store and listener wire code change; `shared_interface=1`: manifest routes and broker projection are shared authorities | 5 | **high** because the hard trigger removes compatibility fields and the distinct soft score is also 5 | Codex implementer, profile `none` | independent Codex reviewer, profile `none`; independent Grok reviewer, profile `none` | `b2d_task_risk_v1` |
| 3 | Enforce the real companion JSON-RPC line budget | `delegation/companion.rs`, `bin/codeg_mcp.rs` | `public_compatibility`: changes MCP success packaging and JSON-RPC validation/error semantics; `concurrency_lifecycle`: oversized ids and invalid detail must be rejected before inflight registration and cancellation state exists | `cross_runtime_or_process=2`: broker values become stdio MCP frames; `broad_production_surface=1`: every accepted recovery response passes this renderer; `multiple_ownership_modules=1`: library dispatch and binary stdout writer change; `shared_interface=1`: one serializer is shared by measurement, regression tests, and the writer | 5 | **high** because two hard triggers govern the host-facing response and inflight lifecycle | Codex implementer, profile `none` | independent Codex reviewer, profile `none`; independent Grok reviewer, profile `none` | `b2d_task_risk_v1` |
| 4 | Update tool and Skill recovery compatibility | `tool_schema.json`, B2D `SKILL.md`, validator library/tests | `public_compatibility`: changes the advertised tool input/secondary-fetch contract and the orchestrator instructions that consume the breaking response | `cross_runtime_or_process=2`: catalog guidance crosses MCP host boundaries; `broad_production_surface=1`: all B2D recovery flows use the Skill; `multiple_ownership_modules=1`: Rust catalog and bundled Skill package change; `shared_interface=1`: schema and Skill must agree on pointer semantics | 5 | **high** because the hard trigger updates public consumers and stale instructions could mis-settle a workflow | Codex implementer, profile `none` | independent Codex reviewer, profile `none`; independent Grok reviewer, profile `none` | `b2d_task_risk_v1` |

If implementation evidence expands a Task beyond its row before admission, return the Plan to its Codex Author for a material revision and complete Codex+Grok Plan re-review. Never change a frozen route inside an implementation prompt.

---

### Task 1: Define the Deterministic Index and Omission Contract

**Required Skills:**

- `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/state_dto.rs:1-120`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs:44-48`
- Test: `src-tauri/src/acp/delegation/workflow/state_dto.rs` inline `tests` module

**Interfaces:**

- Consumes: existing `WorkflowStateDto`, `WorkflowNodeStateDto`, `WorkflowGateStateDto`, `PlanReviewRoundState`, `ManifestTaskPolicy`, `TaskRiskLevel`, and `DocumentRef`.
- Produces:

```rust
pub const INDEX_MAX_NODES: usize = 12;
pub const INDEX_MAX_FINDING_STUBS: usize = 4;
pub const DIGEST_PREFIX_HEX_CHARS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStateIndexDto {
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub workflow_kind: String,
    pub capability_version: String,
    pub publication_token: String,
    pub workflow_state: ManifestWorkflowState,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub schema_version: u64,
    pub plan_target_rel_path: String,
    pub risk_policy_version: String,
    pub detail: WorkflowStateDetail,
    pub inline_findings: bool,
    pub payload_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
    pub evidence_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<DocumentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<DocumentRef>,
    pub gates: Vec<WorkflowGateStateDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_plan_review: Option<PlanReviewIndexDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<WorkflowNodeIndexDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_policies: Vec<TaskPolicyIndexDto>,
    pub actionable_task_routes: Vec<ActionableTaskRouteDto>,
    #[serde(rename = "_codeg_omission_state")]
    pub omission_state: WorkflowIndexOmissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStateDetail { Index }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPolicyIndexDto {
    pub task_index: u32,
    pub level: TaskRiskLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionableTaskRouteDto {
    pub task_index: u32,
    pub level: TaskRiskLevel,
    pub implementer_node_id: String,
    pub reviewer_node_ids: Vec<String>,
}

pub fn project_workflow_state_index(
    state: WorkflowStateDto,
    active_manifest_node_ids: &HashSet<String>,
    task_gate_passed: &BTreeMap<u32, bool>,
) -> WorkflowStateIndexDto;

impl WorkflowStateIndexDto {
    pub fn public_value(&self) -> Result<Value, serde_json::Error>;
    pub fn apply_omission_step(&mut self, step: WorkflowIndexOmissionStep) -> bool;
    pub fn validate_protected_minimum(&self) -> Result<(), WorkflowIndexProtectedError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowIndexOmissionState {
    pub nodes: Vec<WorkflowIndexNodeOmissionMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowIndexNodeOmissionMeta {
    pub node_id: String,
    pub evidence_time: Option<DateTime<Utc>>,
    pub active_manifest_work_unit: bool,
    pub in_actionable_route: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowIndexOmissionStep {
    PlanFindings,
    TerminalNodeEvidence,
    TaskPolicies,
    FullDigests,
    EvidenceRefs,
    NonRequiredWorkUnitKeys,
    NonActionableNodeIndex,
    NodeIndex,
}

impl WorkflowIndexOmissionStep {
    pub const ALL: [Self; 8] = [
        Self::PlanFindings,
        Self::TerminalNodeEvidence,
        Self::TaskPolicies,
        Self::FullDigests,
        Self::EvidenceRefs,
        Self::NonRequiredWorkUnitKeys,
        Self::NonActionableNodeIndex,
        Self::NodeIndex,
    ];

    pub fn token(self) -> &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowIndexProtectedError {
    MissingOpenFindingRecoveryPointer,
}
```

`WorkflowIndexOmissionState` is a private annotation on the single pre-capped broker index and `public_value()` serializes `self` to `serde_json::Value`, removes the top-level `_codeg_omission_state` key, and returns that public value for `content[0].text`. It stores each retained node's `evidence_time`, `active_manifest_work_unit`, and `in_actionable_route` flags so the companion can perform the exact ladder after the cross-process hop without carrying the old full DTO. `WorkflowIndexOmissionStep` has eight variants in the Global Constraints order and exposes `token() -> &'static str`.

Reuse `WorkflowGateStateDto` without renaming or dropping any field: `gate_id`, `gate_kind`, `resolution_mode`, `reviewer_cohort_node_ids`, `required_reviewer_node_ids`, `latest_gate_cycle`, `latest_outcome`, and `next_gate_cycle`. This is the protected full Plan cohort and gate-cycle contract.

- [ ] **Step 1: Write failing serialization and pre-cap tests**

Add a `sample_full_state(node_count, finding_count) -> WorkflowStateDto` fixture in `ManifestWorkflowState::Estimated` with 20 nodes, 15 findings, two Plan reviewers, two Task policies/routes, deterministic timestamps, full 64-hex digests, and multi-kilobyte finding summaries. Its route entries prove Estimated recovery/navigation metadata is populated while no test treats that state as implementation admission. Add tests named:

```rust
#[test]
fn index_projection_caps_and_removes_rich_recovery_bodies() {
    let index = project_workflow_state_index(
        sample_full_state(20, 15),
        &HashSet::from(["task-1-impl".to_string(), "task-1-review-codex".to_string()]),
        &BTreeMap::from([(1, false), (2, false)]),
    );
    let json = index.public_value().unwrap();
    assert_eq!(json["detail"], "index");
    assert_eq!(json["inline_findings"], false);
    assert!(json["nodes"].as_array().unwrap().len() <= INDEX_MAX_NODES);
    assert!(json["latest_plan_review"]["findings"].as_array().unwrap().len() <= INDEX_MAX_FINDING_STUBS);
    assert!(json.pointer("/latest_plan_review/findings/0/summary").is_none());
    assert!(json.to_string().find(&"prose".repeat(1024)).is_none());
    assert_eq!(json["latest_plan_review"]["finding_total_count"], 15);
    assert_eq!(json["payload_truncated"], true);
    assert_eq!(json["omitted"][0], "plan_findings");
}

#[test]
fn node_rank_is_independent_of_input_row_order() {
    let ordered = sample_full_state(20, 15);
    let mut reversed = ordered.clone();
    reversed.nodes.reverse();
    let active = HashSet::from(["task-1-impl".to_string()]);
    let gates = BTreeMap::from([(1, false), (2, false)]);
    let a = project_workflow_state_index(ordered, &active, &gates);
    let b = project_workflow_state_index(reversed, &active, &gates);
    assert_eq!(serde_json::to_vec(&a).unwrap(), serde_json::to_vec(&b).unwrap());
    assert_eq!(a.nodes[0].node_id, "plan-reviewer-codex");
    assert!(a.evidence_truncated);
}
```

The fixture must include rank ties so the test proves newer `evidence_time` wins and `node_id` is the final tie-break. Add `node_only_pre_cap_sets_both_truncation_flags_and_reports_omission` with 14 otherwise equal nodes named `node-00` through `node-13` and at most four findings so no finding cap fires; project both normal and reversed input order, assert `payload_truncated == true`, `evidence_truncated == true`, retained ids exactly `node-00` through `node-11`, and `omitted.is_empty()`. This is the Design-consistent reporting rule: the two truncation flags report node loss at pre-cap, while `omitted` names only ladder mutations that actually ran; the node pre-cap must not pre-insert the Step 8 `node_index` token and break canonical token order.

Add `finding_pre_cap_prioritizes_non_resolved_critical_and_important` with stubs `critical-open`, `critical-resolved`, `important-open`, `important-new`, `important-reopened`, and `minor-open`. Project normal and reversed input order and assert retained ids exactly `critical-open`, `important-open`, `important-new`, `important-reopened`. Rank by the exact tuple `(primary_bucket, severity_rank, status_rank, finding_id)`, where `primary_bucket = 0` only for Critical or Important findings whose status is not `resolved`, otherwise `1`; severity ranks `critical=0`, `important=1`, `minor=2`, and status ranks `open=0`, `new=1`, `reopened=2`, `resolved=3`.

- [ ] **Step 2: Prepare default-feature Rust prerequisites in a fresh worktree**

The default Tauri feature runs `src-tauri/build.rs`, and `src-tauri/tauri.conf.json` requires `frontendDist: "../out"`. From `src-tauri/`, prepare the repository-root dependencies and static export before the first default-feature Rust RED command:

```powershell
Push-Location ..
try {
    if (-not (Test-Path -LiteralPath node_modules)) {
        pnpm install --frozen-lockfile
        if ($LASTEXITCODE -ne 0) { throw "pnpm install --frozen-lockfile failed" }
    }
    if (-not (Test-Path -LiteralPath out)) {
        pnpm build
        if ($LASTEXITCODE -ne 0) { throw "pnpm build failed" }
    }
    if (-not (Test-Path -LiteralPath node_modules -PathType Container)) {
        throw "node_modules prerequisite is missing"
    }
    if (-not (Test-Path -LiteralPath out -PathType Container)) {
        throw "out prerequisite is missing"
    }
} finally {
    Pop-Location
}
```

Expected: `pnpm install --frozen-lockfile` runs only when `node_modules/` is absent, `pnpm build` runs only when `out/` is absent, both commands succeed when invoked, and the final checks prove both directories exist. Do not proceed to RED on a dependency-install, Next export, or missing `out` failure.

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```powershell
cargo test --features test-utils state_dto::tests::index_projection_caps_and_removes_rich_recovery_bodies -- --nocapture
cargo test --features test-utils state_dto::tests::node_rank_is_independent_of_input_row_order -- --nocapture
cargo test --features test-utils state_dto::tests::node_only_pre_cap_sets_both_truncation_flags_and_reports_omission -- --nocapture
cargo test --features test-utils state_dto::tests::finding_pre_cap_prioritizes_non_resolved_critical_and_important -- --nocapture
```

Expected: compilation fails because `WorkflowStateIndexDto`, its compact child DTOs/constants, and `project_workflow_state_index` do not exist. It must not fail because Tauri cannot find the frontend `out` resource.

- [ ] **Step 4: Implement the index types and deterministic preferred projection**

Add explicit compact structs for `WorkflowNodeIndexDto`, `PlanReviewIndexDto`, `PlanFindingStubDto`, and `PlanRecoverySourceDto`. Their serialized fields are exactly:

```rust
pub struct WorkflowNodeIndexDto {
    pub node_id: String,
    pub role: String,
    pub agent_type: Option<String>,
    pub phase_id: Option<String>,
    pub task_index: Option<u32>,
    pub latest_status: Option<String>,
    pub latest_task_id: Option<String>,
    pub required_for_gate: bool,
    pub child_conversation_id: Option<i32>,
    pub verdict: Option<String>,
    pub report_file: Option<String>,
    pub artifact_digest: Option<String>,
    pub work_unit_key: Option<String>,
}

pub struct PlanFindingStubDto {
    pub finding_id: String,
    pub severity: FindingSeverity,
    pub status: FindingStatus,
    pub owner_reviewer_node_ids: Vec<String>,
    pub report_file: Option<String>,
    pub evidence_ref: Option<String>,
}

pub struct PlanRecoverySourceDto {
    pub node_id: String,
    pub report_file: Option<String>,
    pub latest_task_id: Option<String>,
    pub child_conversation_id: Option<i32>,
}

pub struct PlanReviewIndexDto {
    pub scope: PlanReviewScope,
    pub revision_kind: PlanRevisionKind,
    pub covered_author_task_id: String,
    pub covered_plan_digest: String,
    pub reviewed_reviewer_node_ids: Vec<String>,
    pub next_required_reviewer_node_ids: Vec<String>,
    pub critical_count: u32,
    pub important_count: u32,
    pub minor_count: u32,
    pub net_improvement: bool,
    pub stagnation_count: u32,
    pub rewrite_used: bool,
    pub next_action: PlanReviewNextAction,
    pub finding_total_count: u32,
    pub finding_returned_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<PlanFindingStubDto>,
    pub recovery_sources: Vec<PlanRecoverySourceDto>,
}
```

Use `#[serde(default, skip_serializing_if = "Option::is_none")]` on every optional wire field, including `agent_type` and `phase_id`, because omission Step 7 removes them. Sort finding stubs by the exact `(primary_bucket, severity_rank, status_rank, finding_id)` tuple defined in Step 1, then apply the four-stub cap. Derive recovery-source membership and order from the current normalized Plan gate's `required_reviewer_node_ids`, not historical `latest_plan_review.next_required_reviewer_node_ids`; historical round nodes/findings may only fill `report_file`, `latest_task_id`, and `child_conversation_id`, or supply the deterministic open-finding fallback when no current required reviewer has a usable pointer. Build compact policies and authoritative route entries from `ManifestTaskPolicy`, never from parsed node names.

For node projection, compute `is_terminal` only from the exact `latest_status` string set `completed|failed|canceled`. If the 12-node pre-cap drops any row, set both `payload_truncated` and `evidence_truncated`; do not add an omission token solely for this pre-cap. Project compact routes for both `Estimated` and `Approved`; route entries in `Estimated` are recovery/navigation metadata and do not weaken the existing Task admission checks.

Do not copy `PlanReviewRoundState.scope_reason`, `lineage_reset_reason`, finding `summary`, full hard/soft risk evidence, risk reason strings, profile ids, generation/replacement chains, `reviewed_task_id`, or redundant observation/freeze flags into the public index.

- [ ] **Step 5: Write failing omission-ladder and protected-minimum tests**

Add one test that clones the same projection and applies all steps, and one test for the open-finding pointer invariant:

```rust
#[test]
fn omission_ladder_is_ordered_idempotent_and_preserves_authority() {
    let mut index = project_workflow_state_index(
        sample_full_state(12, 4),
        &HashSet::from(["task-1-impl".to_string()]),
        &BTreeMap::from([(1, false), (2, false)]),
    );
    let original_gate = index.gates[0].clone();
    let original_route = index.actionable_task_routes[0].clone();
    for step in WorkflowIndexOmissionStep::ALL {
        index.apply_omission_step(step);
        index.apply_omission_step(step);
    }
    assert_eq!(index.omitted, vec![
        "plan_findings", "terminal_node_evidence", "task_policies",
        "full_digests", "evidence_refs", "non_required_work_unit_keys",
        "non_actionable_node_index", "node_index",
    ]);
    assert_eq!(index.gates[0], original_gate);
    assert_eq!(index.actionable_task_routes[0], original_route);
    assert_eq!(index.design.as_ref().unwrap().digest, "sha256:0123456789abcdef");
    assert!(index.nodes.is_empty());
}

#[test]
fn open_findings_require_a_recovery_report_or_task() {
    let mut index = project_workflow_state_index(
        sample_full_state(12, 4),
        &HashSet::from(["task-1-impl".to_string()]),
        &BTreeMap::from([(1, false), (2, false)]),
    );
    index.latest_plan_review.as_mut().unwrap().recovery_sources.clear();
    assert_eq!(
        index.validate_protected_minimum(),
        Err(WorkflowIndexProtectedError::MissingOpenFindingRecoveryPointer)
    );
}

#[test]
fn terminal_predicate_and_step_two_cover_all_three_statuses() {
    let mut index = projected_status_fixture([
        ("completed-node", "completed"),
        ("failed-node", "failed"),
        ("canceled-node", "canceled"),
        ("running-node", "running"),
    ]);
    index.apply_omission_step(WorkflowIndexOmissionStep::TerminalNodeEvidence);
    let ids = index.nodes.iter().map(|node| node.node_id.as_str()).collect::<Vec<_>>();
    assert!(!ids.contains(&"completed-node"));
    assert!(!ids.contains(&"failed-node"));
    assert!(!ids.contains(&"canceled-node"));
    assert!(ids.contains(&"running-node"));
}
```

- [ ] **Step 6: Run omission tests and verify RED**

Run:

```powershell
cargo test --features test-utils state_dto::tests::omission_ladder_is_ordered_idempotent_and_preserves_authority -- --nocapture
cargo test --features test-utils state_dto::tests::open_findings_require_a_recovery_report_or_task -- --nocapture
cargo test --features test-utils state_dto::tests::terminal_predicate_and_step_two_cover_all_three_statuses -- --nocapture
```

Expected: compilation fails because omission variants, mutation methods, digest shortening, and protected-minimum validation are missing.

- [ ] **Step 7: Implement every one-step omission operation**

Keep `omission_state` synchronized when nodes are removed. Step 1 clears finding stubs and sets `finding_returned_count = 0`. Step 2 removes every non-required node whose `latest_status` is exactly `completed`, `failed`, or `canceled`, in ascending `(evidence_time_or_min, node_id)` order. Step 3 clears only the compact `task_policies` list, leaves `actionable_task_routes` untouched, and appends `task_policies` only when a non-empty list was cleared. Step 4 shortens only digest payloads containing more than 16 ASCII hexadecimal characters, retains an existing `sha256:` prefix, and leaves shorter/non-hex values unchanged. Step 5 clears stub `evidence_ref` before `report_file`, then clears recovery-source `report_file` only where `latest_task_id` remains; if no source has a task id, retain one deterministic recovery `report_file`. Step 6 removes `work_unit_key` only when both `required_for_gate` and `in_actionable_route` are false. Step 7 retains required, non-terminal, and actionable nodes and clears every field except `node_id`, `role`, `task_index`, `latest_status`, `latest_task_id`, `required_for_gate`, and protected/actionable `work_unit_key`. Step 8 clears the remaining `nodes` and all matching private node metadata, appends `node_index`, and preserves protected header fields, gates, Plan review metadata/counts/`next_action`, `recovery_sources`, and `actionable_task_routes`. `apply_omission_step` returns `false` and does not append a token when a step changes nothing; otherwise it sets `payload_truncated`, updates `evidence_truncated` for node loss, and appends the token once.

- [ ] **Step 8: Run Task 1 tests and format**

Run:

```powershell
cargo test --features test-utils state_dto::tests -- --nocapture
cargo fmt --all -- --check
```

Expected: all `state_dto::tests` pass; serialized index bytes are deterministic; rustfmt reports no diff.

- [ ] **Step 9: Commit Task 1**

```powershell
git add src-tauri/src/acp/delegation/workflow/state_dto.rs src-tauri/src/acp/delegation/workflow/mod.rs
git commit -m "feat(workflow): define compact recovery index"
```

---

### Task 2: Project Durable Store State into the Authoritative Index

**Required Skills:**

- `superpowers:test-driven-development`

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/store.rs:42-48,642-900,2585-2695,3650-3755,4455-4625,5740-6035,6380-6400,6755-6775`
- Modify: `src-tauri/src/acp/delegation/listener.rs:1532-1551,6336-6351`
- Test: the inline test modules in the same two Rust files

**Interfaces:**

- Consumes: Task 1 `project_workflow_state_index`, `WorkflowStateIndexDto`, existing `evaluate_execution_gate`, `evidence_from_run_and_binding`, `ExecutionGateInput`, `ExecutionGateKind::Task`, and authoritative `ManifestTaskPolicy.route` values.
- Produces:

```rust
pub async fn get_workflow_state_core(
    db: &AppDatabase,
    parent_conversation_id: i32,
    workflow_id: Option<&str>,
) -> Result<WorkflowStateIndexDto, WorkflowStoreError>;
```

`BrokerGetWorkflowStateRequest` remains `{ token: String, workflow_id: Option<String> }`. The listener's broker-private success `outcome` serializes one pre-capped `WorkflowStateIndexDto`; Task 3 removes its `_codeg_omission_state` annotation before emitting the public index to the MCP client.

For each policy, compute a full gate evaluation with the latest run/binding pair for the implementer and every configured reviewer. A Task is passed only when the existing strict gate evaluator passes. For this projection, “durable route evidence” means the current internal full-state node/run-binding evidence for an exact implementer or reviewer id from that policy route; never infer it from names. Compute routes when `workflow_state` is `Estimated` or `Approved`; return no routes for `Skeleton` or `Blocked`. The active route is the lowest numeric non-passed Task with any durable route evidence. When an active route exists, the next route is the lowest greater Task with no route evidence whose lower Tasks other than the active Task have passed; it is navigation for the next serial candidate, not permission to bypass admission. With no active route, the next route is the lowest non-passed/no-evidence Task whose lower Tasks passed. Return active first and next second only when distinct; mark nodes from the first entry as `in_actionable_route` for pre-cap ranking. An `Estimated` route exposes the same manifest authority already persisted in policy fixtures but does not make the Task executable: implementation admission still requires the existing approved workflow state and gate checks.

For `latest_plan_review.recovery_sources`, locate the current normalized Plan gate in `gates` and use that gate's `required_reviewer_node_ids` as the authoritative membership and order. A historical round's `next_required_reviewer_node_ids`, owners, and findings may fill pointer fields or provide the open-finding fallback, but must never add, remove, or reorder available current-cohort sources.

- [ ] **Step 1: Replace full-DTO recovery assertions with failing index assertions**

Rename `get_workflow_state_b4_fields` to `get_workflow_state_index_preserves_recovery_authority` and change its assertions to the compact contract:

```rust
let state = get_workflow_state_core(&db, parent, Some(&r.workflow_id)).await.unwrap();
assert_eq!(state.workflow_id, r.workflow_id);
assert_eq!(state.manifest_revision, 1);
assert_eq!(state.detail, WorkflowStateDetail::Index);
assert!(!state.inline_findings);
let design = state.nodes.iter().find(|n| n.node_id == "design-reviewer-1").unwrap();
assert_eq!(design.latest_task_id.as_deref(), Some("task-state-1"));
assert_eq!(design.latest_status.as_deref(), Some("completed"));
assert!(serde_json::to_value(state).unwrap().pointer("/nodes/0/latest_generation").is_none());
```

Rename `task4_plan_initial_round_persists_derived_state_and_full_recovery` to `task4_plan_initial_round_persists_derived_state_and_index_recovery` and assert the fixture remains `ManifestWorkflowState::Estimated`, plus counts, `next_action`, full reviewed/next-required reviewer ids, finding stubs without summaries, deduplicated recovery sources, compact policy levels, and the exact Task 1 route. Update the score-3 recovery test to assert it also remains `Estimated`, `task_policies[0] == { task_index: 1, level: High }`, and `actionable_task_routes[0]` retains the full manifest route instead of expecting hard/soft evidence prose. These routes are recovery/navigation metadata only; neither test executes a Task before approval.

- [ ] **Step 2: Add failing deterministic route and recovery-source integration tests**

Add store fixtures with two serial Tasks and high-risk dual reviewers:

```rust
#[tokio::test]
async fn index_routes_use_manifest_authority_and_durable_gate_state() {
    let fixture = seed_two_task_index_workflow().await;
    fixture.complete_task_1_implementer_only().await;
    let active = get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id)).await.unwrap();
    assert_eq!(active.actionable_task_routes.iter().map(|r| r.task_index).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(active.actionable_task_routes[0].reviewer_node_ids, vec!["task-1-review-codex", "task-1-review-grok"]);

    fixture.complete_both_task_1_reviews_against_latest_artifact().await;
    let next = get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id)).await.unwrap();
    assert_eq!(next.actionable_task_routes.iter().map(|r| r.task_index).collect::<Vec<_>>(), vec![2]);
}

#[tokio::test]
async fn index_recovery_sources_cover_each_required_plan_reviewer() {
    let fixture = seed_open_plan_findings_with_reviewer_runs().await;
    let index = get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id)).await.unwrap();
    let review = index.latest_plan_review.unwrap();
    assert_eq!(review.recovery_sources.iter().map(|s| s.node_id.as_str()).collect::<Vec<_>>(), vec!["plan-reviewer-codex", "plan-reviewer-grok"]);
    assert!(review.recovery_sources.iter().all(|s| s.report_file.is_some() || s.latest_task_id.is_some()));
}

#[tokio::test]
async fn material_republish_uses_current_plan_gate_cohort_through_omission() {
    let fixture = seed_historical_plan_round_with_required_reviewers(["plan-reviewer-old"]).await;
    fixture.materially_republish_plan_with_reviewers([
        "plan-reviewer-codex",
        "plan-reviewer-grok",
    ]).await;
    fixture.record_current_reviewer_pointers().await;

    let mut index = get_workflow_state_core(
        &fixture.db,
        fixture.parent,
        Some(&fixture.workflow_id),
    ).await.unwrap();
    let expected = vec!["plan-reviewer-codex", "plan-reviewer-grok"];
    assert_eq!(recovery_source_ids(&index), expected);
    for step in WorkflowIndexOmissionStep::ALL {
        index.apply_omission_step(step);
        assert_eq!(recovery_source_ids(&index), expected);
    }
}
```

- [ ] **Step 3: Run focused store tests and verify RED**

Run:

```powershell
cargo test --features test-utils get_workflow_state_index_preserves_recovery_authority -- --nocapture
cargo test --features test-utils task4_plan_initial_round_persists_derived_state_and_index_recovery -- --nocapture
cargo test --features test-utils index_routes_use_manifest_authority_and_durable_gate_state -- --nocapture
cargo test --features test-utils index_recovery_sources_cover_each_required_plan_reviewer -- --nocapture
cargo test --features test-utils material_republish_uses_current_plan_gate_cohort_through_omission -- --nocapture
```

Expected: compilation/assertion failures show `get_workflow_state_core` still returns `WorkflowStateDto`, full risk/finding bodies, the old 400-node truncation path, and no actionable routes or recovery sources.

- [ ] **Step 4: Build the full internal snapshot and project before leaving the read transaction**

Retain the current single SQLite read transaction. Remove `MAX_STATE_NODE_EVIDENCE` and the old `truncate_node_evidence` call so Task 1's exact 12-node rank is the only index pre-cap. Build the current rich state locally, derive `active_manifest_node_ids`, evaluate all policy gates from current `run_bindings`/`runs`, and identify the current normalized Plan gate's `required_reviewer_node_ids` before constructing recovery sources. Historical round data fills only pointer fields and the open-finding fallback. Then call:

```rust
let full_state = WorkflowStateDto {
    workflow_id: header.workflow_id,
    parent_conversation_id: header.parent_conversation_id,
    workflow_kind: header.workflow_kind,
    capability_version: header.capability_version,
    workflow_state: workflow_state_to_manifest(header.workflow_state),
    manifest_revision: header.active_manifest_revision as u64,
    graph_revision: header.graph_revision as u64,
    schema_version: header.schema_version as u64,
    publication_token: header.publication_token,
    plan_target_rel_path: normalized.plan_target_rel_path,
    risk_policy_version: normalized.risk_policy_version,
    task_policies: normalized.task_policies,
    design: normalized.design,
    plan: normalized.plan,
    nodes,
    gates,
    latest_plan_review,
    evidence_truncated: false,
};
Ok(project_workflow_state_index(
    full_state,
    &active_manifest_node_ids,
    &task_gate_passed,
))
```

The implementation must pass actual `ManifestTaskPolicy` values into projection. For gate evaluation, use each route's exact implementer id and complete reviewer id vector, `summary_validated`, `reviewed_task_id`, generation, digest, and terminal card status through existing `evidence_from_run_and_binding`; do not reduce a high Task to the single-reviewer convenience helper.

- [ ] **Step 5: Migrate all store callers without weakening coverage**

Keep direct index calls as `state.gates`, `state.latest_plan_review`, and `recovery.task_policies` after the return-type change. Preserve cross-parent, missing workflow, stale gate visibility, design-only/plan-only rewrite, current fingerprint, stagnation, and cohort tests. Delete only assertions for intentionally removed rich fields; replace them with compact counts, route identities, pointer paths, and explicit absence assertions.

- [ ] **Step 6: Add broker-private listener coverage and verify typed errors remain stable**

Extend the listener workflow test to assert its successful `outcome` has top-level `detail == "index"` plus `_codeg_omission_state`, while cross-parent/not-found still expose existing `error.code` values. This annotation is allowed only on the broker hop:

```rust
assert_eq!(outcome["detail"], "index");
assert!(outcome.get("_codeg_omission_state").is_some());
assert!(outcome.pointer("/latest_plan_review/findings/0/summary").is_none());
```

- [ ] **Step 7: Run Task 2 regression set and verify GREEN**

Run:

```powershell
cargo test --features test-utils get_workflow_state -- --nocapture
cargo test --features test-utils task4_plan_initial_round_persists_derived_state_and_index_recovery -- --nocapture
cargo test --features test-utils task4_score3_high_route_persists_and_recovers -- --nocapture
cargo test --features test-utils cross_parent_reject_on_get_and_settle -- --nocapture
cargo test --features test-utils workflow_feature_disabled_token_is_rejected -- --nocapture
cargo fmt --all -- --check
```

Expected: all tests pass; broker-private metadata exists only on the broker outcome and is not part of the later public value; full summaries and policy reasons are absent from the public index; not-found/cross-parent behavior is unchanged.

- [ ] **Step 8: Commit Task 2**

```powershell
git add src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/listener.rs
git commit -m "feat(workflow): project recovery state as index"
```

---

### Task 3: Enforce the Real Companion JSON-RPC Line Budget

**Required Skills:**

- `superpowers:test-driven-development`
- `superpowers:systematic-debugging` if any framing, cancellation, or timing regression appears

**Files:**

- Modify: `src-tauri/src/acp/delegation/companion.rs:119-157,350-442,557-844,933-1035,1830-1865,3160-3246,3365-3484`
- Modify: `src-tauri/src/bin/codeg_mcp.rs:145-158,269-308,357-373`
- Test: inline test modules in both files

**Interfaces:**

- Consumes: Task 2 broker-private `WorkflowStateIndexDto` outcome and Task 1 `public_value`/omission/protected-minimum APIs.
- Produces:

```rust
pub const GET_WORKFLOW_STATE_MAX_RESULT_BYTES: usize = 7_680;
pub const GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES: usize = 256;

pub fn serialize_jsonrpc_line(
    response: &JsonRpcResponse,
) -> Result<Vec<u8>, serde_json::Error>;

fn parse_get_workflow_state_args(
    arguments: &Value,
    token: &str,
) -> Result<BrokerGetWorkflowStateRequest, String>;

fn render_get_workflow_state_response(
    id: Value,
    index: WorkflowStateIndexDto,
) -> JsonRpcResponse;

fn render_get_workflow_state_response_with_budget(
    id: Value,
    index: WorkflowStateIndexDto,
    max_bytes: usize,
) -> Result<JsonRpcResponse, serde_json::Error>;

fn render_get_workflow_state_outcome_with_budget(
    id: Value,
    outcome: Value,
    max_bytes: usize,
) -> Result<JsonRpcResponse, serde_json::Error>;

fn render_bounded_workflow_error(
    id: Value,
    stable_code: &'static str,
) -> JsonRpcResponse;

fn workflow_state_stable_error_code(outcome: &Value) -> &'static str;

fn render_payload_too_large(id: Value) -> JsonRpcResponse;
```

The production wrapper always passes `7_680`. Every broker success or error outcome enters `render_get_workflow_state_outcome_with_budget` with the real accepted request id. A success deserializes the index and delegates to the omission helper. An error first renders the existing typed tool-error shape and measures its complete JSON-RPC line; if oversized, it emits `render_bounded_workflow_error`. Extract `stable_code` only by matching the bounded literal codes emitted by `workflow_store_error_value` (including `not_found` and `cross_parent`); map an absent, non-string, or unknown code to the fixed literal `internal_error` rather than copying untrusted code text.

The bounded fallback retains `isError: true` and `structuredContent.error.code`, but its text and structured message are fixed literals and never copy the broker message or workflow id:

```json
{
  "content": [{ "type": "text", "text": "get_workflow_state failed; inspect structuredContent.error.code" }],
  "isError": true,
  "structuredContent": {
    "error": {
      "code": "not_found",
      "message": "get_workflow_state failed"
    }
  }
}
```

The budgeted success helper serializes an `ok(id, result)` candidate with `serialize_jsonrpc_line`, applies each omission step, and repeats. It returns `ok(id, typed_error_result)` after step 8 if the protected minimum is invalid or still too large. Its success result is:

```json
{
  "content": [{ "type": "text", "text": "{\"detail\":\"index\",\"workflow_id\":\"wf-1\"}" }],
  "isError": false
}
```

The bounded last-resort tool result is:

```json
{
  "content": [{ "type": "text", "text": "get_workflow_state payload exceeds the 7680-byte response budget" }],
  "isError": true,
  "structuredContent": {
    "error": {
      "code": "payload_too_large",
      "message": "get_workflow_state protected recovery index exceeds 7680 bytes"
    }
  }
}
```

- [ ] **Step 1: Write failing input-boundary tests before changing dispatch**

Add helpers that create string ids by serialized length and tests for `detail`:

```rust
fn ascii_string_id_with_serialized_len(bytes: usize) -> Value {
    Value::String("x".repeat(bytes - 2))
}

#[tokio::test]
async fn get_workflow_state_detail_contract_rejects_before_inflight() {
    let omitted = parse_get_workflow_state_args(&json!({}), "token").unwrap();
    let explicit = parse_get_workflow_state_args(&json!({"detail":"index"}), "token").unwrap();
    assert_eq!(serde_json::to_value(omitted).unwrap(), serde_json::to_value(explicit).unwrap());

    for detail in [
        Value::Null,
        json!("full"),
        json!(1),
        json!([]),
        json!({}),
        json!(true),
        json!(false),
    ] {
        let inflight = Arc::new(InflightCalls::new());
        let line = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "get_workflow_state", "arguments": { "detail": detail } }
        }).to_string();
        let response = unwrap_respond(dispatch_line(
            &ctx_with(WORKFLOW_ROOT),
            inflight.clone(),
            &line,
        ).await);
        assert_eq!(response.error.unwrap().code, -32602);
        assert!(inflight.drain_all().await.is_empty());
    }
    assert!(matches!(dispatch_with_features(WORKFLOW_ROOT, &call(2, "get_workflow_state", json!({}))).await, LineAction::Spawn(_)));
    assert!(matches!(dispatch_with_features(WORKFLOW_ROOT, &call(3, "get_workflow_state", json!({"detail":"index"}))).await, LineAction::Spawn(_)));
}

#[tokio::test]
async fn get_workflow_state_request_id_limit_is_pre_inflight_and_bounded() {
    let accepted = ascii_string_id_with_serialized_len(256);
    let accepted_line = json!({
        "jsonrpc": "2.0", "id": accepted, "method": "tools/call",
        "params": { "name": "get_workflow_state", "arguments": {} }
    }).to_string();
    assert!(matches!(
        dispatch_with_features(WORKFLOW_ROOT, &accepted_line).await,
        LineAction::Spawn(_)
    ));

    for rejected in [
        ascii_string_id_with_serialized_len(257),
        Value::String("\\".repeat(128)),
        Value::String("界".repeat(85)),
    ] {
        let inflight = Arc::new(InflightCalls::new());
        let line = json!({
            "jsonrpc": "2.0", "id": rejected, "method": "tools/call",
            "params": { "name": "get_workflow_state", "arguments": {} }
        }).to_string();
        let response = unwrap_respond(dispatch_line(
            &ctx_with(WORKFLOW_ROOT), inflight.clone(), &line
        ).await);
        assert_eq!(response.id, Value::Null);
        assert_eq!(response.error.as_ref().unwrap().code, -32600);
        assert!(inflight.drain_all().await.is_empty());
        assert!(serialize_jsonrpc_line(&response).unwrap().len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES);
    }
}
```

- [ ] **Step 2: Run input tests and verify RED**

Run:

```powershell
cargo test --features test-utils get_workflow_state_detail_contract_rejects_before_inflight -- --nocapture
cargo test --features test-utils get_workflow_state_request_id_limit_is_pre_inflight_and_bounded -- --nocapture
```

Expected: tests fail because explicit `detail` values are ignored, all request ids reach `register_and_spawn`, and the shared JSON-RPC-line serializer/constants are absent.

- [ ] **Step 3: Add the shared serializer and pre-inflight validation**

Implement `serialize_jsonrpc_line` with compact `serde_json::to_vec` and one pushed newline. In `build_tools_call_spawn`, parse `name` first; when it is `get_workflow_state`, serialize `id` and reject values over 256 bytes with `err(Value::Null, -32600, "Invalid Request: get_workflow_state request id exceeds 256 serialized bytes")` before reading workflow arguments or calling `register`. Use `parse_get_workflow_state_args` to make omitted/`"index"` equivalent and every other present `detail` invalid.

Change `codeg_mcp::write_response` to call `serialize_jsonrpc_line`; do not keep a second `serde_json::to_vec` + newline implementation in the binary.

- [ ] **Step 4: Write the failing packaging, budget, omission, and oversize tests**

Build a representative index from at least 20 source nodes and 15 source findings before Task 1 pre-caps it. Include 4 KiB summaries (which must disappear), multibyte/escape-heavy ids, paths, and report metadata. Add the Design-named regression:

```rust
#[test]
fn get_workflow_state_index_jsonrpc_line_under_7680_bytes() {
    for id in [json!(1), json!("quote\"slash\\界") ] {
        let response = render_get_workflow_state_response_with_budget(
            id,
            representative_large_index(),
            GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        ).unwrap();
        let line = serialize_jsonrpc_line(&response).unwrap();
        assert!(line.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES, "{} bytes", line.len());
        let result = response.result.unwrap();
        assert_eq!(result["isError"], false);
        assert!(result.get("structuredContent").is_none());
        let index: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(index["manifest_revision"], 7);
        assert_eq!(index["graph_revision"], 11);
        assert_eq!(index["gates"][0]["next_gate_cycle"], 3);
        assert_eq!(index["latest_plan_review"]["next_action"], "continue_review");
        assert_eq!(index["latest_plan_review"]["important_count"], 8);
        assert!(index.pointer("/latest_plan_review/findings/0/summary").is_none());
    }
}
```

Also add the following exact tests. For each transition test, clone the same index, apply all preceding steps, measure `before`, apply the target step to a second clone, measure `after`, assert `after < before`, then pass `max_bytes = after` to the budgeted renderer and assert that only the target's next canonical token is newly present:

- `get_workflow_state_packaging_text_equals_projected_index_without_structured_copy`;
- `get_workflow_state_each_budget_transition_appends_exact_ordered_token` using `render_get_workflow_state_response_with_budget` and a budget immediately below each candidate length;
- `get_workflow_state_render_is_byte_deterministic` rendering the same state/id twice;
- `get_workflow_state_protected_oversize_returns_bounded_typed_error` with pathological workflow ids/paths and an accepted 256-byte serialized request id;
- `get_workflow_state_open_findings_never_lose_all_recovery_pointers`;
- `get_workflow_state_broker_errors_keep_existing_typed_tool_error_shape` for a small error candidate that fits unchanged;
- `get_workflow_state_oversized_missing_workflow_id_error_uses_bounded_typed_fallback`: dispatch with a short accepted JSON-RPC id and a missing workflow id whose echoed `not_found` message would exceed 7,680 bytes, then assert `isError == true`, `structuredContent.error.code == "not_found"`, absence of the long workflow id/message, and a serialized line `<= 7_680`;
- `get_workflow_state_synthetic_broker_error_uses_bounded_typed_fallback`: inject `{ "error": { "code": "persistence", "message": <16 KiB deterministic string> } }`, assert stable code `persistence`, fixed fallback text/message, no source message bytes, deterministic output, and a serialized line `<= 7_680` for an accepted 256-byte request id;
- `get_workflow_state_all_known_bounded_error_codes_fit_with_max_request_id`: render each exact matched code `risk_assessment_invalid`, `task_route_mismatch`, `validation`, `reviewer_set_mismatch`, `plan_review`, `not_found`, `cross_parent`, `stale_manifest_revision`, `stale_graph_revision`, `publication_token_mismatch`, `publication_token_conflict`, `admitted_node_identity_mutation`, `cohort_frozen`, `reviewed_task_stale`, `artifact_digest_mismatch`, `gate_not_ready`, `gate_cycle_conflict`, `execution_gate_settle_rejected`, `approval_with_open_findings`, `approval_rejected_failed_reviewer`, `summary_too_large`, `negative_finding_counts`, `parent_not_found`, `busy`, and `persistence`, plus the fallback `internal_error`, with a 256-byte serialized request id and assert each complete line is `<= 7_680`.

- [ ] **Step 5: Run renderer tests and verify RED**

Run:

```powershell
cargo test --features test-utils get_workflow_state_index_jsonrpc_line_under_7680_bytes -- --nocapture
cargo test --features test-utils get_workflow_state_packaging -- --nocapture
cargo test --features test-utils get_workflow_state_each_budget_transition -- --nocapture
cargo test --features test-utils get_workflow_state_protected_oversize -- --nocapture
cargo test --features test-utils get_workflow_state_oversized_missing_workflow_id_error -- --nocapture
cargo test --features test-utils get_workflow_state_synthetic_broker_error -- --nocapture
cargo test --features test-utils get_workflow_state_all_known_bounded_error_codes -- --nocapture
```

Expected: failures show the generic workflow renderer duplicates the entire success/error outcome in `structuredContent`, no code measures the actual id-bearing envelope, and the missing-id and synthetic broker messages exceed 7,680 bytes instead of selecting a bounded typed fallback. Each RED must be the intended missing helper/assertion, not a missing frontend `out` resource.

- [ ] **Step 6: Implement the specialized spawned-call renderer and omission loop**

Add `register_and_spawn_workflow_state` rather than changing render behavior for publish/settle. After existing cancellation suppression, pass every broker outcome, success or error, to `render_get_workflow_state_outcome_with_budget(id_for_response, outcome, GET_WORKFLOW_STATE_MAX_RESULT_BYTES)`. Do not let a `get_workflow_state` error use the unmeasured generic relay. Small typed errors may retain their current text/structured message only when their measured line fits; oversized errors use the fixed bounded fallback above. Convert an impossible serialization failure into a fixed JSON-RPC `-32603` response using the same accepted id, measure that line too, and assert the request-id bound makes it fit.

The success budget loop explicitly advances through every entry in `WorkflowIndexOmissionStep::ALL`, including no-op steps:

```rust
for step in WorkflowIndexOmissionStep::ALL {
    let response = render_get_workflow_state_response(id.clone(), index.clone());
    if serialize_jsonrpc_line(&response)?.len() <= max_bytes {
        return Ok(response);
    }
    index.apply_omission_step(step); // false is a no-op; the for-loop still advances
}

let response = render_get_workflow_state_response(id.clone(), index.clone());
if index.validate_protected_minimum().is_ok()
    && serialize_jsonrpc_line(&response)?.len() <= max_bytes
{
    return Ok(response);
}
Ok(render_payload_too_large(id))
```

This measures the preferred candidate before Step 1, advances after every no-op, measures the Step 8 result once after exhausting `ALL`, and only then validates or falls back. Measure the fixed `payload_too_large` result as an error outcome as well; the accepted-id bound and fixed literals must keep it within 7,680. Log the preferred byte count, final byte count, workflow id, and applied omission tokens to stderr through `tracing`, never stdout.

- [ ] **Step 7: Prove the actual writer uses the measured bytes**

Update `write_response_emits_one_complete_jsonl_write` to compare the recorded write byte-for-byte with `serialize_jsonrpc_line(&response)`. Add both a large workflow success and the synthetic oversized broker-error fallback; for each assert one write, one flush, trailing newline, valid JSON, exact byte equality with the measured vector, and length `<= 7_680`. The error writer case must retain `isError == true` and the stable structured code while containing neither the synthetic message nor an oversized workflow id.

Run:

```powershell
cargo test --features test-utils write_response_emits_one_complete_jsonl_write -- --nocapture
cargo test --features test-utils get_workflow_state -- --nocapture
```

Expected: PASS; the writer and budget helper share one serializer and accepted workflow-state lines stay within the fixed budget.

- [ ] **Step 8: Run companion target checks and commit Task 3**

Run:

```powershell
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
cargo fmt --all -- --check
```

Expected: all commands pass without warnings.

Commit:

```powershell
git add src-tauri/src/acp/delegation/companion.rs src-tauri/src/bin/codeg_mcp.rs
git commit -m "feat(mcp): bound workflow state responses"
```

---

### Task 4: Update Tool and Skill Recovery Compatibility

**Required Skills:**

- `superpowers:test-driven-development`
- `superpowers:writing-skills`
- system `skill-creator`

**Files:**

- Modify: `src-tauri/src/acp/delegation/tool_schema.json:255-269`
- Modify: `src-tauri/src/acp/delegation/companion.rs:2920-2945,3365-3426,3440-3484` (schema assertions only)
- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md:68-91,125-146,226-255,273-300`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:6-23,823-900`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:15-55,929-937`
- Verify/update only if metadata is stale: `.agents/skills/brainstorm-to-delivery/agents/openai.yaml`

**Interfaces:**

- Consumes: Task 3 `detail = "index"`, success-without-`structuredContent`, omission flags, `recovery_sources`, `actionable_task_routes`, and secondary fetch pointers.
- Produces this exact compact catalog entry shape:

```json
{
  "name": "get_workflow_state",
  "description": "Load compact workflow index. Read plan/design/report_file for prose; use get_session_info/get_delegation_status for child/run detail.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "workflow_id": { "type": "string" },
      "detail": { "type": "string", "enum": ["index"], "default": "index" }
    }
  }
}
```

The Skill must state that one index read supplies authoritative counts, gate state, reviewer sets, and current/next routes; the root then reads workspace `report_file` paths before settlement and uses bounded secondary tools for transcripts/run outcomes. It must explicitly reject dependence on inline finding summaries, full policy evidence/reasons, replacement chains, or complete historical node lists.

- [ ] **Step 1: Add failing schema and Skill mutation tests**

Extend `workflow_manifest_v2_schema_is_compact_and_constructible` or add `get_workflow_state_schema_describes_index_recovery`:

```rust
assert_eq!(state["inputSchema"]["properties"]["detail"]["enum"], json!(["index"]));
let description = state["description"].as_str().unwrap();
for phrase in ["compact workflow index", "report_file", "get_session_info", "get_delegation_status"] {
    assert!(description.contains(phrase), "missing {phrase}");
}
```

Add these validator requirements:

```javascript
const RECOVERY_REQUIRED = [
  [/recovery_sources/, "recovery_sources"],
  [/actionable_task_routes/, "actionable_task_routes"],
  [/report_file/, "report_file"],
  [/get_session_info/, "get_session_info"],
  [/get_delegation_status/, "get_delegation_status"],
  [/inline finding summaries/i, "inline finding summaries compatibility warning"],
]
```

Add a mutation test that removes the recovery paragraph from `baseValidSkill()` and expects all six labels to fail, while the unmodified fixture passes.

- [ ] **Step 2: Run the new compatibility tests and verify RED**

Run from the repository root:

```powershell
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
```

Run from `src-tauri/`:

```powershell
cargo test --features test-utils get_workflow_state_schema_describes_index_recovery -- --nocapture
```

Expected: the real Skill validation fails on missing `recovery_sources`/`actionable_task_routes` and the schema test fails because `detail` and secondary-fetch guidance are absent.

- [ ] **Step 3: Update the catalog without reopening the tools/list budget**

Apply the exact catalog shape above. Do not add a full schema for the output or repeat the omission ladder in the description. Keep `workflow_id` optional and do not add `include_findings` or `detail=recovery`.

- [ ] **Step 4: Rewrite only the Skill's recovery instructions**

Add this compact behavior to `Workflow capability`, Plan recovery, quick reference, and the end-to-end example without duplicating it in every section:

```markdown
Recovery is index-first. Call `get_workflow_state` with omitted `detail` or
`detail=index`; treat gate cycles/outcomes, counts + `next_action`, full Plan
cohort, `recovery_sources`, and `actionable_task_routes` as authoritative.
Finding bodies and full risk/replacement history are not inline. Before settle,
read the referenced workspace `report_file`; use `get_session_info` with a
bounded `max_messages` for a child transcript and `get_delegation_status` for
selected `latest_task_id` outcomes. Never wait for or reconstruct from inline
finding summaries or a complete historical node list.
```

Keep the existing Author ownership, routing, stagnation, strict-AND, and workspace-gate rules unchanged. Update `baseValidSkill()` with the same vocabulary so validator fixture failures stay attributable.

- [ ] **Step 5: Run Skill and catalog checks and verify GREEN**

Run from the repository root:

```powershell
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
python C:\Users\drawpeng\.codex\skills\.system\skill-creator\scripts\quick_validate.py .agents/skills/brainstorm-to-delivery
(Get-Content .agents/skills/brainstorm-to-delivery/SKILL.md).Count
```

Expected: both Node commands and `quick_validate.py` pass; line count is below 500. Leave `agents/openai.yaml` untouched unless its trigger/interface metadata is demonstrably stale.

Run from `src-tauri/`:

```powershell
cargo test --features test-utils get_workflow_state_schema_describes_index_recovery -- --nocapture
cargo test --features test-utils grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --nocapture
```

Expected: both pass and the printed Grok tools/list line remains `<= 7_680` bytes without raising the literal.

- [ ] **Step 6: Run the complete Rust verification matrix**

Run from `src-tauri/`:

```powershell
cargo fmt --all -- --check
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: every command passes. A failure in a file owned by Tasks 1-3 reopens that Task under its recorded Codex implementer plus independent Codex/Grok reviewer route; after its fix is approved, restart this matrix from the first command.

- [ ] **Step 7: Check scope, commit Task 4, and record final evidence**

Run from the repository root:

```powershell
git diff --check
git status --short
rg -n "include_findings|detail.?recovery" src-tauri/src/acp/delegation .agents/skills/brainstorm-to-delivery
```

Expected: diff check passes; status lists Task-owned files plus the pre-existing approved Design baseline modification, which remains untouched; the final search has no matches. Confirm the implementation diff does not change the Design, frontend graph DTOs, publish/settle schemas, broker framing limits, migrations, database entities, or unrelated Skill rules.

Commit:

```powershell
git add src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/companion.rs .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
git diff --quiet -- .agents/skills/brainstorm-to-delivery/agents/openai.yaml
if ($LASTEXITCODE -ne 0) { git add .agents/skills/brainstorm-to-delivery/agents/openai.yaml }
git commit -m "feat(workflow): document index recovery contract"
```

## Completion Evidence

Before delivery is declared complete, the final reviewer must be able to trace the approved Design to concrete evidence:

- representative 20-node/15-finding output and escape-heavy request id render through the actual tool-specific JSON-RPC helper at `<= 7_680` bytes including newline;
- the actual stdout writer emits exactly the same serialized byte vector used by the budget helper;
- finding summaries and other old prose fields are absent, success omits `structuredContent`, and `content[0].text` parses to the selected index;
- header/CAS fields, design/plan pointers, gate cycles/outcomes, full Plan cohort, counts/`next_action`, recovery pointers, and current/next manifest routes survive projection and every applicable omission step;
- exact pre-cap ordering, finding ordering, omission token order, 16-hex digest shortening, deterministic bytes, and protected oversize behavior are covered by focused tests;
- omitted and explicit `detail=index` agree; invalid detail and request ids over 256 serialized bytes fail before inflight/broker mutation with bounded specified errors;
- workflow missing/cross-parent typed errors, publish/settle validation, 16 MiB broker framing, and frontend `WorkflowGraphSnapshot` behavior remain unchanged;
- the catalog remains under its existing Grok tools/list 7,680-byte regression;
- the B2D Skill validator and Skill package validation pass, and recovery instructions require file/tool pointer follow-up rather than inline bodies;
- default, server, and codeg-mcp Rust check/test/clippy matrices plus `git diff --check` pass on the final Task commit.
