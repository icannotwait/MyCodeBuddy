# Delegation Recovery Policy and User Authorization Design

## Status

Approved by the user on 2026-07-30. The design has not yet been converted into
an implementation plan.

This design amends the automatic-recovery rules in:

- `2026-07-21-delegation-session-reuse-design.md`; and
- `2026-07-21-acp-termination-causality-audit-design.md`.

It is paired with
`2026-07-30-workflow-blocked-recovery-design.md`. The two designs keep their
policy engines separate and share one generic recovery-authorization service,
question flow, and persistence model. The workflow companion owns lifecycle
state recovery, binding lifecycle correction, and Plan lineage reset.

Explicit user or parent cancellation remains ineligible for automatic
recovery. This design adds a separate, server-verifiable, user-authorized
recovery path. It does not redefine cancellation as `unresumable`.

The immediate implementation baseline is commit `cc55cf57` (`fix(delegation):
recover parent-end stuck work units`). That commit correctly identifies a
stuck-lineage failure but admits recovery too broadly.

## Problem

A durable delegation work unit can become permanently fenced when all three
of these conditions hold:

1. a prior run reached `running`, establishing its lineage;
2. the run later settles with a parent-end or cancellation code that the
   continue policy rejects; and
3. replacement admission also rejects the caller's reason because the durable
   thread is still resumable.

The observed task `965a94c4-f7e5-4337-807f-2aa5b1efe95f` demonstrates this
state:

- durable status: `canceled`;
- durable error: `parent_disconnected`;
- `reached_running_at` is present;
- launch snapshot and external session identity are complete;
- `termination_audit_json` is NULL; and
- the lineage and work-unit fences are already established.

The resulting calls fail in sequence:

- `continue_delegation` returns not continuable;
- `delegate_to_agent(... replacement_reason=unresumable)` does not match the
  durable state; and
- a same-key cold dispatch is rejected by established-lineage fencing.

The root-cause diagnosis is correct: the Plan Author child is stuck between
the continue and replacement rails. It is not a missing parent-session resume
identity problem.

Commit `cc55cf57` fixes the immediate dead end by:

- treating `parent_disconnected + reached_running` as an unexpected continue
  even when audit is missing; and
- allowing parent-end, explicit-cancel, and stall codes to match
  `replacement_reason=unresumable`.

The first change cannot distinguish an unexpected transport loss from
`provider_unmount`, `disconnect_all`, application shutdown, or connection
replacement. The second change bypasses resume-first behavior, consumes the
single replacement budget early, and changes the audit meaning of
`unresumable`. Both decisions are based on an error code instead of typed
termination evidence.

## Goals

- Recover established work units without silently reversing a user cancel.
- Make automatic recovery depend on typed, durable termination evidence.
- Require a real interactive user decision for explicit or ambiguous recovery.
- Make that decision server-verifiable, one-shot, scoped, and auditable.
- Enforce resume-first behavior in the backend rather than only in Skill text.
- Preserve existing lineage, work-unit, ownership, route, capability, and
  budget rails.
- Preserve the safe pre-admission host-restart continuation exception.
- Keep `unresumable`, `not_supported`, `budget_exhausted_continue`,
  `admission_failed`, and `admission_unknown` semantically distinct.
- Give status callers a stable recovery disposition without making status
  authoritative for later admission.
- Add enough cold-status diagnostics to distinguish lookup, ownership, token,
  and storage failures internally.
- Recover the observed historical NULL-audit task without guessing its cause.
- Share one authorization infrastructure with workflow recovery without
  allowing either policy to decide or consume the other's actions.

## Non-Goals

- Automatically continuing explicit user or parent cancellations.
- Automatically replacing a child from inside `continue_delegation`.
- Allowing confirmation to override route policy, authorization, ownership,
  agent/profile identity, active-run fencing, or hard budgets.
- Guessing or backfilling historical termination provenance.
- Reworking general delegation scheduling or card summary behavior.
- Making delegation recovery decide workflow lifecycle state or topology; that
  boundary is defined by the workflow blocked-recovery companion design.
- Adding an append-only termination journal; the bounded per-run and
  conversation projections remain sufficient.
- Building a second question-card UI.
- Providing an automated repair path for internally contradictory durable
  rows.

## Chosen Approach

Use a centralized delegation recovery decision engine plus a server-issued,
durable, one-time authorization from the shared recovery-authorization
service.

The existing operation boundaries remain intact:

- `continue_delegation` resumes an existing child session;
- a same-key first dispatch creates a child only when lineage was not
  established;
- `delegate_to_agent` creates a replacement only when a typed replacement
  reason matches durable state; and
- a new authorization tool asks the user for permission but performs no
  delegation admission itself.

This keeps replacement visible and preserves the existing accounting model.

### Rejected: caller-provided confirmation boolean

