# Workflow Blocked Recovery and State Authority Design

## Status

Approved by the user on 2026-07-30. The design has not yet been converted into
an implementation plan.

This is the workflow-level companion to
`2026-07-30-delegation-recovery-authorization-design.md`. The two designs share
one authorization service and persistence model, but deliberately keep
delegation-run recovery and workflow-state recovery in separate policy
engines.

The immediate regression fixture is workflow
`afd89cd7-5df0-49d9-8a40-1d2c95791cbd` from Codeg session 2566. The design is
general and must not special-case that workflow, conversation, document path,
or digest.

## Problem

A brainstorm-to-delivery workflow can contain a current, fully approved Plan
and still become permanently fenced by the workflow header's `blocked` state.
The observed workflow has all of the evidence needed for Task admission:

- header and active manifest revision 8 are `blocked`;
- the current Plan target is
  `client/docs/aiworkdocs/2026-07-30-home-pipeline-station-busy-lock-impl-plan-r2.md`;
- the Plan digest is
  `sha256:77fca1481d57395b3b7fe090be2d116e647f6275e303895b0b88e7ad4428d4b5`;
- the active Plan Author and both required reviewer bindings are valid and
  observed;
- Plan gate cycle 1 is durably `approved` with zero Critical and Important
  findings; and
- four older observed Plan bindings are already retired at revision 8 and are
  absent from the active manifest.

Three independent implementation behaviors turn that state into a deadlock:

1. `apply_binding_diff` processes already-retired protected bindings as if
   every later omission were a new deletion. A state-only publication can
   therefore fail cohort protection and can overwrite the original retirement
   revision.
2. new Task admission rejects a `blocked` header before accepting the current,
   exact Plan gate evidence; and
3. gate settlement can promote only a `skeleton` or `estimated` header. It has
   no typed or authorized `blocked -> non-blocked` transition.

The earlier explanation that an old Plan Author must write the manifest back
is incomplete. Root publishes manifests, and the successor Author and
reviewers already completed successfully. Author succession alone cannot
repair this workflow. The missing product behavior is an authorized,
server-derived workflow recovery transition plus a correct binding lifecycle.

## Goals

- Recover a blocked workflow in place when its current durable evidence is
  self-consistent.
- Require a real user authorization for every `blocked -> non-blocked`
  transition.
- Derive the target state in the backend; never trust a caller-provided target.
- Prevent recovery from changing workflow topology, routes, bindings, gates,
  document identity, or Task policy.
- Preserve every existing manifest, gate settlement, run binding, and retired
  binding as historical evidence.
- Make normal Plan approval materialize a coherent immutable `approved`
  manifest revision rather than changing only the header.
- Keep blocked workflows editable for legitimate Plan work without letting an
  ordinary publication implicitly unblock them.
- Support an authorized Plan-review lineage reset after
  `user_decision_required` without trusting model-authored approval claims.
- Recover the observed session-2566 workflow without rerunning its Author or
  reviewers and without manually cleaning database rows.
- Keep desktop, server, and `codeg-mcp` behavior on one shared core.

## Non-Goals

- Automatically expiring a workflow block, failed delegation lineage, or busy
  run.
- Detaching, canceling, replacing, or superseding an active run as part of
  workflow recovery.
- Allowing authorization to override parent ownership, active-run fencing,
  Task cohort freezing, route identity, Plan freshness, or evidence
  consistency.
- Accepting a complete manifest, target state, binding list, or topology patch
  in `recover_workflow`.
- Guessing why a historical workflow became blocked or fabricating block
  provenance during migration.
- Deleting retired bindings to make a later publication pass.
- Rerunning valid Plan Author or reviewer work solely to leave `blocked`.
- Creating a replacement workflow or session as the product-level repair.
- Combining the workflow policy with the delegation continue/replace decision
  table.
- Refactoring unrelated workflow projection, broker, or overlay UI code.

## Relationship to Delegation Recovery

Two policy engines consume different durable subjects:

```rust
pub fn decide_delegation_recovery(
    source: &DelegationRecoverySourceSnapshot,
    rails: &DelegationRecoveryRailSnapshot,
    operation: RequestedDelegationRecoveryOperation,
) -> DelegationRecoveryDecision;

pub fn decide_workflow_recovery(
    source: &WorkflowRecoverySourceSnapshot,
) -> WorkflowRecoveryDecision;
```

