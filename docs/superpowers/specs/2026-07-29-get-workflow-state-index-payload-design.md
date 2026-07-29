# get_workflow_state Index Payload Design

Date: 2026-07-29
Status: Draft for review

## Problem

Root agents call `get_workflow_state` to recover brainstorm-to-delivery
orchestration state. The tool currently returns a full agent-facing recovery
DTO: header, task policies, every node evidence row (soft-capped at
`MAX_NODES` = 400), gates, and `latest_plan_review` including the full findings
ledger (up to 400 findings, each with multi-kilobyte text fields; plan-round
JSON may reach 512 KiB).

That payload is a single MCP `tools/call` JSON-RPC result (and a single broker
frame on the companion path). Large graphs and multi-finding Plan rounds push
the serialized response far past host-safe sizes. Observed failure mode is
**transport / stdio frame stall**: the host may hang, timeout, or mishandle a
very large JSONL response. The same class of host behavior already forced a
**7,680 UTF-8 byte** budget for Grok's `tools/list` line (8,192 split boundary
minus 512 bytes headroom).

Existing mitigations are insufficient:

- Node soft truncation (`evidence_truncated`) still allows hundreds of rich
  node objects.
- Broker `MAX_FRAME_BYTES` is 16 MiB — a safety ceiling, not a product budget.
- `check_user_feedback` bounds batches; `get_session_info` returns compact
  previews with `max_messages`. `get_workflow_state` has neither pattern.

Agents that only need gate/node decisions should not download finding prose or
full policy narratives. Those bodies already live on disk (`plan` /
`report_file` / `evidence_ref`) or behind other tools
(`get_session_info`, `get_delegation_status`).

## Goals

- Make `get_workflow_state` an **index / navigation** tool by default, in the
  spirit of `get_session_info` (metadata + bounded pointers, not full bodies).
- Enforce a **hard UTF-8 byte budget of 7,680** on the serialized MCP tool
  result payload (the JSON value returned as the tool result content after
  companion packaging — see Measurement below), matching the established Grok
  stdio-safe constant used for `tools/list`.
- Preserve enough state for parent orchestration: CAS revisions, gate cycles /
  outcomes, which nodes are required, latest task ids, plan/design paths and
  digests (or short prefixes), and plan-review **counts + next_action**.
- Point the agent at workspace files and narrower tools for prose and run
  detail; never require a second `get_workflow_state` full dump for finding
  text.
- Keep publish / settle validation and the frontend redacted graph snapshot
  contracts unchanged.
- Add regressions that fail if a representative large fixture serializes over
  7,680 bytes after projection, and that still assert critical recovery fields.

## Non-Goals

- Treating 7,680 as an MCP protocol limit for all servers or all tools.
- Changing Grok's reader, stdio writer framing, or broker length-prefix format.
- Returning full finding summaries inline under any default path.
- Adding a new multi-round “page findings” MCP tool in this design.
- Changing `publish_workflow_manifest` / `settle_workflow_gate` schemas or
  admission rules.
- Replacing frontend `WorkflowGraphSnapshot` with the agent DTO.
- Compressing tool **descriptions** (already covered by tools/list budget work).

## Approaches Considered

### A. Detail layering only (`summary` vs `recovery`)

Optional `detail` without a hard byte cap. Improves the common case but cannot
honestly guarantee transport safety on large graphs.

### B. Always-full payload + post-serialize truncation

Single schema; drop fields when over budget. Agents cannot predict which
fields survive; recovery guidance becomes unreliable.

### C. Index-only projection + hard 7,680 budget (selected)

Default (and only v1) response is a compact index. Finding **bodies** are never
inlined. Optional finding **stubs** (id, severity, status, paths) may appear
until the budget forces them off. Post-serialize progressive omission with
explicit `payload_truncated` / `omitted` keeps the frame under 7,680 even for
pathological graphs.

Selected because it matches the user's transport failure mode, the
`get_session_info` “brief + read sources” pattern, and the existing 7,680 host
budget discipline.

## Design

### Measurement and constant

Define a shared constant for the product budget (name may live next to the
tools/list budget or in the workflow recovery module):

```text
GET_WORKFLOW_STATE_MAX_RESULT_BYTES = 7_680
```

**What is measured:** UTF-8 length of `serde_json::to_vec` on the **agent-facing
outcome object** returned from `get_workflow_state` (the DTO / projected
`Value` before optional MCP content-text wrapping). If the companion wraps the
object as MCP `content: [{ type: "text", text: <json> }]`, the implementation
MUST either:

1. measure the final text body that agents receive, or  
2. measure the DTO and reserve headroom so the wrapped form stays ≤ 7,680.