Adding `user_confirmed=true` to an MCP request is insufficient. The model is
the caller and can set the value without the user seeing a question. It does
not prove user intent and is replayable across calls unless the server builds
nearly all of the authorization design around it anyway.

### Rejected: fully automatic `recover_delegation`

A tool that chooses continue, presents a question, and silently falls through
to replacement would hide the existing recovery rails. It would also recreate
the central defect: replacement could occur without a separately observable
durable resumability failure.

### Rejected: permanent same-key stop after cancellation

This preserves cancellation intent but makes accidental parent teardown or a
later explicit user retry impossible without inventing a new work-unit key.
That weakens lineage accounting and encourages callers to bypass fencing.

## Terminology

**Established lineage** means at least one prior run for the child/work unit
has a non-NULL `reached_running_at`. A pure generation-1 pre-admission abort
does not establish lineage.

**Pure pre-admission abort** means a generation-1 run with no earlier
established lineage never reached running, no prompt may have been admitted,
and it is not an `admission_failed`, `admission_unknown`, or bound host-restart
case. A NULL `reached_running_at` alone is not sufficient.

**Resume identity** means a complete launch snapshot, route identity, agent and
profile identity, and a resume-capable external session id.

**Automatic recovery** means admission without a user authorization.

**Authorized recovery** means the user approved a server-authored recovery
challenge and the admission transaction consumed the resulting authorization.

**Structural unresumability** means the backend can prove that resume cannot
be attempted, for example because the external session identity is missing.
It does not include a cancel, parent-end, or stall code by itself.

## Core Invariants

1. Every public recovery entry point uses the same server-side decision.
2. The caller may request an operation but cannot choose its recovery class.
3. When an established lineage has valid resume identity and budget, recovery
   must continue the existing child before replacement is admissible.
4. `unresumable` matches only a durable resumability failure or structural
   absence of resume identity. Cancellation codes never alias it.
5. A user authorization expresses permission to retry. It never overrides
   ownership, route, capability, latest-run, active-run, or budget checks.
6. Explicit or ambiguous termination requires confirmation even if another
   independent fact, such as missing resume identity, makes replacement the
   proposed action.
7. A pure generation-1 pre-admission abort uses same-key fresh dispatch and
   consumes neither the unexpected-continue nor replacement rail.
8. A pre-admission host restart with a complete external resume identity may use
   the existing continue exception and inherits its original admission class.
   A source whose class is `replacement` cannot move onto the continue rail.
9. `admission_unknown` never uses fresh dispatch or continue. It requires an
   explicit, user-authorized replacement because the prior prompt may have
   executed.
10. A pre-admission continuation/replacement attempt does not erase an earlier
    established lineage. When execution is known not to have been admitted, it
    retries its existing rail and preserves its admission class/reason; it
    never becomes a fresh first dispatch.
11. Budget counters are charged only at the existing successful `running`
    promotion point. Authorization does not change counter timing.
12. A terminal status/error-code contradiction fails closed as
    `inconsistent_durable_state`; it is not treated as a compatibility case.
13. A confirmation-required decision does not mint a run or consume a budget.
14. A reserving or running source always returns `busy_thread`; authorization
    cannot detach or supersede it, and the busy tool result keeps
    `Input.Detach=false`.
15. Failed and canceled lineage fences do not expire with wall-clock time.
    They leave the fence only through the typed continue/fresh/replacement
    policy defined here.

## Architecture

### Recovery Policy

Add one pure decision function in a focused recovery-policy module:

```rust
pub fn decide_delegation_recovery(
    source: &RecoverySourceSnapshot,
    rails: &RecoveryRailSnapshot,
    operation: RequestedRecoveryOperation,
) -> RecoveryDecision;
```

The store builds `RecoverySourceSnapshot` from durable rows. It contains only
the fields that affect policy:

- parent, child, lineage, work-unit, agent, and profile identity;
- latest/superseded/history-only state;
- active-run state;
- run status, error code, reached-running state, and admission class;
- parsed termination evidence;
- launch-snapshot completeness and external-session presence;
- current capability; and
- replacement and unexpected-continue budget snapshots.

The pure function does not query the database and does not parse ad hoc JSON.
The store parses termination JSON into a typed compatibility enum first.

The decision shape is:

```rust
pub struct RecoveryDecision {
    pub source_task_id: String,
    pub source_state_fingerprint: String,
    pub disposition: RecoveryDisposition,
    pub confirmation: RecoveryConfirmation,
    pub cause_code: RecoveryCauseCode,
    pub risk_class: RecoveryRiskClass,
}

pub enum RecoveryDisposition {
    Continue { admission_class: AdmissionClass },
    FreshDispatch,
    Replace { replacement_reason: ReplacementReason },
    Stop { code: RecoveryStopCode },
    InconsistentDurableState,
}

pub enum RecoveryConfirmation {
    NotRequired,
    Required,
}
```

