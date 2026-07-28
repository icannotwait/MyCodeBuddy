# Workflow Reviewer Amendment Design

## Status

Approved in design dialogue on 2026-07-28. This document is pending the
required written-spec review before implementation planning.

This is a design only. It does not authorize implementation.

## Problem

Workflow manifest v2 permits CAS publication of a new manifest revision, but
it has no focused operation for changing a document-gate reviewer after work
has started. The only available recovery is to reconstruct and republish the
entire manifest.

Conversation 2381 exposed the cost of that recovery. One required Design
reviewer ended in `failed/child_refusal`. The Root had to block the workflow,
retain the failed node as canceled history, publish a new skeleton revision
with a smaller required set, and re-run the other reviewers so their evidence
was bound to the new manifest fingerprint. The reviewed document digest did
not change.

Two current behaviors cause this:

- workflow mutation is expressed only as a complete manifest replacement; and
- the Design and Plan fingerprints combine document identity with reviewer
  policy, so changing only the required reviewer set invalidates otherwise
  current review evidence.

The result is operationally correct but unnecessarily expensive and difficult
for a Root agent to perform reliably.

## Goals

- Let a Root explicitly remove or replace a required Design or Plan reviewer
  whose latest run is durably terminal `failed`.
- Preserve every old node, run, report, and amendment as auditable history.
- Reuse completed sibling-reviewer evidence when the reviewed subject and gate
  cycle have not changed.
- Let a Design gate reduce its required reviewer set to zero and transition to
  self-review without automatically approving it.
- Keep at least one required reviewer on every Plan gate.
- Make amendment retry, concurrent Root calls, restart recovery, and live graph
  updates deterministic.
- Preserve the existing four-tool `workflow_manifest_v2` capability contract
  for older clients.

## Non-Goals

- Replacing a reviewer that is running, completed, or canceled.
- Replacing a reviewer solely because its verdict is unfavorable.
- Automatically choosing a replacement agent or profile.
- Automatically approving a Design gate after its last reviewer is removed.
- Allowing a Plan gate to have no required reviewers.
- Reusing evidence across gate cycles or after document content changes.
- General arbitrary graph editing through a patch language.
- Changing Task implementer/reviewer cohort freezing in this feature.
- Adding a user-facing manual reviewer editor. The Root MCP operation is the
  authority in this version; the frontend only projects its result.

## Considered Approaches

### 1. Continue full manifest republish

The Root can clone the active document, edit nodes and gates, and call
`publish_workflow_manifest` with the current manifest revision.

This preserves the existing API but leaves the Root responsible for every
unrelated manifest invariant. It also keeps the combined fingerprint behavior,
so valid sibling evidence is invalidated. This is the current workaround and
does not meet the goals.

### 2. Add amendment fields to `publish_workflow_manifest`

The publish request could accept either a complete manifest or a local reviewer
change.

This avoids one MCP tool, but mixes declarative replacement with imperative
mutation in one request shape. Idempotency, error reporting, and authorization
would become conditional on the request variant. It also makes the base v2
contract harder to evolve safely.

### 3. Dedicated amendment operation with split evidence identity - selected

Add a Root-only `amend_workflow_reviewers` operation. The store atomically
clones the active manifest, applies one validated reviewer amendment, persists
a new immutable revision, records an audit row, and advances the graph clock.

Review subject identity is separated from reviewer-policy identity. This lets
unchanged sibling evidence survive a reviewer-only amendment while retaining a
complete record of which policy admitted each run.

## Capability Contract

The existing `workflow_manifest_v2` capability remains the all-or-none base
set of four operations:

- `get_workflow_capabilities`
- `get_workflow_state`
- `publish_workflow_manifest`
- `settle_workflow_gate`

Reviewer amendment is advertised as an extension:

```json
{
  "versions": {
    "workflow_manifest_v2": true,
    "workflow_reviewer_amendment_v1": true
  },
  "operations": [
    "get_workflow_capabilities",
    "get_workflow_state",
    "publish_workflow_manifest",
    "settle_workflow_gate",
    "amend_workflow_reviewers"
  ]
}
```

The new operation is exposed only when `workflow_v2` is enabled and the
companion role is Root. It is kept outside the base four-tool classifier so an
older Root can continue to recognize and use workflow manifest v2 while
ignoring the extension.