They share only the following infrastructure:

- server-owned confirmation questions;
- a durable, one-use `recovery_authorizations` entity;
- canonical source fingerprints;
- approval, decline, abandonment, expiry, and consumption behavior;
- stable authorization errors and observability; and
- parent-conversation ownership.

Delegation policy remains responsible for `continue`, `fresh_dispatch`, and
`replace`. Workflow policy remains responsible for recovering the lifecycle
state or requiring a Plan lineage reset. A workflow authorization cannot be
presented to a delegation admission, and a delegation authorization cannot be
presented to `recover_workflow` or Plan settlement.

## Core Invariants

1. `blocked` is sticky. No ordinary manifest publication or gate settlement
   may leave it without a matching user authorization.
2. A caller can request evaluation but cannot select the recovery action or
   target lifecycle state.
3. Recovery never runs binding diff and never changes structural workflow
   content.
4. The workflow header state always equals the state in its active manifest
   revision after a committed transaction.
5. Header, manifest revision, authorization consumption, and recovery
   provenance commit atomically.
6. Exact current Plan approval recovers to `approved`; stale approval does not.
7. A workflow with unresolved active or contradictory Task evidence remains
   blocked even after user approval.
8. Already-retired, still-omitted bindings are a no-op.
9. Retirement protection applies to the `active -> retired` edge, not every
   later manifest publication.
10. A frozen Task cohort cannot be removed or have its route redefined merely
    because a manifest is `blocked`.
11. Status projection is advisory. Every mutation rebuilds the source snapshot
    and policy decision inside its transaction.
12. Authorization permits one exact transition. It does not reserve or grant
    any later Task admission.
13. No migration or compatibility reader invents historical block cause,
    user intent, Plan approval, or lineage-reset provenance.

## Workflow Recovery Policy

### Source Snapshot

`WorkflowRecoverySourceSnapshot` is a typed, bounded view assembled from a
single consistent read. It contains only policy-relevant data:

- workflow, parent, kind, schema, and capability identity;
- header state, active manifest revision, structural revision, and current
  design/Plan fingerprints;
- active manifest digest and normalized document state;
- current Design and Plan document paths and digests;
- current Plan Author binding and covered author task identity;
- required Plan reviewer binding identities;
- latest Plan gate id, cycle, outcome, content fingerprint, finding counts,
  next action, covered author task, and covered Plan digest;
- binding active/retired/observed/frozen lifecycle needed by recovery;
- active, reserving, unresolved canceled, and contradictory Task/run evidence;
- current typed block cause, or `legacy_unknown` when absent; and
- pending Plan-lineage reset state, if any.

The pure policy does not query SQLite, parse arbitrary JSON, mutate a manifest,
or inspect user-facing prose.

### Decision Shape

```rust
pub struct WorkflowRecoveryDecision {
    pub workflow_id: String,
    pub source_state_fingerprint: String,
    pub disposition: WorkflowRecoveryDisposition,
    pub confirmation: RecoveryConfirmation,
    pub cause_code: WorkflowRecoveryCauseCode,
    pub risk_class: RecoveryRiskClass,
}

pub enum WorkflowRecoveryDisposition {
    Recover { target_state: ManifestWorkflowState },
    ResetPlanLineage,
    Stop { code: WorkflowRecoveryStopCode },
    InconsistentDurableState,
}
```

Every `Recover` and `ResetPlanLineage` decision originating from `blocked`
requires confirmation. A `Stop` or `InconsistentDurableState` decision cannot
be changed by confirmation.

### Decision Precedence

The store preserves outer security and idempotency checks, then evaluates the
policy in this order:

1. direct parent/root ownership and workflow kind;
2. exact active manifest revision and normalized document integrity;
3. active/reserving runs and concurrent mutation fences;
4. header/active-revision state agreement;
5. binding identity, frozen cohort, and run-binding consistency;
6. current Plan document identity and active Author/reviewer evidence;
7. latest gate settlement freshness against the current Plan fingerprint;
8. Plan `user_decision_required` lineage state;
9. exact current Plan approval; and
10. fallback to current Plan presence or Design-only/skeleton evidence.