The same decision function is used by:

- continue admission;
- fresh first-dispatch admission;
- replacement admission;
- recovery-authorization issuance; and
- terminal status projection.

Status output is advisory. Every mutating path rebuilds the snapshot and
recomputes the decision inside its admission transaction.

### Decision Precedence

The backend preserves the established outer security and idempotency checks,
then evaluates recovery in this order:

1. parent tool-call idempotency and direct-parent ownership;
2. active run, yielding `busy_thread`;
3. source latestness/supersession, yielding `stale_task_id`;
4. agent, profile, workspace, route, history, and ownership invariants;
5. durable status/error consistency;
6. lineage establishment and pre-admission classification;
7. termination cause and execution ambiguity;
8. resume identity and capability;
9. continue and replacement budgets; and
10. attachment of the cause-derived confirmation requirement to the final
    safe action.

The policy derives confirmation requirements from the termination cause so a
fallback from continue to replacement cannot erase them. It then computes the
final rail-safe action and attaches that requirement. A confirmation cannot
turn a stop decision into admission.

### Durable Consistency Checks

At minimum, these combinations are inconsistent rather than recoverable:

- `completed` with a cancel/error code;
- `failed` carrying a code that is defined as a canceled disposition, including
  `parent_disconnected`, `parent_canceled`, `parent_turn_failed`,
  `join_abandoned`, `user_cancelled`, or `tool_stalled_timeout`;
- non-terminal status selected as a recovery source after the active-run check;
- a replacement linkage that does not target the latest terminal run; and
- a run whose immutable parent/child/agent/profile lineage disagrees with the
  work-unit budget rows.

This deliberately removes the `cc55cf57` defensive acceptance of
`failed + parent_disconnected`.

### Recovery Decision Table

| Durable evidence | Disposition | Confirmation |
| --- | --- | --- |
| `completed` | continue / `normal_revision` | no |
| post-running revision-eligible `failed` | continue / `normal_revision` | no |
| post-running `canceled` with typed unexpected transport/process/session evidence | continue / `unexpected_continue` | no |
| post-running host restart with running audit | continue / `unexpected_continue` | no |
| post-running `parent_disconnected` with NULL, malformed, intentional, or ambiguous audit | continue / `unexpected_continue` when resumable | yes |
| post-running `parent_canceled` or `user_cancelled` | continue when resumable | yes |
| post-running `parent_turn_failed`, `join_abandoned`, or `tool_stalled_timeout` | continue when resumable | yes |
| post-running generic/legacy unknown cancel | continue when resumable | yes, high-risk copy |
| pure generation-1 pre-admission infrastructure abort | same-key fresh dispatch | no |
| pure generation-1 explicit, parent-end, stall, or legacy abort | same-key fresh dispatch | yes |
| pre-admission continue attempt in an established lineage, with no execution ambiguity | retry continue and inherit admission class | inherited from cause |
| pre-admission replacement attempt in an established lineage, with no execution ambiguity | retry replacement with the same reason | inherited from cause/provenance |
| pre-admission host restart with complete resume identity and non-replacement class | continue, inheriting the class | no |
| `admission_failed` | replacement / `admission_failed` | no |
| `admission_unknown` | replacement / `admission_unknown` | yes |
| structural resume identity absence | replacement / `unresumable` | inherited from cause |
| an actual resume/load attempt persisted as `unresumable` | replacement / `unresumable` | no second confirmation when provenance permits |
| unexpected-continue rail exhausted | replacement / `budget_exhausted_continue` | inherited from cause |
| resume capability disabled | replacement / `not_supported` | inherited from cause |
| route/policy/auth rejection | stop | confirmation cannot override |
| replacement rail exhausted | stop / user escalation | confirmation cannot override |
| contradictory durable row | inconsistent durable state | no admission |

"Inherited from cause" means a canceled or ambiguous source still needs user
authorization even if resume is structurally impossible. A normal completed or
revision-eligible source does not gain a confirmation requirement merely
because its external session later proves unavailable.

### Resume-First Enforcement

When the source is established, resumable, capable, and within the continue
budget, the only decision the authorization service can issue is `Continue`.
There is no request parameter that asks the authorization service to issue a
replacement receipt instead.

If the continue bootstrap later proves the session unresumable, that new run
settles as `failed/unresumable`. Replacement admission evaluates that new
latest run, not the earlier canceled run. This produces the required durable
evidence before replacement.

`continue_delegation` itself never creates a replacement.

## Termination Evidence

### Typed Parent-End Context

Parent teardown must carry cause, not only a wire error code:

```rust
pub struct ParentEndContext {
    pub reason: ParentTurnEndReason,
    pub termination: AcpTerminationSummaryV1,
}
```