## MCP Request and Result

The operation accepts one amendment. Batch amendment is deliberately omitted
so validation, idempotency, and audit attribution remain local.

### Common request

```json
{
  "amendment_token": "5c39f45e-4a38-4ba3-a13a-32f445504b19",
  "workflow_id": "ef6f75c1-8382-4416-bd22-f2a0d7369054",
  "gate_id": "design-gate",
  "gate_cycle": 1,
  "action": "remove",
  "failed_node_id": "design-review-kimik3",
  "failed_task_id": "423c6487-990b-44e8-a318-a8120918ed93",
  "reason": "child_refusal",
  "justification": "Remove an unavailable required reviewer",
  "expected_manifest_revision": 3,
  "expected_graph_revision": 31,
  "expected_subject_fingerprint": "sha256:21034053f0c38d2f60bc06360e5e288b8ff637f1a68be33c42e096b70837c7e8"
}
```

`action` is `remove` or `replace`. `reason` is bounded audit text and does not
control eligibility. Eligibility comes from durable run status. `justification`
is required, redacted under the existing display-string policy, and bounded to
4 KiB.

Subject fingerprints use the canonical `sha256:<64 lowercase hex>` wire form.

### Replacement extension

`replace` additionally requires:

```json
{
  "replacement": {
    "agent_type": "code_buddy",
    "profile_id": "replacement-profile-id",
    "title": "Replacement design reviewer"
  }
}
```

The Root chooses `agent_type` and optional `profile_id`. The backend validates
the route and generates the durable `node_id` and canonical `work_unit_key`.
The replacement identity must differ from the failed node's agent/profile
identity. A same-identity retry belongs to the existing continuation or
run-replacement mechanism.

### Result

```json
{
  "amendment_id": "review-amendment-01",
  "workflow_id": "ef6f75c1-8382-4416-bd22-f2a0d7369054",
  "manifest_revision": 4,
  "graph_revision": 32,
  "gate_id": "design-gate",
  "gate_cycle": 1,
  "resolution_mode": "parent_adjudication",
  "replacement_node": null,
  "reused_reviewer_node_ids": [
    "design-review-opus48",
    "design-review-grok"
  ],
  "pending_reviewer_node_ids": []
}
```

For `replace`, `replacement_node` contains the generated `node_id`,
`agent_type`, `profile_id`, and `work_unit_key`. The Root then calls the normal
`delegate_to_agent` operation with that returned work-unit identity. Amendment
and delegation remain separate operations: a launch failure is visible as a
new failed reviewer and never requires rollback of the reviewer policy.

## Eligibility Rules

An amendment succeeds only when all of these are true in the transaction's
fresh database snapshot:

1. The workflow belongs to the active parent conversation.
2. Both expected revisions match.
3. The requested gate and gate cycle are current and open.
4. `failed_node_id` is in the gate's active required reviewer set.
5. `failed_task_id` is the latest workflow-bound run for that node and cycle.
6. The run's durable task status is exactly `failed`.
7. The request's subject fingerprint matches the current gate subject.
8. No settlement already closes that gate cycle.
9. The resulting gate satisfies the Design or Plan invariant below.

The task's `error_code` is recorded in the amendment but is not an allowlist.
Provider and adapter error vocabularies evolve; the stable eligibility boundary
is the durable terminal `failed` status. A completed reviewer with a `blocked`
or `request_changes` verdict is not failed and cannot be removed through this
operation.

Running, completed, and canceled runs always produce a typed rejection. A
failed task that is no longer the node's latest run produces
`reviewer_failure_superseded` rather than modifying the newer lineage.

## Design and Plan Invariants

### Design

A Design gate may remove its last required reviewer. In that transition the
backend changes the active gate to:

```text
required_reviewer_node_ids = []
reviewer_cohort_node_ids = []
resolution_mode = self_review
gate_kind = design
```

Historical reviewer nodes remain in the manifest's node list and durable node
bindings, but are no longer members of the active gate cohort. This shape is
valid under the existing Design self-review rules.

The transition does not settle or approve the gate. The Root must perform the
self-review and call `settle_workflow_gate` with current revisions, the open
gate cycle, and Design evidence. Existing settlement validation remains the
approval authority.