Hard stops are evaluated before target derivation. A valid Plan approval does
not override an active run, a contradictory frozen cohort, cross-parent data,
or a corrupt manifest revision.

### Target Derivation

| Current durable evidence | Decision |
| --- | --- |
| Exact current Plan gate approval with matching fingerprint, Author, reviewer set, and digest | recover to `approved` |
| Current Plan exists but its exact gate is absent, stale, rejected, or requires ordinary revision | recover to `estimated` |
| No current Plan exists | recover to `skeleton` |
| Latest Plan round says `user_decision_required` | require `ResetPlanLineage` |
| Active/reserving run or unresolved frozen Task cohort | stop and remain `blocked` |
| Header/revision, binding, gate, or run evidence contradicts itself | inconsistent; remain `blocked` |

An absent historical block reason is not itself contradictory. It maps to
`legacy_unknown`, uses high-risk fixed copy, and still derives its target from
the current durable evidence. This is the compatibility path used by the
session-2566 fixture.

### Typed Block Causes

New transitions into `blocked` record a bounded cause enum, including:

- `plan_user_decision_required`;
- `plan_gate_blocked`;
- `explicit_manifest_block`;
- `unresolved_task_cohort`;
- `durable_state_inconsistent`; and
- `legacy_unknown` for reads of rows that predate the column.

The cause informs risk copy and policy but never substitutes for current
evidence. A stale historical cause cannot force a target state.

## State Authority and Immutable Revisions

### Ordinary Manifest Publication

`publish_workflow_manifest` remains the only operation that can change
workflow structure. When the current header is `blocked`:

- publications that change Plan content or legitimate Plan topology remain
  allowed under all existing validation and binding rules;
- the effective published state remains `blocked`, even if the caller's
  document says `skeleton`, `estimated`, or `approved`;
- the response includes a recovery projection and stable
  `workflow_recovery_required` disposition; and
- no caller-provided state value proves permission to unblock.

Such a structural publication commits successfully with
`workflow_state=blocked`; `workflow_recovery_required` is the typed recovery
disposition, not a rollback of the legitimate Plan change. A publication whose
only requested effect is leaving `blocked` creates no revision and returns the
same code, directing the caller to `recover_workflow`.

Publishing a document whose explicit target state is `blocked` remains legal.
New block transitions persist a typed block cause. Leaving `blocked` always
uses an authorized server-controlled path.

### Server-Controlled State-Only Revision

Introduce one internal helper used by Plan settlement and workflow recovery:

```rust
async fn append_state_only_manifest_revision(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    target_state: ManifestWorkflowState,
    provenance: StateTransitionProvenance,
) -> Result<StateOnlyRevision, WorkflowStoreError>;
```

The helper:

1. loads and validates the active manifest;
2. clones it and changes only `workflow_state`;
3. recomputes canonical document JSON and digest;
4. inserts a new immutable revision with `revision_kind=state_only`;
5. preserves `structural_revision`, design fingerprint, Plan fingerprint,
   publication topology, and every binding;
6. updates header state, active manifest revision, and graph revision; and
7. records source revision, transition reason, and optional authorization id.

It never calls `apply_binding_diff`. It fails if normalized structural content
changes or if the current header no longer matches the expected revision.

### Plan Approval Settlement

The normal B2D lifecycle becomes:

```text
estimated Plan manifest
  -> reviewers settle the exact Plan gate as approved
  -> settlement transaction appends an approved state-only revision
  -> header and active revision both become approved
```

The caller no longer has to pre-publish a manifest that claims `approved`.
Approval is derived from the gate settlement, not asserted by the model.

Within the same transaction, the settlement row continues to reference the
reviewed structural revision and fingerprint, while the new state-only
revision points back to that source revision. Because structural and Plan
fingerprints do not change, the approval remains current.

If the workflow was already `blocked`, an approved settlement is still stored
durably but does not leave `blocked`. A later `recover_workflow` uses that exact
evidence. A blocked settlement likewise materializes a blocked state-only
revision so header and active document cannot diverge.

## Binding Lifecycle Correction

`apply_binding_diff` must classify the prior lifecycle before evaluating
protection or absence:

| Prior binding | Present in next manifest | Required behavior |
| --- | --- | --- |
| active | yes | preserve identity rules; update only fields currently legal to update |
| active | no | perform the one legal retirement edge or reject it |
| retired | no | no-op; preserve the original retirement revision and flags |
| retired | yes | permit only exact-identity reactivation |

### Active to Retired

- An unobserved, unfrozen binding may retain the existing cleanup behavior.
- An observed or otherwise protected non-Task binding may retire only through
  an explicit validated cancellation/succession path.
- Retirement sets `retired_revision` only when it was previously NULL.
- `retained_observed` remains true once observation requires history retention.
- A generic `workflow_state=blocked` is not permission to drop every protected
  binding.

### Already Retired and Omitted

This branch exits before frozen-route and protected-deletion checks. It does
not update timestamps, `retired_revision`, `retained_observed`, or outcome.
Repeated identical publications are therefore stable.

### Exact-Identity Reactivation

A retired binding can reactivate only when all immutable identity fields match:

- workflow and node id;
- work-unit key;
- role;
- agent and profile;
- phase; and
- Task index.

On legal reactivation, the current retirement marker is cleared, the immutable
manifest history remains available in prior revisions, and
`retained_observed` cannot be downgraded. A changed identity must use a new node
id and pass normal topology rules.

### Frozen Task Cohorts

Once a Task implementer/reviewer cohort is frozen or has admitted runs:

- membership and route remain immutable;
- a blocked manifest cannot remove either side;
- cancellation may update a validated outcome but does not erase the binding;
  and
- continuation/replacement admission continues to use retained historical
  identity under the existing rules.

These constraints are independent of workflow recovery authorization.

A frozen cohort is unresolved for recovery when it has a reserving or
non-terminal run, missing implementer/reviewer route identity, a canceled node
whose paired binding or run projection disagrees, invalid latest-run
supersession, or an active manifest that attempts to remove/redefine the
cohort. A fully terminal, internally consistent frozen cohort is historical
evidence and is not by itself a recovery blocker.

## Shared Recovery Authorization

The companion delegation design defines the shared table and approval
lifecycle. Workflow recovery uses these generic fields:

- `subject_kind=workflow`;
- `subject_id=workflow_id`;
- exact `allowed_action` of `recover_workflow` or `reset_plan_lineage`;
- canonical action payload containing the derived target state when applicable;
- workflow source fingerprint;
- typed workflow cause and risk class; and
- a workflow revision as the consumer provenance.

The server-owned card offers one exact recovery action and `Keep blocked`.
Dismissal is a decline. No interactive user means no approval.

Approvals expire ten minutes after approval. They bind to the owning parent
conversation, not an ephemeral connection, so an approved receipt may survive
a reconnect within its TTL.

## Workflow Source Fingerprint

The fingerprint is a versioned SHA-256 over canonical serialization of:

- workflow and parent identity;
- active manifest and structural revision;
- active manifest digest and normalized state;
- design and Plan fingerprints;
- current Plan path and digest;
- active Plan Author identity and covered task;
- required reviewer identities;
- latest Plan gate cycle, outcome, content fingerprint, counts, next action,
  covered author task, and covered digest;
- policy-relevant binding lifecycle state;
- active/reserving/unresolved Task and frozen-cohort evidence;
- current typed block cause; and
- Plan lineage-reset state and, for reset authorization, a hash of the exact
  displayed reset reason.

It excludes arbitrary summary prose, prompts, raw external-session ids, and
unrelated UI projection. Active-run and revision CAS checks are repeated
authoritatively during consumption even when represented in the fingerprint.

## Wire Contracts

### Authorization Request

The shared MCP tool is:

```text
request_recovery_authorization(
  subject_kind,
  subject_id,
  correlation_id,
  proposed_user_reason?
)
```

For workflow subjects, the server loads current evidence and derives either
`recover_workflow`, `reset_plan_lineage`, or a hard stop. The caller cannot
provide an action or target state.

`proposed_user_reason` is rejected unless the derived action is
`reset_plan_lineage`. In that case it must be non-empty, bounded, persisted in
the challenge, hashed into the fingerprint, and displayed verbatim to the user.
The proposed text alone is not approval evidence.

The structured result includes:

```json
{
  "status": "approved",
  "recovery_authorization_id": "uuid",
  "subject_kind": "workflow",
  "subject_id": "workflow-id",
  "allowed_action": "recover_workflow",
  "target_state": "approved",
  "cause_code": "legacy_block_with_current_plan_approval",
  "expires_at": "2026-07-30T12:10:00Z"
}
```

Stable statuses are `approved`, `declined`, `abandoned`, `not_required`,
`blocked`, and `inconsistent_durable_state`.

### Workflow Recovery

Add a root-only MCP/store operation:

```text
recover_workflow(
  workflow_id,
  recovery_authorization_id,
  expected_manifest_revision,
  correlation_id
)
```

The request has no manifest, target state, node list, edge list, binding list,
lineage-reset reason, or force flag.

The result includes the old and new state, source and new manifest revisions,
new graph revision, cause code, consumed authorization id, and idempotent replay
flag. Exact parent tool-call replay returns the original result.

Calling `recover_workflow` when policy requires `reset_plan_lineage` returns
`plan_lineage_reset_required`; it cannot consume a generic recover action.

## Plan `user_decision_required` and Lineage Reset

Plan review currently permits a model-supplied `lineage_reset_reason` in its
pure transition type but correctly rejects it at the store boundary because no
user evidence exists. The authorized flow is:

1. the latest completed Plan round derives `user_decision_required` and the
   workflow remains `blocked`;
2. Plan work may prepare a new current Plan while blocked under ordinary
   publication and binding rules;
3. Root requests workflow recovery authorization with the exact bounded reset
   reason that will be shown to the user;
4. policy derives only `reset_plan_lineage` for that source state;
5. after approval, the next initial Plan round settlement supplies both the
   exact `lineage_reset_reason` and `recovery_authorization_id`;
6. settlement recomputes policy and fingerprint in its transaction, validates
   the exact displayed reason, consumes the receipt, and persists reset
   provenance; and
7. the resulting evidence produces an authorized state-only `estimated` or
   `approved` revision, or remains `blocked` if another decision is required.

Direct caller-supplied `lineage_reset_reason` without a matching approved
receipt remains invalid. A failed settlement does not consume the receipt. A
changed Plan, gate state, reviewer set, or reset reason makes it stale.

The reset authorization is also the permission for the resulting
`blocked -> estimated/approved` transition. A second confirmation is not
required for the same atomic settlement.

## Transactional Recovery Flow

`recover_workflow` performs all authoritative work in one SQLite transaction:

1. load the workflow under the direct parent conversation;
2. require current `blocked` state and exact expected manifest revision;
3. load and validate the active manifest and all policy evidence;
4. reject active/reserving work and contradictory frozen cohorts;
5. recompute decision and source fingerprint;
6. load the authorization and require approved, unexpired, unconsumed, exact
   subject, fingerprint, action, and target-state payload;
7. append the server-controlled state-only revision;
8. CAS-update header state, active revision, graph revision, and active block
   projection;
9. conditionally consume the authorization with the new manifest revision; and
10. commit, then emit one workflow graph-changed event.

Any pre-commit failure rolls back both revision insertion and authorization
consumption. Once committed, the authorization remains consumed even if
post-commit event delivery fails; normal state reload converges the UI.

## Persistence and Migration

Use additive schema changes only:

1. create the shared `recovery_authorizations` table and indexes described in
   the delegation companion;
2. add nullable `revision_kind`, `source_manifest_revision`,
   `recovery_authorization_id`, and `transition_reason_code` provenance to
   workflow manifest revisions;
3. add nullable `block_cause_code` and `block_source_manifest_revision` to the
   workflow header;
4. add nullable `lineage_reset_authorization_id` to Plan gate settlements; and
5. add the delegation-run authorization provenance required by the companion
   design in the same migration series.

Existing manifest JSON, digests, revisions, workflow states, gate settlements,
bindings, run bindings, counters, and Plan-review JSON are not rewritten.

Rows with no typed block cause are read as `legacy_unknown`. Existing retired
bindings keep their current retirement revision. Migration does not infer
which publication caused a block or manufacture a user decision.

A NULL historical `revision_kind` is read as `publication`. Entering a new
block writes the active block fields, remaining blocked preserves or replaces
them only with typed newer provenance, and successful recovery clears the
header's active block fields. Immutable manifest-revision provenance retains
the transition history.