For `ParentDisconnected`, the lifecycle termination-intent registry supplies
the root cause. The projection preserves distinctions including:

- unrequested transport close;
- process exit;
- control-channel/session loss;
- frontend `provider_unmount`;
- frontend `disconnect_all`;
- application shutdown;
- connection replacement/supersession; and
- legacy unspecified origin.

When the registry has no evidence, the broker writes `LegacyUnspecified`. It
never fabricates an unexpected transport source.

Known non-connection causes use typed synthetic summaries with stable fields:

- `parent_canceled` and `user_cancelled`: explicit cancellation;
- `parent_turn_failed`: parent failure;
- `join_abandoned`: orchestration abandonment;
- `tool_stalled_timeout`: automated timeout with execution ambiguity;
- host restart: host-restart provenance and prior admission state; and
- child disconnect/error: the child connection's lifecycle summary.

### Typed Terminal Writes

Replace raw `with_termination_audit_json(String)` use in production with a
typed terminal-evidence builder. Serialization occurs at the store boundary.
New canceled rows require termination evidence. Test/migration-only helpers may
construct legacy NULL-audit rows explicitly.

The per-run projection must retain the fields needed by recovery:

- version;
- stable source, reason, and classification;
- prior run status and admission class;
- root/request correlation when available;
- whether a prompt may already have executed; and
- observed/requested timestamps.

The implementation may represent this as the existing
`AcpTerminationSummaryV1` plus typed run context, but policy code consumes a
single typed Rust enum. It must not inspect arbitrary `serde_json::Value`
fields.

### Atomic Persistence

The winning terminal CAS writes, in one transaction:

- final run status and error code;
- `delegation_task_runs.termination_audit_json`;
- final runtime/card fields already owned by settlement; and
- the child conversation's latest termination projection.

All terminal producers use the audit-aware builder:

- drained running tasks;
- reserving handoffs;
- DB-only non-terminal parent-end sweeps;
- setup/admission-window terminals;
- external-handle and explicit task cancellation;
- child connection terminal handling; and
- startup host-restart reconciliation.

First-terminal-wins remains authoritative. If an earlier child terminal wins,
a later parent end cannot replace its error code or audit. The process-local
handoff disposition therefore carries both the winning reason and evidence.

### Legacy Audit Compatibility

No migration guesses historical provenance.

- `canceled + parent_disconnected + reached_running + audit=NULL` becomes the
  typed compatibility cause `legacy_parent_disconnect`; it requires user
  authorization and proposes continue when resume identity is intact.
- The same exact cause before running proposes an authorized fresh dispatch
  when it is a pure pre-admission abort.
- Other NULL or malformed cancel audits become `legacy_unspecified`; they
  require a high-risk user authorization and the policy's most conservative
  safe action.
- A malformed audit never becomes automatic unexpected recovery.
- A status/error contradiction is not a legacy-audit case and does not receive
  an authorization.

## User Recovery Authorization

### Server-Owned Confirmation

Add one shared MCP tool:

```text
request_recovery_authorization(
  subject_kind,
  subject_id,
  correlation_id,
  proposed_user_reason?
)
```

For this design, `subject_kind` must be `delegation_task`, `subject_id` is the
exact source task id, and `proposed_user_reason` is rejected. The optional
reason exists only for the workflow companion's user-visible Plan lineage
reset flow.

The caller cannot supply action, reason, warning text, options, work-unit key,
or target child. The server loads the task, computes policy, and either:

- returns `authorization_not_required` with the current safe action;
- returns a hard stop/inconsistent decision;
- reuses an existing pending/approved challenge for the same source state; or
- opens a server-owned confirmation challenge.

The challenge reuses the existing blocking question-card transport and card
component. It adds a server-owned recovery question kind with stable choice ids
`approve` and `decline`. The frontend localizes the fixed action/cause/risk
copy through the existing i18n system; the model cannot customize it.

The card offers one recovery action and "Keep stopped". Dismissal is a decline.
If no interactive user is attached, the request cannot auto-approve.

### Authorization Data Model

Add the shared `recovery_authorizations` table:

| Column | Purpose |
| --- | --- |
| `authorization_id` | server-minted UUID primary key |
| `parent_conversation_id` | parent ownership boundary |
| `subject_kind` | `delegation_task` or `workflow` |
| `subject_id` | exact task/workflow subject id |
| `source_task_id` | exact source terminal run for delegation subjects, otherwise NULL |
| `child_conversation_id` | delegation child identity binding, otherwise NULL |
| `lineage_root_task_id` | delegation lineage binding, otherwise NULL |
| `work_unit_key` | delegation orchestration binding, nullable for ad hoc/workflow subjects |
| `source_state_fingerprint` | versioned hash of policy-relevant source state |
| `allowed_action` | exact delegation or workflow recovery action |
| `action_payload_json` | canonical exact action parameters, including replacement reason or workflow target |
| `cause_code` | stable displayed/audited cause |
| `risk_class` | stable displayed/audited warning class |
| `display_reason` | bounded user-visible reason when an action requires one |
| `status` | `pending`, `approved`, `declined`, `consumed`, `expired`, or `abandoned` |
| `question_id` | question registry correlation |
| `requested_at` | challenge creation time |
| `approved_at` | user approval time |
| `expires_at` | approval expiration time |
| `consumed_at` | successful consumer transaction time |
| `consumed_by_kind` | `delegation_task_run` or `workflow_manifest_revision` |
| `consumed_by_id` | newly inserted run/revision identity, when consumed |

Use a partial unique index over parent, subject kind, subject id, and source
fingerprint for `pending`/`approved` rows. One source state cannot carry two
competing actions; repeated requests reuse its active challenge instead of
opening multiple cards.

Approvals expire ten minutes after approval. Pending questions are reclaimed
when their parent turn/connection closes. Expired rows are marked lazily;
consumed/declined audit rows follow the application's existing bounded
retention policy.

The authorization table and consumer provenance columns are logical references
rather than circular SQLite foreign keys. Conversation deletion cleanup
removes authorization rows for that parent conversation.

### Delegation Source State Fingerprint

The fingerprint is a versioned SHA-256 over a canonical serialization of:

- source task, parent, child, lineage, and work-unit identity;
- agent/profile identity;
- terminal status, error code, reached-running time, and admission class;
- canonical parsed termination evidence, or a hash of the raw bytes when a
  legacy audit cannot be parsed;
- launch/route snapshot identity;
- hashed external-session identity/presence; and
- replacement linkage/supersession state.

It contains no prompt or raw session id. Budget values are not part of the
fingerprint because they are independently and authoritatively rechecked in
the consumption transaction.

### Approval Lifecycle

On approval, the question handler conditionally changes `pending` to
`approved`. On decline/dismissal it changes to `declined`. A parent disconnect
before a decision changes an unresolved challenge to `abandoned`.

An approved authorization is bound to the parent conversation rather than the
ephemeral parent connection. It may therefore survive a reconnect within its
TTL. A repeated authorization request can return the still-valid approved id
to the reconnected parent.

### Transactional Consumption

Both `ContinueDelegationRequest` and `DelegationRequest` gain:

```rust
pub recovery_authorization_id: Option<String>;
```

When policy requires confirmation, admission performs all of these in one
transaction:

1. reload source, latest run, active fences, identity, capability, and budgets;
2. recompute the policy decision and source fingerprint;
3. load the authorization under the same parent conversation;
4. require `approved`, unexpired, unconsumed, exact fingerprint, exact action,
   and exact action payload including replacement reason;
5. run existing conditional budget/fence preflights;
6. insert the new reserving run; and
7. conditionally mark the authorization `consumed` with the new task id.

Any failure rolls back both insertion and consumption. Concurrent consumers
cannot both win. Existing parent tool-call idempotency still returns the same
result for an exact replay of one invocation.

Once the reserving run commits, the authorization remains consumed even if a
later spawn/resume step fails. A new independent admission attempt requires a
new authorization unless the resulting durable run itself establishes the
typed follow-on replacement case described below.

### Provenance on Runs

Add nullable `delegation_task_runs.recovery_authorization_id`. An admitted run
stores the consumed authorization id.

If an authorized continue subsequently settles as a real
`failed/unresumable`, the next replacement can follow that provenance without
asking the user a second time. The replacement still needs
`replacement_reason=unresumable` and still consumes the normal one-replacement
rail. This is not an automatic replacement; the caller performs a separate
`delegate_to_agent` operation.

The same principle applies when the approved action was already replacement
because resume identity was structurally absent, capability was disabled, or
the continue budget was exhausted. The authorization is bound to that exact
replacement reason.

## Wire Contracts

### Authorization Tool Result

The new tool returns structured content resembling:

```json
{
  "status": "approved",
  "recovery_authorization_id": "uuid",
  "subject_kind": "delegation_task",
  "subject_id": "task-id",
  "allowed_action": "continue",
  "replacement_reason": null,
  "cause_code": "legacy_parent_disconnect",
  "expires_at": "2026-07-30T12:10:00Z"
}
```

Other stable statuses are `declined`, `abandoned`, `not_required`, `blocked`,
and `inconsistent_durable_state`.

### Status Projection

`DelegationTaskReport` gains an optional read-only recovery projection:

```json
{
  "recovery": {
    "disposition": "confirmation_required",
    "proposed_action": "continue",
    "replacement_reason": null,
    "cause_code": "legacy_parent_disconnect",
    "risk_class": "legacy_unknown_origin",
    "authorization_required": true
  }
}
```