If a later document revision restores external Design review, that change is a
normal full manifest publication and opens the appropriate new review cycle. It
is not a reversal of the amendment row.

### Plan

A Plan gate must retain at least one required reviewer. Removing its last
required reviewer fails with `plan_requires_reviewer`. Replacing the last
reviewer is allowed because the old ID is exchanged for the new ID in one
transaction and the resulting set is non-empty.

## Immutable Revision Materialization

The operation does not mutate the stored JSON for an existing manifest
revision. It:

1. loads and deserializes the active revision;
2. applies the requested local change in memory;
3. runs normal manifest validation on the complete result;
4. applies node-binding changes under amendment-specific eligibility rules;
5. inserts the next immutable manifest revision; and
6. updates the workflow header's active and graph revisions.

For `remove`, the failed node remains in the manifest node list with
`required=false`. For a non-empty document gate it remains in
`reviewer_cohort_node_ids` as historical configured membership while being
absent from `required_reviewer_node_ids`. For a Design transition to
self-review, the active cohort becomes empty because that is the required
schema shape.

For `replace`, the failed node remains non-required, and the generated node is
added to both the complete reviewer cohort and required reviewer set. Node IDs
are backend-generated, opaque, stable, and collision checked. Work-unit keys
continue to use the existing canonical document-reviewer grammar.

The Task route `cohort_frozen` behavior is unchanged. This feature operates on
Design and Plan document gates only.

## Evidence Identity

The current Design and Plan fingerprints cover both the reviewed document and
the reviewer policy. Reviewer-only changes therefore make old evidence appear
stale. The implementation separates three concepts:

- `subject_fingerprint`: the material being reviewed;
- `review_policy_revision`: the required set and resolution mode in force; and
- `manifest_revision`: the complete workflow declaration version.

### Subject fingerprint

For Design, the subject consists of the Design document relative path and
digest.

For Plan, the subject consists of the Plan target, Plan path and digest, Design
document identity, risk-policy version, Task risk policies/routes, and other
material Plan inputs already covered by the current Plan freshness rules.
Reviewer node identity, required sets, cohort sets, titles, and reviewer
work-unit keys are excluded from subject identity.

The existing combined structural fingerprints may remain for manifest
demotion and structural audit. Gate evidence eligibility must use the new
subject fingerprint.

### Policy revision

Each document gate has a monotonic `review_policy_revision`, starting at one.
An amendment increments it. A full manifest publication increments it only if
the gate's reviewer cohort, required set, or resolution mode changes.

Every admitted reviewer run records the current subject fingerprint and policy
revision. Every settlement records the subject fingerprint, policy revision,
and exact required reviewer set used for adjudication.

### Evidence reuse

A reviewer run is eligible for the current gate when:

- its node is in the current required reviewer set;
- its terminal summary is validated;
- its subject fingerprint equals the current subject fingerprint;
- its gate ID and gate cycle equal the current open gate; and
- it is the latest eligible run for that node.

Policy revision equality is not required for a sibling node that remains
required. The settlement snapshots the current policy and required set so the
historical decision remains reproducible.

Evidence is never reused across gate cycles, after a subject change, for a
removed node, or from a failed/canceled run.

## Persistence Model

Add `delegation_workflow_reviewer_amendments` with:

```text
amendment_id                         TEXT primary key
amendment_token                      TEXT unique not null
request_digest                       TEXT not null
workflow_id                          TEXT not null
gate_id                              TEXT not null
gate_cycle                           INTEGER not null
action                               TEXT not null
failed_node_id                       TEXT not null
failed_task_id                       TEXT not null
failed_error_code                    TEXT null
replacement_node_id                  TEXT null
subject_fingerprint                  TEXT not null
review_policy_revision_before        INTEGER not null
review_policy_revision_after         INTEGER not null
manifest_revision_before             INTEGER not null
manifest_revision_after              INTEGER not null
graph_revision_before                INTEGER not null
graph_revision_after                 INTEGER not null
reason                               TEXT not null
justification                        TEXT not null
result_json                          TEXT not null
created_at                           TEXT not null
```

The unique amendment token makes transport retries idempotent. Reusing a token
with different canonical request content returns
`amendment_token_mismatch`, mirroring publication-token behavior. The bounded
`result_json` is the exact successful agent-facing result returned on the first
commit; an identical retry returns that snapshot even if later runs or
amendments have changed the current workflow state.