Preferred: measure the **final MCP tools/call result JSON-RPC line** (compact
JSON + newline) when that is what hosts read as one frame; otherwise measure
the text body of the tool result. The regression suite pins one definition and
documents it in the test name.

The budget equals the Grok `tools/list` host-safe line budget so one mental
model covers “stdio-safe codeg-mcp messages” for list **and** this recovery
read. It is still a Codeg product mitigation, not an MCP specification limit.

### Input contract

Keep `workflow_id` optional (parent + kind resolution when omitted).

Optional input (v1):

| Field | Type | Default | Notes |
|-------|------|---------|--------|
| `detail` | string enum | `"index"` | Only `"index"` is valid in v1. Unknown values → tool error. |

No `include_findings` full-body switch in v1. Bodies stay on disk.

### Output contract (`detail = "index"`)

#### Always attempt to include (priority 0 — drop only if still over budget after everything else)

- `workflow_id`
- `parent_conversation_id`
- `workflow_kind`
- `capability_version`
- `workflow_state`
- `manifest_revision`
- `graph_revision`
- `schema_version`
- `plan_target_rel_path`
- `design` / `plan` as path + digest (digest may be shortened under pressure;
  see omission ladder)
- `gates[]` with: `gate_id`, `gate_kind`, `resolution_mode`,
  `required_reviewer_node_ids`, `latest_gate_cycle`, `latest_outcome`,
  `next_gate_cycle`
- `detail`: `"index"`
- `inline_findings`: `false`
- `payload_truncated`: bool
- `omitted`: string array (stable tokens, only when truncated)

#### Plan review summary (priority 1)

- `next_action`
- `critical_count` / `important_count` / `minor_count`
- `stagnation_count`, `rewrite_used`, `net_improvement` (if already stored)
- `covered_author_task_id`, `covered_plan_digest` (shorten digest if needed)
- `next_required_reviewer_node_ids`
- **No** finding `summary` text fields

#### Finding stubs (priority 2 — first to shrink under budget)

Optional array under `latest_plan_review.findings` **without** `summary`:

```json
{
  "finding_id": "F1",
  "severity": "critical",
  "status": "open",
  "report_file": "docs/.../review.md",
  "evidence_ref": "docs/.../plan.md#L40"
}
```

Prefer open critical/important stubs first. Soft cap before serialize
(e.g. ≤ 8 stubs). Further reduction via the omission ladder.

#### Nodes (priority 3)

Each node is a **skeleton**, not today's full `WorkflowNodeStateDto`:

| Include | Omit by default |
|---------|-----------------|
| `node_id`, `role`, `agent_type`, `phase_id` | long `work_unit_key` if budget tight (keep when required for admission debugging only if under budget) |
| `latest_status`, `latest_task_id` | full generation / replacement chains |
| `required_for_gate` | redundant flags that do not affect routing |
| `child_conversation_id` | — |
| `verdict`, `report_file` | long free-text |
| `artifact_digest` (may shorten) | — |

Prefer keeping: required-gate nodes, active-manifest work units, non-terminal
nodes. Soft cap before serialize (e.g. ≤ 24 nodes), then ladder drops more.
Set `evidence_truncated: true` when any node evidence row is dropped (reuse
existing flag semantics where possible).

#### Task policies (priority 4)

- Keep `risk_policy_version`.
- At most a compact list `{ "task_index", "level" }` with no long reason
  strings, or omit the list entirely under budget (record in `omitted`).

### Post-serialize omission ladder

After building the preferred index object, serialize. While
`bytes > GET_WORKFLOW_STATE_MAX_RESULT_BYTES`, apply the next step and
re-serialize. Set `payload_truncated = true` and append to `omitted` when a
step removes data:

1. Drop all finding stubs (`omitted: "plan_findings"`).
2. Drop non-required completed node skeletons (oldest first)
   (`omitted: "completed_node_evidence"`).
3. Drop compact task policy list (`omitted: "task_policies"`).
4. Shorten digests to a fixed prefix (e.g. 16 hex chars) and mark
   `omitted: "full_digests"` if full digests were replaced.
5. Drop non-required terminal nodes until only required + non-terminal remain.
6. Collapse `nodes` to required / non-terminal only with minimal fields
   (`node_id`, `role`, `latest_status`, `latest_task_id`, `required_for_gate`).
7. Last resort: header + gates + plan path + plan-review counts/`next_action`
   only (`omitted: "node_index"`).

If step 7 still exceeds 7,680 (should be unreachable with normal ids/paths),
return a typed tool error `payload_too_large` rather than an incomplete
silent frame. Log sizes for diagnostics.

### Agent secondary fetch (no new tools)

Document in the tool description (compressed style, tools/list budget aware):