This appears on cold and live status results when the caller owns the task.
The projection never contains an authorization id. It may become stale and is
not accepted as admission evidence.

A `continue_delegation` call that lacks required authorization returns the same
typed recovery projection with `recovery_confirmation_required`. It does not
insert a failed run or consume any rail.

### Stable Errors

Add:

- `recovery_confirmation_required`;
- `recovery_declined`;
- `recovery_authorization_expired`;
- `recovery_authorization_stale`;
- `recovery_authorization_consumed`;
- `recovery_authorization_action_mismatch`; and
- `inconsistent_durable_state`.

These are operation/setup results unless a run was already admitted. They do
not fabricate durable failed children.

Keep existing `busy_thread`, `stale_task_id`, `not_continuable`,
`invalid_replacement`, `budget_exhausted`, `unresumable`, `not_supported`, and
admission error codes unchanged.

### Replacement Schema

Remove the `cc55cf57` tool-schema statement that `unresumable` also matches
parent-end, explicit-cancel, and stall codes. The allowed enum remains:

- `unresumable`;
- `budget_exhausted_continue`;
- `not_supported`;
- `admission_failed`; and
- `admission_unknown`.

`replacement_reason_matches_source` is no longer an isolated error-code
matcher. Replacement admission consumes the central policy decision.

## End-to-End Flows

### Audited Unexpected Disconnect

1. The parent connection terminates without a registered destructive request.
2. Lifecycle evidence proves transport/process/session loss.
3. The child run settles canceled with that audit.
4. Status projects automatic `continue/unexpected_continue`.
5. Continue reserves and resumes through `ResumeExistingOnly`.
6. The unexpected-continue counter is charged only when the prompt is admitted
   and the new run promotes to running.

### Historical NULL-Audit Parent Disconnect

1. Status classifies the source as `legacy_parent_disconnect` and proposes
   confirmation-required continue.
2. A direct continue returns `recovery_confirmation_required` without a run.
3. The authorization tool presents the fixed recovery card.
4. User approval creates a ten-minute, continue-only authorization.
5. Continue consumes it transactionally and calls `ResumeExistingOnly` with
   the original external session.
6. Success reuses the child; no replacement budget is consumed.

### Authorized Continue Becomes Unresumable

1. An authorized continue reserves a new run.
2. Resume/load proves the external session cannot be loaded.
3. The new run settles `failed/unresumable` and retains the authorization id.
4. The prior canceled task is no longer the latest source.
5. A separate same-key replacement targets the failed continue run with
   `replacement_reason=unresumable`.
6. Normal replacement ownership, identity, latestness, and budget checks apply.

### Explicit Cancellation

1. `parent_canceled` or `user_cancelled` settles with explicit-cancel audit.
2. No automatic continue or replacement is allowed.
3. User approval authorizes the policy-proposed action.
4. With established resume identity and budget, the proposed action is
   continue, not replacement.
5. Decline leaves the work unit stopped and all counters unchanged.

### Stall or Execution-Ambiguous Termination

`tool_stalled_timeout`, `join_abandoned`, `parent_turn_failed`, malformed
legacy evidence, and `admission_unknown` all present cause-specific risk copy.
The first three prefer authorized continue when possible. `admission_unknown`
requires an authorized replacement because the prompt may have executed but no
safe resume admission exists.

### Pure Generation-1 Pre-Admission Abort

An unbound generation-1 run with no prior established lineage that never
admitted a prompt does not establish lineage. Infrastructure aborts can
same-key fresh-dispatch automatically. Explicit, parent-end, stall, or unknown
causes require user authorization, but the admitted action remains fresh
dispatch and consumes no recovery rail.

### Pre-Admission Host Restart Exception

The existing safe exception remains:

- a reserving host-restart row with no bound `child_connection_id` but a
  complete external resume identity may continue and inherit `normal_revision`
  or `unexpected_continue`;
- a replacement-class source does not move to continue and must retry through
  the replacement path; and
- a bound reserving row whose prompt may have executed remains
  `admission_unknown`, not a fresh abort.

### Pre-Admission Attempt in an Established Lineage

A later-generation continue run, or a generation-1 replacement run attached to
an existing lineage, can terminate before its own `reached_running_at` is set.
That does not make the work unit fresh:

- if the backend can prove the prompt was not admitted, retry the same continue
  or replacement rail and inherit its admission class/replacement reason;
- retain the cause's confirmation requirement and any valid authorization
  provenance;
- do not charge a counter until a retry actually promotes to running; and
- if prompt admission is uncertain, classify the row as `admission_unknown`
  and require an authorized replacement.

No branch in this case performs an unlinked same-key first dispatch.

## Concurrency and Failure Handling