Add the subject and policy fields needed by workflow headers, run bindings, and
gate settlements. Migrations backfill existing rows from their current combined
fingerprints and immutable manifest revisions. For each historical run or
settlement, the migration uses its recorded manifest revision to derive the
subject that was actually reviewed rather than assuming the active manifest.
Backfilled evidence remains valid under existing behavior; it becomes reusable
across a future reviewer-only amendment only when the migration can derive the
subject fingerprint unambiguously. Otherwise it fails closed and requires a
fresh review.

No existing revision, node binding, run binding, or settlement row is deleted.

## Transaction and Idempotency

The store processes an amendment in one database transaction:

1. authenticate and resolve parent ownership before store entry;
2. canonicalize the request and compute its digest;
3. check an existing `amendment_token`;
4. load the workflow header, manifest, gate, bindings, latest runs, and
   settlements;
5. enforce both revision CAS values and every eligibility rule;
6. build and validate the next manifest;
7. persist the amendment row, next manifest revision, binding updates, header
   revisions, subject/policy state, and graph revision; and
8. commit before emitting `workflow_graph_changed`.

An identical token and digest returns the original result. A different digest
for the same token fails without mutation. Concurrent different tokens race on
the header CAS; at most one commits.

The event contains the committed graph revision. Existing frontend revision
gating discards stale events and snapshots.

## Recovery DTO and Frontend Projection

`get_workflow_state` adds:

- current gate `subject_fingerprint` and `review_policy_revision`;
- a bounded amendment history sufficient to reconstruct remove/replace chains;
- per-node amendment disposition; and
- replacement linkage where present.

The redacted frontend graph adds optional safe fields rather than exposing raw
work-unit keys or justifications:

```text
reviewer_disposition = removed_after_failure | replaced_after_failure | null
replacement_node_id = safe public node id | null
```

The graph keeps the failed node visible. Its status presentation distinguishes:

- `Failed - Removed`
- `Failed - Replaced by <reviewer>`

The replacement node appears in the active reviewer branch and gate counts.
A zero-reviewer Design gate presents self-review/waiting-adjudication state;
it must not look approved.

All new visible labels are added to every supported locale. No frontend editor
or mutation control is added.

## Error Contract

The operation returns the existing structured workflow error shape. New stable
codes are:

| Code | Meaning |
|---|---|
| `amendment_token_mismatch` | Token already names a different amendment request |
| `reviewer_not_required` | Node is not in the current required set |
| `reviewer_not_failed` | Latest run is not durably `failed` |
| `reviewer_failure_superseded` | Supplied task is not the node's latest run |
| `gate_cycle_closed` | Requested cycle already has a settlement or is not current |
| `subject_fingerprint_mismatch` | Reviewed material changed or caller state is stale |
| `replacement_identity_conflict` | Replacement duplicates the failed or active reviewer identity |
| `plan_requires_reviewer` | Removal would leave a Plan gate with no required reviewer |

Existing `stale_manifest_revision`, `stale_graph_revision`, ownership,
validation, persistence, and busy errors remain applicable.

Validation errors are non-retryable until the Root changes the request.
Stale errors require `get_workflow_state` and a new decision. Transient busy or
persistence errors may retry the identical amendment token.

## Security and Bounds

- Reuse the current Root-only workflow authentication and parent ownership
  checks.
- Never accept work-unit keys, generated node IDs, or replacement linkage from
  the Root.
- Validate replacement agent/profile through the existing route/profile
  catalog before mutation.
- Bound `reason` to a stable short enum-like string and `justification` to
  4 KiB.
- Redact justification before any frontend or log projection.
- Do not log full profile configuration, document paths, reports, or evidence.
- Bound amendment history in agent and frontend DTOs while keeping the database
  audit complete.
- Continue enforcing manifest node, edge, gate, and JSON size limits after the
  generated revision is materialized.

## Test Strategy

### Unit tests

- Parse and validate remove and replace request shapes.
- Reject missing/mismatched amendment tokens and overlong text.
- Accept only Root plus `workflow_v2` capability.
- Preserve the base four-tool classifier while advertising the extension.
- Derive stable Design and Plan subject fingerprints that exclude reviewer
  policy but retain all reviewed material.
