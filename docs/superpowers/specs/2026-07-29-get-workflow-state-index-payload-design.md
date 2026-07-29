# get_workflow_state Index Payload Design

Date: 2026-07-29
Status: Revised after Design Gate cycle 1

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
- Enforce a **hard UTF-8 byte budget of 7,680** on the compact, newline-
  terminated JSON-RPC response line actually written by the companion for the
  `tools/call`, matching the established Grok stdio-safe measurement used for
  `tools/list`.
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
inlined. Optional finding **stubs** (id, severity, status, owners, paths) and
protected recovery/route pointers provide navigation until deterministic
omission removes optional evidence. Explicit `payload_truncated` / `omitted`
keeps the frame under 7,680 even for pathological graphs.

Selected because it matches the user's transport failure mode, the
`get_session_info` “brief + read sources” pattern, and the existing 7,680 host
budget discipline.

## Design

### Measurement, packaging, and constant

Define a shared constant for the product budget (name may live next to the
tools/list budget or in the workflow recovery module):

```text
GET_WORKFLOW_STATE_MAX_RESULT_BYTES = 7_680
GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES = 256
```

**Normative measurement:** construct the successful `JsonRpcResponse` for the
actual inbound request id after `get_workflow_state`-specific rendering,
serialize it with compact `serde_json::to_vec`, append one `\n`, and measure
that byte vector. The omission loop and the regression suite MUST call the same
pure rendering/measurement helper. Runtime enforcement therefore covers
numeric or string request ids rather than relying on test-only envelope
headroom.

For this tool only, success rendering contains the compact index JSON in
`content[0].text` and **omits `structuredContent`**. Returning the complete
index in both locations is forbidden because it approximately doubles the
stdio frame. Other tools keep their current rendering contract. Tests use
request id `1` for the representative fixture and also cover an escape-heavy
string id; runtime always measures the real id received by the companion.

The accepted request-id domain is also normative. For this tool, compact
`serde_json::to_vec(&id)` must be at most
`GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES` bytes. The check runs after parsing
the method/name but before inflight registration, cancellation metadata, or
broker contact. An oversized id makes the request invalid for this product:
return compact JSON-RPC `-32600` (`Invalid Request`) with `id: null`, append
`\n`, and do not echo the oversized value. That rejection line must also be
≤ 7,680 bytes. This bounded-input rule makes both success and error guarantees
achievable; ordinary numeric, UUID, and short string ids remain unchanged.

The companion is the hard-budget owner because it is the only layer that sees
the final host frame. Workflow code owns deterministic index projection and
omission operations; the broker may carry the preferred index under its
existing 16 MiB safety ceiling. If the protected minimum cannot fit after the
last omission step, the companion renders the small typed error instead of a
partial success.

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
- `publication_token`
- `workflow_state`
- `manifest_revision`
- `graph_revision`
- `schema_version`
- `plan_target_rel_path`
- `design` / `plan` as path + digest (digest may be shortened under pressure;
  see omission ladder)
- `gates[]` with: `gate_id`, `gate_kind`, `resolution_mode`,
  `reviewer_cohort_node_ids`, `required_reviewer_node_ids`,
  `latest_gate_cycle`, `latest_outcome`, `next_gate_cycle`
- `detail`: `"index"`
- `inline_findings`: `false`
- `payload_truncated`: bool
- `omitted`: string array (stable tokens, only when truncated)

#### Plan review summary (priority 1)