| Race/failure | Required result |
| --- | --- |
| two authorization requests for one source state | reuse the unique pending/approved challenge |
| source changes while card is open | fingerprint mismatch; authorization is stale |
| two calls consume one authorization | one conditional update wins |
| continue and replacement race | shared policy, active-run fence, and transaction allow one |
| approval followed by parent reconnect | approved row remains usable by the same parent conversation until TTL |
| parent disconnects before answering | pending challenge becomes abandoned |
| terminal audit and later parent end race | first terminal CAS keeps its evidence |
| authorization transaction fails | neither run nor consumption commits |
| post-commit spawn/resume fails | authorization stays consumed; durable run records the actual failure |
| audit serialization fails | terminal write fails loudly; it must not persist a new canceled row with NULL audit |
| status policy read fails | return existing safe unavailable/unknown surface and emit structured internal cause |

Authorization does not reserve a budget while the card is open. A user can
leave the card pending without blocking unrelated work units.

## Cold Status and Observability

The earlier session also observed `get_delegation_status` returning `unknown`
for a task that later proved to exist in the database. This is not the direct
resume/replacement cause, but recovery depends on trustworthy cold status.

The public API may continue returning `unknown` for both not-found and
not-owned tasks to avoid an ownership oracle. Internally, every cold lookup miss
emits one stable reason:

- `db_not_found`;
- `ownership_mismatch`;
- `token_parent_mismatch`;
- `store_error`; or
- `prefix_ambiguous` when prefix resolution applies.

Add structured recovery lifecycle events:

- `recovery.decision`;
- `recovery.confirmation_requested`;
- `recovery.confirmation_approved`;
- `recovery.confirmation_declined`;
- `recovery.authorization_consumed`;
- `recovery.authorization_rejected`;
- `recovery.resume_failed`; and
- `recovery.replacement_admitted`.

Logs include stable task/authorization ids, parent/child ids, action, cause,
risk class, and rejection code. They do not include task prompts, arbitrary
error text, answer prose, or external session ids.

## Database Migration and Compatibility

The coordinated migration adds:

1. shared `recovery_authorizations` and its active-challenge/expiry indexes;
2. nullable `delegation_task_runs.recovery_authorization_id`; and
3. nullable `conversation.last_termination_audit_json` if the earlier
   termination-audit design has not already introduced it.

The workflow companion adds its manifest/gate provenance columns in the same
migration series. Neither policy owns a second authorization table.

It does not rewrite existing run status, error code, audit JSON, counters, or
lineage links. Legacy NULL audit remains NULL and is classified at read time.

The migration is additive. An older binary can ignore the added table/columns,
although rolling application code back to `cc55cf57` would also restore its
unsafe policy and is not a behavioral rollback strategy.

`codeg-mcp` embeds its schema at process start. A companion process launched
before the upgrade does not know the authorization tool and must reconnect to
refresh its MCP catalog. The host does not retain the broad `unresumable`
fallback for old companions.

Desktop and server modes use the same store, policy, authorization, question,
and broker core. Only event delivery remains runtime-specific.

## Implementation Boundaries

The design expects focused ownership boundaries:

- a delegation recovery-policy module for typed classification and pure
  decisions;
- a shared recovery-authorization module for entity/store/service behavior;
- database migration and entities for authorization/provenance fields;
- termination-audit and broker settlement changes for typed evidence;
- `run_store` integration for atomic policy/admission/authorization handling;
- request/report types and companion schema/dispatcher changes;
- existing question registry/card integration for the fixed recovery prompt;
- i18n messages for all supported locales; and
- updates to the brainstorm-to-delivery Skill and the amended/companion design
  docs.

Do not combine this work with unrelated broker decomposition or workflow/UI
refactoring. Workflow lifecycle decisions remain in the policy and store
defined by `2026-07-30-workflow-blocked-recovery-design.md`.

## Testing Strategy

### Policy Unit Tests

Use table-driven tests over:

- terminal status and error-code consistency;
- reached-running and pure-pre-admission classification;
- audit source, reason, classification, NULL, and malformed forms;
- launch snapshot and external-session identity;
- agent resume capability;
- admission class;
- continue/replacement budgets; and
- requested operation.

Required negative cases include:

- `failed + parent_disconnected`;
- `completed + cancel code`;
- intentional teardown audit presented as `parent_disconnected`;
- malformed audit presented as unexpected infrastructure;
- authorization presented for a different action/reason;
- authorization presented after source supersession;
- confirmation attempting to override route/policy/auth failure;
- pre-running parent/cancel rows attempting to consume replacement;
- a pre-running later-generation continue attempting to cold-dispatch as a new
  generation-1 work unit; and
- a pre-running replacement retry attempting to switch onto the continue rail.

### Termination Audit Tests