- Increment policy revision only for reviewer-policy changes.
- Generate collision-free replacement node IDs and canonical work-unit keys.
- Enforce Design-zero and Plan-nonzero invariants.
- Project removed/replaced/self-review states without exposing keys or raw
  justification.

### Store and integration tests

- Reproduce conversation 2381: one Design reviewer fails, is removed, and two
  completed sibling summaries remain eligible without new runs.
- Replace a failed reviewer and prove the gate waits only for the new node.
- Remove the last Design reviewer and prove the gate becomes self-review but is
  not settled or approved.
- Settle that self-review explicitly through `settle_workflow_gate`.
- Reject removal of the last Plan reviewer; allow its atomic replacement.
- Reject running, completed, canceled, non-required, and superseded targets.
- Reject replacement with the same or an already active identity.
- Reject amendment after the gate cycle is settled.
- Invalidate old evidence when the subject digest changes.
- Never reuse evidence across gate cycles.
- Commit exactly one of two concurrent CAS-valid amendments.
- Return the same result for an identical amendment-token retry and reject a
  mismatched reuse.
- Recover identical amendment, required-set, policy revision, and evidence
  state after closing and reopening the database.
- Emit one committed graph event and discard stale frontend revisions.

### Migration tests

- Create and upgrade a pre-amendment workflow database.
- Backfill derivable subject fingerprints and policy revision one.
- Fail closed for legacy evidence whose subject cannot be derived.
- Preserve all existing manifests, bindings, runs, and settlements.
- Verify desktop and server modes apply the same migration.

### Frontend tests

- Render failed-removed and failed-replaced reviewer nodes.
- Update required/returned gate counts after an amendment event.
- Render a zero-reviewer Design gate as self-review awaiting settlement.
- Keep historical nodes visible without counting them as required.
- Discard lower-or-equal graph revisions.
- Validate all locale message files contain the new labels.

### Repository verification

Implementation affects shared Rust workflow, MCP, migration, and frontend graph
paths. Run:

```powershell
pnpm eslint .
pnpm test
pnpm build

Set-Location src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings

cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Review migration snapshots with `cargo insta review` if snapshot output changes.

## Expected Implementation Boundary

Expected areas in scope:

- `src-tauri/src/acp/delegation/companion.rs` for catalog, schema, parsing, and
  local capability reporting;
- `src-tauri/src/acp/delegation/transport.rs` and `listener.rs` for the broker
  message and Root dispatch;
- `src-tauri/src/acp/delegation/workflow/` for amendment validation, store
  transaction, evidence identity, recovery, projection, errors, and tests;
- `src-tauri/src/db/entities/` and `src-tauri/src/db/migration/` for amendment
  audit and fingerprint/policy persistence;
- `src/lib/types.ts`, workflow graph state, and chat workflow graph components;
- all locale message files; and
- focused Rust and frontend tests adjacent to those modules.

Task cohort mutation, delegation runtime replacement rules, unrelated workflow
schema changes, and a user-facing reviewer editor are outside scope.

## Acceptance Criteria

1. A Root can remove a currently required reviewer only when its latest bound
   run for the open gate cycle is durably `failed`.
2. A Root can replace such a reviewer by explicitly choosing a different
   agent/profile and receives a backend-generated node ID and work-unit key.
3. All failed reviewer nodes, runs, reports, and amendment relationships remain
   queryable after removal, replacement, and restart.
4. Completed sibling evidence remains valid after a reviewer-only amendment
   when subject fingerprint and gate cycle are unchanged.
5. Subject changes and gate-cycle changes invalidate prior evidence.
6. Removing the last Design reviewer transitions to self-review without
   settling or approving the gate.
7. The Root must explicitly settle the zero-reviewer Design gate.
8. A Plan gate can never have zero required reviewers.
9. Identical amendment-token retries are idempotent, mismatched reuse fails,
   and concurrent CAS updates cannot partially commit.
10. Existing workflow manifest v2 clients continue to recognize and use the
    original four-tool capability set.
11. Frontend workflow state shows removed and replaced history while gate
    counts use only the current required set.
12. Required frontend and Rust verification passes in desktop, server, and
    `codeg-mcp` modes.