An older binary can ignore nullable columns and the new table, but rolling back
application behavior after new state-only revisions have been created is not a
supported behavioral rollback strategy.

## Compatibility for Existing Polluted Workflows

No direct repair migration is needed for session 2566 or equivalent rows:

- already-retired omitted bindings become stable no-ops under the corrected
  diff;
- legacy block provenance is classified at read time;
- current Plan/gate evidence is evaluated normally;
- authorization allows a server-controlled state-only revision; and
- no Author/reviewer run or binding must be recreated.

New-session recovery remains an operational workaround for old binaries, not
the intended product behavior after this change.

## Concurrency and Failure Handling

| Race or failure | Required result |
| --- | --- |
| two authorization requests for one workflow state | reuse the unique pending/approved challenge |
| manifest or Plan evidence changes while card is open | fingerprint mismatch; authorization is stale |
| active run starts while card is open | transaction stops recovery; authorization is not consumed |
| two `recover_workflow` calls consume one receipt | one conditional consumer wins |
| ordinary publication races recovery | manifest revision CAS permits one winner |
| gate settlement races recovery | fingerprint/revision CAS permits one coherent winner |
| user approves after parent reconnect | same parent conversation may consume within TTL |
| user declines or dismisses | workflow remains blocked and no revision is inserted |
| parent disconnects before answering | pending challenge becomes abandoned |
| state-only serialization or validation fails | transaction rolls back without consuming receipt |
| database commit succeeds but event emit fails | durable recovered state wins; reload converges |
| exact correlation replay after success | return the original recovery result |

Authorization does not hold a workflow mutation lock while the question is
open. Normal Plan work may continue, but doing so intentionally stales the old
receipt.

## Status Projection and Stable Errors

Workflow state and Task-admission responses gain an optional read-only
projection:

```json
{
  "recovery": {
    "disposition": "confirmation_required",
    "proposed_action": "recover_workflow",
    "target_state": "approved",
    "cause_code": "legacy_block_with_current_plan_approval",
    "risk_class": "legacy_unknown_origin",
    "authorization_required": true,
    "blockers": []
  }
}
```

The projection never contains an authorization id and is never accepted as
write evidence. New Task first-dispatch continues to reject a blocked workflow,
but returns this typed projection when available.

Add stable errors:

- `workflow_recovery_required`;
- `workflow_recovery_not_available`;
- `workflow_recovery_conflict`;
- `plan_lineage_reset_required`;
- `plan_lineage_reset_authorization_required`;
- `recovery_authorization_expired`;
- `recovery_authorization_stale`;
- `recovery_authorization_consumed`;
- `recovery_authorization_action_mismatch`; and
- `inconsistent_durable_state`.

Existing `workflow_blocked`, `plan_gate_reopen`, `plan_gate_not_approved`,
`busy_thread`, and stale-manifest errors remain valid where their original
conditions apply.

## Observability

Emit structured events for:

- `workflow.recovery_decision`;
- `workflow.recovery_confirmation_requested`;
- `workflow.recovery_authorization_consumed`;
- `workflow.recovery_rejected`;
- `workflow.state_only_revision_created`;
- `workflow.plan_lineage_reset`; and
- `workflow.binding_reactivated`.

Logs include stable workflow, authorization, revision, action, target, cause,
and rejection codes. They exclude Plan contents, arbitrary reason prose,
prompts, and external session ids. User-visible reset prose remains only in the
bounded authorization/audit record that requires it.

## Testing Strategy

### Workflow Policy Tests

Use a table-driven matrix over:

- current lifecycle state and block cause;
- active manifest/header agreement;
- Plan presence, path, digest, and fingerprint;
- Author and reviewer evidence;
- latest Plan gate outcome, cycle, covered digest, and finding counts;
- active/reserving/unresolved Task evidence;
- frozen cohort consistency; and
- `user_decision_required` lineage state.

Required negative cases include stale gate approval, mismatched covered Author,
changed reviewer set, active runs, contradictory header/manifest states,
corrupt manifest JSON, frozen route mutation, and authorization attempting to
override a hard stop.

### Binding Diff Tests

Cover all four lifecycle/presence combinations, repeated omitted-retired
publication, retirement revision stability, observed flag stability,
exact-identity reactivation, changed-identity rejection, and frozen Task
cohorts under a blocked manifest.