Every terminal producer named in this design must prove that a newly canceled
row has typed audit. Race tests prove that an earlier child terminal is not
overwritten by parent teardown and that run/conversation projections commit
together.

### Authorization Tests

Cover:

- approve, decline, dismiss, abandon, expire, and reconnect;
- cross-parent, cross-task, cross-child, and cross-work-unit rejection;
- stale fingerprint and changed policy action;
- concurrent request deduplication;
- concurrent single-use consumption;
- transaction rollback before run insertion;
- post-commit failure keeping the authorization consumed; and
- no authorization id leaking from ordinary status.

### Broker End-to-End Tests

Cover real broker paths rather than only the pure matcher:

- audited unexpected continue through `ResumeExistingOnly`;
- legacy NULL-audit confirmation followed by continue;
- external session id remains unchanged on successful resume;
- unexpected budget charges only at running promotion;
- explicit cancel cannot continue without authorization;
- direct `unresumable` replacement of cancel/parent/stall codes is rejected;
- actual resume failure persists `unresumable`, then permits replacement;
- pre-running fresh dispatch consumes no recovery rail;
- pre-running continuation/replacement attempts preserve their existing rail
  when earlier lineage is established;
- bound host-restart continuation inherits the original non-replacement class;
- `admission_unknown` requires authorized replacement; and
- desktop/server paths call the same core.

### MCP, Question, and Frontend Tests

Cover tool discovery, input validation, stable structured results, fixed
server-owned question choices, localization keys, approve/decline rendering,
and removal of the dangerous `unresumable` description. Existing generic
question behavior must remain unchanged.

### Cold Status Tests

Prove that a DB-only task owned by the same parent conversation is returned
after process restart. Wrong ownership still returns public `unknown`, while
test instrumentation observes the correct internal reason.

### Migration Tests

Run migration from a database containing:

- historical NULL-audit parent-disconnect rows;
- established work-unit and lineage budgets;
- a pre-running pure abort;
- a bound `admission_unknown` row; and
- existing replacement chains.

Assert that no existing durable value changes and that active authorization
uniqueness works after repeated migration setup.

## Acceptance Scenario

The observed task `965a94c4-f7e5-4337-807f-2aa5b1efe95f` is a fixed regression
fixture or equivalent reconstructed database scenario.

1. Status returns `confirmation_required`, cause
   `legacy_parent_disconnect`, proposed action `continue`.
2. A direct continue without authorization creates no run and changes no
   counter.
3. Decline leaves the task canceled and all counters unchanged.
4. Approval issues a continue-only authorization.
5. Continue consumes it and invokes `ResumeExistingOnly` with the original
   external session identity.
6. Successful running promotion charges one unexpected continue and zero
   replacements.
7. If resume/load fails, the new latest run becomes `failed/unresumable` and
   retains authorization provenance.
8. Only then can a separate `unresumable` replacement consume the single
   replacement rail.

## Rollout and Verification

The combined delegation/workflow implementation order is dependency-driven:

1. additive shared authorization/provenance migration and typed contracts;
2. workflow binding-lifecycle correction from the companion design;
3. audit-complete delegation terminal producers;
4. separate pure delegation/workflow policies and decision-table tests;
5. shared authorization service, tool, and fixed question integration;
6. workflow state-only revision and gate-settlement authority changes;
7. continue/fresh/replacement integration through the delegation policy;
8. workflow recovery and Plan lineage-reset integration;
9. removal of `cc55cf57` broad matchers and Skill/schema wording; and
10. final combined end-to-end and full repository verification.

The behavioral cutover ships atomically. Do not remove the compatibility
matcher before the authorization route exists, and do not ship both the broad
matcher and the new policy as selectable alternatives.

During implementation, run focused tests/checks needed to develop and diagnose
the current component. Do not repeatedly run the full long-running repository
matrix after intermediate steps. Run the complete validation once, after all
implementation, migrations, docs, schema, and tests are finished:

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

Any failure in that terminal matrix is fixed before completion is claimed; the
full matrix is then rerun only as needed to verify the final fix set.

## Completion Criteria

The feature is complete only when:

- all recovery entry points consume the central policy;
- no cancellation/parent/stall code aliases `unresumable`;
- newly canceled delegation runs have typed termination audit;
- explicit and ambiguous recovery cannot proceed without a consumed
  server-issued authorization;
- delegation and workflow subjects share one authorization service but cannot
  cross-consume actions;
- valid resume identity enforces continue before replacement;
- pre-admission host-restart and pure-abort behavior remain distinct;
- the observed stuck-task scenario passes end to end;
- cold DB status has reason-coded internal diagnostics;
- the workflow companion's session-2566 acceptance fixture recovers in place;
- Skill and tool schema describe the same policy as the backend; and
- the one final full validation matrix passes.