- `next_action`
- `critical_count` / `important_count` / `minor_count`
- `stagnation_count`, `rewrite_used`, `net_improvement` (if already stored)
- `scope`, `revision_kind`, `reviewed_reviewer_node_ids`
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
  "owner_reviewer_node_ids": ["plan-reviewer-codex"],
  "report_file": "docs/.../review.md",
  "evidence_ref": "docs/.../plan.md#L40"
}
```

Prefer open critical/important stubs first, ordered by severity, status, then
`finding_id`. `INDEX_MAX_FINDING_STUBS = 4` is a pre-ladder upper bound, not a
size guarantee. Include `finding_total_count` and `finding_returned_count`;
pre-cap omission sets `payload_truncated` and adds `plan_findings` to
`omitted`. Further reduction uses the omission ladder.

`latest_plan_review.recovery_sources` is separate from optional stubs. Keep a
deduplicated pointer for each currently required reviewer when available:
`node_id`, `report_file`, `latest_task_id`, and `child_conversation_id`.
When findings are open, every successful response must retain at least one
usable `report_file` or `latest_task_id`; otherwise return
`payload_too_large`. This remains true after all omission steps.

#### Nodes (priority 3)

Each node is a **skeleton**, not today's full `WorkflowNodeStateDto`:

| Include | Omit by default |
|---------|-----------------|
| `node_id`, `role`, `agent_type`, `phase_id`, `task_index` | full generation / replacement chains |
| `latest_status`, `latest_task_id` | full generation / replacement chains |
| `required_for_gate` | redundant flags that do not affect routing |
| `child_conversation_id` | — |
| `verdict`, `report_file` | long free-text |
| `artifact_digest` (may shorten), `work_unit_key` | — |

Prefer keeping: required-gate nodes, active-manifest work units, non-terminal
nodes, and nodes in the current actionable Task route. `INDEX_MAX_NODES = 12`
is a pre-ladder upper bound, not a size guarantee. Then the ladder drops more.
Set `evidence_truncated: true` when any node evidence row is dropped (reuse
existing flag semantics where possible). On retained nodes, keep
`work_unit_key` until the explicit work-unit-key omission step; never infer
Task identity from a node name.

Pre-cap selection uses this exact ascending rank tuple, independent of database
row order: `(!required_for_gate, !in_actionable_route, terminal,
!active_manifest_work_unit, task_index_or_u32_max,
Reverse(evidence_time_or_min), node_id)`. Thus required, actionable,
non-terminal, and active-manifest rows win in that order; newer evidence wins
within equal categories, and `node_id` is the final tie-break.

#### Task policies (priority 4)

- Keep `risk_policy_version`.
- A compact policy list may contain `{ "task_index", "level" }` without long
  reason strings and may be omitted under budget.
- A separate protected `actionable_task_routes` list retains the admitted Task
  (if active) and the next serially admissible Task, selected by numeric Task
  order and durable terminal/admission state. Each entry contains
  `{ task_index, level, implementer_node_id, reviewer_node_ids }`. It is
  authoritative manifest routing, not a reconstruction from node ids. If an
  actionable route cannot fit, return `payload_too_large`.

### Post-serialize omission ladder

After building the preferred index object, measure it with the normative
Measurement helper (`bytes` is the complete JSON-RPC line plus `\n`). While
`bytes > GET_WORKFLOW_STATE_MAX_RESULT_BYTES`, apply the next step and
re-serialize. Set `payload_truncated = true` and append to `omitted` when a
step removes data:

1. Drop optional finding stubs while preserving `recovery_sources`
   (`omitted: "plan_findings"`).
2. Drop non-required terminal node skeletons (`completed`, `failed`, or
   `canceled`) oldest first by internal `evidence_time`, breaking ties by
   `node_id` (`omitted: "terminal_node_evidence"`).
3. Drop compact non-actionable task policy list (`omitted: "task_policies"`).
4. Shorten digests to a fixed 16-hex-character prefix and mark
   `omitted: "full_digests"` if full digests were replaced.
5. Drop `evidence_ref` before `report_file` from optional stubs and recovery
   sources (`omitted: "evidence_refs"`). Workspace-relative paths are never
   converted to absolute paths or silently truncated.
6. Drop `work_unit_key` only from non-required, non-actionable retained nodes
   (`omitted: "non_required_work_unit_keys"`).
7. Collapse `nodes` to required, non-terminal, and actionable-route nodes with
   minimal fields (`node_id`, `role`, `task_index`, `latest_status`,
   `latest_task_id`, `required_for_gate`, plus `work_unit_key` for required or
   actionable nodes) (`omitted: "non_actionable_node_index"`).
8. Last resort: protected header, gates including full Plan cohort, Plan review
   state/counts/`next_action`, `recovery_sources`, and
   `actionable_task_routes`; omit the remaining node index
   (`omitted: "node_index"`).

If step 8 still exceeds 7,680,
return a typed tool error `payload_too_large` rather than an incomplete
silent frame. Log sizes for diagnostics.

`omitted` tokens appear once in the ladder order above. Any pre-serialize cap
also sets `payload_truncated`; `evidence_truncated` additionally reports node
evidence loss. Repeated projection of the same durable state is byte-for-byte
deterministic.

Representative preferred indexes are expected to enter this ladder. The named
soft caps bound selection work before measurement; they do not define a second
size budget or imply that the preferred shape normally fits.

### Agent secondary fetch (no new tools)

Document in the tool description (compressed style, tools/list budget aware):

1. Plan/design body → read `plan_target_rel_path` / design `rel_path`.
2. Finding narrative → read a stub or `recovery_sources` `report_file`, then
   use structured stub owners and counts for settlement reconstruction.
3. Child transcript → `get_session_info` with `child_conversation_id` and
   bounded `max_messages`.
4. Run outcome → `get_delegation_status` for `latest_task_id`s of interest.

Root settle evidence continues to be assembled from files and child runs, not
from inlined finding prose in `get_workflow_state`.

### Compatibility

- **Breaking for agents** that assume full `WorkflowStateDto` bodies,
  `latest_plan_review.findings[].summary`, full Task risk evidence, replacement
  chains, or every historical node are present. Update brainstorm-to-delivery
  and recovery skill text in the same change set so counts, structured owners,
  routes, and file/tool pointers are authoritative.
- Preserve current header/gate names. The index explicitly retains
  `publication_token`, `risk_policy_version`, full Plan reviewer cohort, CAS
  revisions, gate cycles/outcomes, and the protected Plan review fields above.
  Removed rich DTO fields are absent rather than renamed. `detail: "index"`
  and `capability_version` discriminate the new output contract.
- Frontend HTTP graph snapshot is out of scope and remains redacted / separate.

### Implementation sketch (for planning, not this doc's delivery)

Primary touch points (expected):

- `workflow/state_dto.rs` — index projection types, deterministic selection,
  omission operations, and protected-minimum validation
- `workflow/store.rs` — `get_workflow_state_core` builds full internal state then
  projects; or projects in one pass
- `companion.rs` — validate `detail` and the serialized request-id limit before
  inflight/broker mutation; render this tool without full `structuredContent`;
  enforce the actual newline-terminated JSON-RPC line budget using the real
  accepted request id; own the local stable `payload_too_large` tool error
- `transport.rs` — no `detail` field is required in v1 because the companion
  accepts only `index` and the broker/store have one projection contract
- `tool_schema.json` — short description of index-only + file follow-up
- Existing `get_workflow_state` tests — migrate full-DTO assertions to the
  explicit index contract rather than deleting recovery coverage

### Skill / prompt updates

Any skill that tells the root to “call get_workflow_state and use finding
summaries” must switch to: use counts + paths; `read` report files before
settle. Keep skill text short so tools/list / skill token budgets stay healthy.

## Error And Truncation Behavior

| Case | Behavior |
|------|----------|
| Workflow missing / cross-parent | Existing typed errors unchanged |
| `detail` omitted or `index` | Companion requests the sole v1 index projection |
| `detail` null, wrong type, or other string | Companion JSON-RPC `-32602`; no broker mutation |
| Serialized request id ≤ 256 bytes | Accepted and echoed normally |
| Serialized request id > 256 bytes | Companion JSON-RPC `-32600` with `id: null`; no inflight/broker mutation; bounded line |
| Soft node/finding caps applied | `evidence_truncated` and/or `payload_truncated` as specified |
| Hard budget ladder applied | `payload_truncated: true`, `omitted` lists steps |
| Protected minimum exceeds budget | Typed workflow tool error with `error.code = "payload_too_large"`; small bounded error frame |

Truncation must never invent gate outcomes or revise CAS numbers. Prefer
dropping optional evidence over corrupting authoritative header/gate fields.

## Tests

1. **Budget regression:** `get_workflow_state_index_jsonrpc_line_under_7680_bytes`
   renders a representative graph with ≥ 20 nodes and ≥ 15 source findings
   through the real tool-specific renderer, with request id `1`, and asserts
   the compact JSON-RPC line including newline is ≤ 7,680 bytes. Repeat with
   multibyte/escape-heavy data and an escape-heavy string request id.
2. **No inline summaries:** default output JSON has no
   `latest_plan_review.findings[].summary` (and no other multi-KB prose
   fields from the old full DTO).
3. **Orchestration fields survive:** for a mid-plan-review fixture,
   `manifest_revision`, `graph_revision`, plan gate `next_gate_cycle`,
   `next_action`, and severity counts remain present after projection.
4. **Omission ladder:** force every transition independently; assert exact
   ordered omission tokens, deterministic bytes, counts/completeness flags,
   full Plan cohort, at least one recovery pointer when findings are open, and
   the current actionable Task route.
5. **Input boundaries:** omitted and explicit `detail=index` agree; null,
   wrong-type, and unknown values return `-32602` without broker mutation.
   Request ids at 256 serialized bytes succeed; 257-byte, escape-heavy, and
   multibyte-over-limit ids return bounded `-32600` with `id:null`, without
   inflight registration or broker mutation.
6. **Protected oversize:** pathological protected workflow ids/paths return bounded
   `payload_too_large` with `isError` and structured error code.
7. **Packaging:** success has parseable `content[0].text`, no complete
   `structuredContent` copy, and text/index equivalence.
8. **Cross-parent / not found:** existing tests still pass.
9. **Catalog budget:** rerun the existing companion tools/list 7,680-byte
   regression after changing the short tool description.

## Acceptance Criteria

1. Default `get_workflow_state` responses for representative B2D recovery
   fixtures produce a compact newline-terminated JSON-RPC response line of
   **≤ 7,680 UTF-8 bytes** after actual companion rendering.
2. Finding **bodies** are never returned; agents recover prose via workspace
   files or other tools.
3. A parent agent can decide the next gate action, recover a full Plan cohort,
   locate finding prose, and identify the current/next Task route from the
   index alone when digests and task ids are current.
4. Over-budget workflow state either truncates with explicit flags or returns
   `payload_too_large`; accepted companion stdout JSONL responses never exceed
   7,680 bytes. The length-prefixed broker/UDS hop carries only the pre-capped
   preferred index and retains its separate existing 16 MiB safety ceiling; it
   is not governed by the stdout product budget.
5. Focused Rust tests encode the 7,680 contract so regressions fail in CI.
6. Tool description remains short enough not to reopen the tools/list budget
   problem for Grok.

## Residual Risks

- Pathologically long workspace-relative paths can pressure the protected
  minimum; the runtime returns `payload_too_large` rather than truncating
  identifiers or paths. Oversized request ids are rejected earlier by the
  explicit 256-byte serialized-id contract.
- Skills not updated in lockstep will look for missing summaries and may
  mis-settle; treat skill updates as required companion work.
- Tool-specific omission of `structuredContent` may affect clients that read
  only structured results; the compatibility update and renderer tests must
  make `content[0].text` the explicit contract for this tool.
- Ultra-compact last-resort payloads may omit `child_conversation_id`; agents
  then use `latest_task_id` + `get_delegation_status` only.

## Open Follow-Ups (out of v1)

- Optional `detail=recovery` with a higher budget for non-stdio hosts only —
  deferred; v1 is index-only for all agents to keep one contract.
- Content-addressed short path aliases — not required if stubs are capped.
- Align other large tool results to the same 7,680 product budget — separate
  designs if needed.