### State Authority Tests

Prove that:

- ordinary publication cannot leave `blocked`;
- Plan work can still publish while the effective state remains blocked;
- normal estimated Plan approval atomically creates an approved state-only
  revision;
- approval while blocked keeps durable approval evidence but does not unblock;
- state-only revisions preserve structural revision and fingerprints;
- state-only recovery never calls binding diff; and
- header state always matches the active manifest state after commit/restart.

### Authorization and Concurrency Tests

Cover approve, decline, dismiss, abandon, expiry, reconnect, cross-parent,
cross-workflow, stale fingerprint, action/target mismatch, concurrent request
deduplication, concurrent single-use consumption, transaction rollback, exact
idempotent replay, and post-commit event failure.

### Plan Lineage Reset Tests

Cover fixed user-visible reason text, bounds, missing receipt, mismatched
reason, stale Plan/reviewer/gate state, successful initial-round reset,
approved and estimated outcomes, repeated `user_decision_required`, decline,
and durable settlement provenance.

### Migration Tests

Migrate fixtures containing:

- a blocked workflow with no block provenance;
- already-retired observed bindings;
- approved current Plan evidence;
- frozen Task cohorts;
- `user_decision_required` Plan state; and
- unrelated existing workflow histories.

Assert that no existing durable value changes and that new uniqueness/CAS
constraints work after repeated setup.

## Session-2566 Acceptance Fixture

Reconstruct the observed state rather than depending on a developer database:

1. workflow revision 8 and header are `blocked`;
2. the current Plan path/digest match the values in the Problem section;
3. active Plan Author and two reviewer bindings are valid and observed;
4. four older protected Plan bindings are retired at revision 8 and omitted;
5. Plan gate cycle 1 is approved with Critical=0 and Important=0;
6. no active or contradictory Task run exists;
7. status derives confirmation-required `recover_workflow -> approved`;
8. direct publication cannot unblock and does not change retired revisions;
9. user approval issues an approved-only workflow authorization;
10. `recover_workflow` consumes it and creates state-only revision 9;
11. header and revision 9 are `approved`, with unchanged structural and Plan
    fingerprints;
12. all four historical bindings retain `retired_revision=8`;
13. no Author or reviewer run is added; and
14. Task 1 first-dispatch passes the existing exact Plan gate admission checks.

## Rollout and Verification

This design and the delegation companion ship through one dependency-ordered
implementation plan:

1. additive shared authorization/provenance migration and typed contracts;
2. binding lifecycle correction and regression tests;
3. typed termination evidence required by delegation recovery;
4. separate pure delegation and workflow policy engines;
5. shared authorization service, tool, and fixed question integration;
6. state-only revision helper and Plan settlement authority changes;
7. delegation admission integration;
8. workflow recovery and Plan lineage-reset integration;
9. status projection, i18n, MCP schema, Skill, and documentation updates; and
10. final end-to-end and full repository verification.

The behavioral cutover must not expose the old broad `unresumable` matcher as
an alternative to the new delegation policy. Likewise, ordinary publication
must not gain a temporary force-unblock flag while workflow recovery is being
introduced.

During implementation, run focused tests needed for development and diagnosis.
Per user direction, run the full long-running repository matrix only after all
implementation, migrations, docs, schema, and focused tests are complete:

```powershell
pnpm eslint .
pnpm test
pnpm build

Set-Location src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Any failure in that terminal matrix is fixed before completion is claimed. The
matrix is rerun only as needed to verify the final fix set.

## Completion Criteria

The combined recovery feature is complete only when:

- delegation and workflow recovery use separate central policies;
- both consume one generic, server-issued authorization infrastructure;
- no active/busy run is detached, expired, or overridden by recovery;
- no ordinary publication can perform `blocked -> non-blocked`;
- normal Plan approval creates a coherent immutable approved revision;
- already-retired omitted bindings are stable no-ops;
- frozen Task cohorts remain protected in blocked manifests;
- every workflow recovery target is derived from current durable evidence;
- Plan lineage reset requires an exact user-approved receipt;
- migration fabricates no history;
- the session-2566 fixture recovers in place and admits Task 1; and
- the one final full validation matrix passes.