1. Plan/design body → read `plan_target_rel_path` / design `rel_path`.
2. Finding narrative → read each stub's `report_file` / `evidence_ref`.
3. Child transcript → `get_session_info` with `child_conversation_id` and
   bounded `max_messages`.
4. Run outcome → `get_delegation_status` for `latest_task_id`s of interest.

Root settle evidence continues to be assembled from files and child runs, not
from inlined finding prose in `get_workflow_state`.

### Compatibility

- **Breaking for agents** that assume `latest_plan_review.findings[].summary`
  is always present. Update brainstorm-to-delivery / recovery skill text in
  the same change set or an immediate follow-up commit so skills do not
  require inline summaries.
- Field names for the index skeleton may omit optional full-DTO keys via
  `skip_serializing_if`; do not rename existing gate/header keys without need.
- Frontend HTTP graph snapshot is out of scope and remains redacted / separate.

### Implementation sketch (for planning, not this doc's delivery)

Primary touch points (expected):

- `workflow/state_dto.rs` — index projection types or serde-friendly slim DTOs
- `workflow/store.rs` — `get_workflow_state_core` builds full internal state then
  projects; or projects in one pass
- `companion.rs` / tool result rendering — enforce budget at the boundary if
  projection is shared
- `tool_schema.json` — short description of index-only + file follow-up
- Tests: large fixture ≤ 7,680; critical fields present after forced omission;
  no finding `summary` keys in default output

### Skill / prompt updates

Any skill that tells the root to “call get_workflow_state and use finding
summaries” must switch to: use counts + paths; `read` report files before
settle. Keep skill text short so tools/list / skill token budgets stay healthy.

## Error And Truncation Behavior

| Case | Behavior |
|------|----------|
| Workflow missing / cross-parent | Existing typed errors unchanged |
| `detail` other than `index` | Invalid params error |
| Soft node/finding caps applied | `evidence_truncated` and/or `payload_truncated` as specified |
| Hard budget ladder applied | `payload_truncated: true`, `omitted` lists steps |
| Unreachable size after last resort | `payload_too_large` tool error |

Truncation must never invent gate outcomes or revise CAS numbers. Prefer
dropping optional evidence over corrupting authoritative header/gate fields.

## Tests

1. **Budget regression:** synthetic or fixture graph with ≥ 20 nodes and ≥ 15
   finding stubs projects to ≤ 7,680 UTF-8 bytes under the pinned measurement
   definition (include newline if measuring JSON-RPC lines).
2. **No inline summaries:** default output JSON has no
   `latest_plan_review.findings[].summary` (and no other multi-KB prose
   fields from the old full DTO).
3. **Orchestration fields survive:** for a mid-plan-review fixture,
   `manifest_revision`, `graph_revision`, plan gate `next_gate_cycle`,
   `next_action`, and severity counts remain present after projection.
4. **Omission ladder:** force an oversized path list / many nodes; assert
   `payload_truncated` and that `omitted` is non-empty while gates remain.
5. **Invalid detail:** rejected without broker mutation.
6. **Cross-parent / not found:** existing tests still pass.

## Acceptance Criteria

1. Default `get_workflow_state` responses for representative B2D recovery
   fixtures serialize to **≤ 7,680 UTF-8 bytes** under the documented
   measurement definition.
2. Finding **bodies** are never returned; agents recover prose via workspace
   files or other tools.
3. A parent agent can decide the next gate action (who to dispatch / whether
   to settle) from the index alone when digests and task ids are current.
4. Over-budget pathological inputs either truncate with explicit flags or
   return `payload_too_large` — never hang the stdio/UDS path on multi-hundred
   KB recovery dumps.
5. Focused Rust tests encode the 7,680 contract so regressions fail in CI.
6. Tool description remains short enough not to reopen the tools/list budget
   problem for Grok.

## Residual Risks

- Pathologically long absolute paths in `report_file` can still pressure the
  budget; the ladder must drop stubs before dropping gates.
- Skills not updated in lockstep will look for missing summaries and may
  mis-settle; treat skill updates as required companion work.
- Measuring DTO vs wrapped MCP line incorrectly could “pass” tests while hosts
  still see > 7,680; pin one definition in tests.
- Ultra-compact last-resort payloads may omit `child_conversation_id`; agents
  then use `latest_task_id` + `get_delegation_status` only.

## Open Follow-Ups (out of v1)

- Optional `detail=recovery` with a higher budget for non-stdio hosts only —
  deferred; v1 is index-only for all agents to keep one contract.
- Content-addressed short path aliases — not required if stubs are capped.
- Align other large tool results to the same 7,680 product budget — separate
  designs if needed.
