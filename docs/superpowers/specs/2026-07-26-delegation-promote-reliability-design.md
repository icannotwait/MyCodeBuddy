# Delegation Promote Reliability Design

Date: 2026-07-26

Status: Approved by design review group (2026-07-26 r3: Grok, CodeBuddy:KimiK3,
Codex). Minors resolved in-doc; ready for implementation planning.

## Summary

Fix the post-prompt delegation admission failure path that can turn a
successfully enqueued child prompt into a durable `spawn_failed` run. The
selected design makes `RunStore::promote_running` write-first, keeps recovery
budget charging in the same transaction, gives generation 2 and later runs
their own accepted timestamps, reliably settles failures, and adds explicit
recovery for known and ambiguous admission failures.

The normal path will not depend on retries. A write-first claim prevents the
deferred SQLite transaction from first taking a read snapshot and later
failing to upgrade that stale snapshot with `SQLITE_BUSY_SNAPSHOT`. Two short
retries remain only as a bounded defense against ordinary `SQLITE_BUSY` and
`SQLITE_LOCKED` contention from another writer.

**Gen-1 must durable-bind the child connection before prompt enqueue**,
matching continuation's existing `bind_child_connection_while_reserving`
ordering. Without that, an unbound `reserving` row after host restart cannot
be treated as "safe pre-admission."

This design is provider-neutral. It does not add CodeBuddy-specific
concurrency limits, GLM-specific behavior, or a separate home directory for
each child.

## Incident And Evidence

The observed failure sequence is:

1. The child prompt is accepted by the ACP send path.
2. The broker calls `RunStore::promote_running`.
3. Promote fails before the durable run reaches `running`.
4. The broker actively cancels and disconnects the child.
5. The local child reports `TurnComplete(cancelled)`.
6. The broker persists or reports the run as `spawn_failed`.

The continuation path currently does not emit a structured log for the
original promote error (the error still reaches the wire report via
`store_err_to_delegation_error`), and both generation-1 and continuation
branches discard some settlement results with `let _ = settle_terminal(...)`.
That makes the final `spawn_failed` indistinguishable from a process launch
or prompt-send failure.

The strongest database explanation is `SQLITE_BUSY_SNAPSHOT`: the current
promote transaction reads the run before its first write. SeaORM opens a
deferred SQLite transaction, so a concurrent writer can commit after that
read. SQLite then cannot upgrade the stale read snapshot to a writer. The
same repository already documents and uses a write-first transaction for
fork persistence in `acp/manager.rs` to prevent this exact failure mode.

The continuation branch does not log its promote error, so the historic
incident cannot be proven down to the SQLite extended error code. The design
therefore fixes both the likely root cause and every failure-handling defect
that currently hides or amplifies it.

### Code-backed path facts (as of review baseline)

| Path | Durable bind before send? | Current promote-failure settle code |
| --- | --- | --- |
| Continuation | **Attempted, not fail-closed** — `begin_run_admission` calls `bind_child_connection_while_reserving` but warn-and-continues on error (and ignores non-matching already-bound) | non-budget → `spawn_failed`; settlement often discarded |
| Generation 1 | **No** — in-memory reserve only; durable `child_connection_id` is written inside `promote_running` | non-budget → `spawn_failed`; settlement discarded |

Gen-1 post-accept / pre-promote host crash therefore leaves durable
`reserving` with `child_connection_id IS NULL` even though the prompt was
accepted. Any recovery model that treats "unbound reserving" as safe
pre-admission is wrong for gen-1 until bind-before-send lands.

## Goals

- Prevent normal `promote_running` operation from producing
  `SQLITE_BUSY_SNAPSHOT`.
- Keep run promotion, recovery-budget charging, accepted timestamps, and the
  latest conversation projection atomic.
- Retry only bounded ordinary SQLite writer contention.
- Give post-prompt promotion failures a precise durable outcome.
- Never discard a terminal settlement result.
- Preserve settlement retry ownership until durable truth is known.
- Give a caller an explicit recovery path without automatically replaying a
  prompt that might already have executed.
- Give every run generation its own accepted timestamp.
- Count accepted generations and failures by stable, low-cardinality labels.
- Preserve first-terminal-wins behavior across cancel, child terminal,
  disconnect, promote, and settlement races.
- Make gen-1 and continuation share the same pre-send durable-bind contract
  so crash recovery can distinguish pre-send from post-send reserving rows.

## Non-Goals

- Adding a CodeBuddy or GLM concurrency limit.
- Giving every child process a separate home directory.
- Serializing all delegation work behind a global application mutex.
- Automatically resending a prompt after a post-send failure or ambiguous
  process restart.
- Providing exactly-once execution across every ACP provider. That would
  require a provider-side idempotency key or accepted-prompt query protocol
  that ACP does not currently define.
- Migrating historic `spawn_failed` rows to a new error code.
- Changing frontend delegation-card presentation (unknown codes keep the
  existing default badge). Locale keys for the new codes may be a follow-up.
- Refactoring unrelated spawn, continuation, or persistence code (including
  making `settle_terminal` write-first; residual BUSY_SNAPSHOT there remains
  covered by the existing settlement retry kernel).

## Terms And Durable Invariants

**Prompt accepted** means the current prompt was successfully placed onto the
child connection's command path. Its wall-clock sample is
`prompt_accepted_at`.

**Durably accepted** means the run's `reserving -> running` transaction has
committed. Only this boundary may produce a running acknowledgement or an
accepted metric.

**Admission failure** means a prompt was accepted but its run could not be
durably promoted. It is distinct from process spawn failure and prompt-send
failure.

**Admission outcome unknown** means the host restarted while a bound run was
still `reserving`. The prompt may or may not have crossed the external side
effect boundary.

The implementation must preserve these invariants:

1. A running acknowledgement implies a committed `running` run.
2. Recovery budget is charged exactly once and only in the transaction that
   commits `running`.
3. A failed promote leaves both the run and all budget counters unchanged by
   that transaction.
4. A run that never reached `running` never qualifies for automatic prompt
   replay solely because its prompt might have failed.
5. A known terminal winner is never overwritten by `admission_failed`.
6. Once a terminal settlement starts, either durable settlement or a retry
   record owns it before live coordination is released.
7. Conversation projection is monotonic by generation.
8. Accepted metrics are emitted only after the durable accepted boundary.
9. **No prompt enqueue may occur for a run whose durable row lacks
   `child_connection_id`.** Gen-1 and continuation share this pre-send bind
   contract. Bind failure is fail-closed: do not send the prompt.

## Selected Architecture

### 0. Pre-send durable bind for **both** gen-1 and continuation (required)

Today:

- Continuation calls `bind_child_connection_while_reserving` inside
  `begin_run_admission`, but on bind **error** only logs a warning and still
  returns success — so a prompt can be sent while the durable row remains
  unbound.
- Gen-1 does not durable-bind before send at all; the first durable
  `child_connection_id` write is inside `promote_running`.
- The bind helper treats "already bound to a **different** connection" as
  silent success ("first bind wins"), which can leave the live path holding a
  connection id the durable row does not own.

This design **changes** both paths so invariant 9 is real:

1. After the child process/session exists and the task id is known, call
   `RunStore::bind_child_connection_while_reserving(task_id, child_connection_id)`
   (gen-1 must add this; continuation keeps the call site but must not ignore
   errors).
2. Bind success means either:
   - rows_affected == 1 (first bind of this connection while reserving), or
   - idempotent already-bound to **this same** `child_connection_id`.
3. Bind **failure** (DB error, not reserving, missing row, or already bound to
   a **different** connection) is **fail-closed**:
   - unwind live registration / reservation / inflight handoff as appropriate;
   - do **not** enqueue the prompt;
   - settle/report a pre-admission failure (`spawn_failed` or the existing
     pre-send cancel/error path — not `admission_failed`);
   - disconnect if a child connection was opened and will not be used.
4. `begin_run_admission` / `begin_run_admission_transfer` must **surface bind
   failure** to the caller (typed reject or `Result`), not warn-and-continue.
   Continuation and gen-1 share this contract.
5. Update `bind_child_connection_while_reserving` so a different already-bound
   connection returns a typed permanent ownership conflict, **not** `Ok(())`.

Promote then **retains** the already-bound connection; it must not be the
first writer of `child_connection_id` on the success path. Promote may still
set `child_connection_id` only when the claim row already has the matching
id (ownership check), or leave the column unchanged when equal. A null or
mismatched connection on promote is a typed ownership/state conflict, not a
generic `admission_failed` rewrite of an unrelated terminal.

This makes "unbound reserving" mean **pre-send** for both generations.

### 1. Write-first promote transaction

`RunStore::promote_running` will execute the following sequence in one SQLite
transaction:

1. Its first SQL statement performs a claim write against the target run,
   filtered by `task_id`, `status = reserving`, and the expected
   `child_connection_id` (always present after successful pre-send bind).
   Updating `updated_at` is sufficient to acquire the SQLite writer lock.
   This write is rolled back if any later step fails.
2. Require `rows_affected == 1`. A zero-row result is a typed state conflict,
   not an unstructured permanent database error.
3. Read the run while holding the writer lock and validate its bound child
   connection and immutable admission metadata.
4. Charge `UnexpectedContinue` or `Replacement` recovery budget according to
   the durable `admission_class`. `NormalRevision` has no charge.
5. Update the run to `running`, set its accepted and reached-running
   timestamps, and retain the already-bound child connection.
6. Project the current generation onto the child conversation in the same
   transaction. The projection sets:
   - current generation fence;
   - conversation status = `InProgress` (running);
   - `delegation_started_at = prompt_accepted_at`;
   - clears prior terminal `error_code` and `finished_at` (write NULL);
   - resets generation-scoped runtime rollup fields.
7. Commit.

**Clearing terminal finish fields:** `ConversationProjection` today cannot
express "write NULL" for `finished_at` (`None` means leave unchanged). This
work extends the projection representation with a nested
`Option<Option<DateTime>>` (or an equivalent clear flag), matching the
existing pattern used for rollup fields such as `additions`/`deletions`.

No other writer can interpose between the claim write, read, budget charge,
run update, and conversation projection. A budget refusal, ownership
invariant failure, projection hard failure, or commit error rolls back the
complete transaction.

**Generation fence soft-miss:** if `project_conversation_in_txn` returns
`Ok(false)` because a newer generation already owns the conversation row,
treat that as a typed state conflict and roll back the promote transaction
(do not leave the run `running` under a stale generation claim). Equal-
generation idempotent re-project remains `Ok(true)` / success.

The promote API will return a typed result rather than requiring callers to
parse error strings. Preferred shape: a dedicated `PromoteRunningResult`
(or equivalent enum) distinct from stringified `TaskStoreError`. At minimum
it must distinguish:

- promoted;
- already running for the same run/connection, if an idempotent reread proves
  that state;
- a durable terminal winner;
- budget exhausted;
- state/ownership conflict;
- retryable SQLite busy or locked;
- permanent persistence failure;
- ambiguous commit outcome requiring durable reread (see below).

For a zero-row claim, the implementation rereads durable truth **outside**
the rolled-back transaction. An already-running matching run is idempotent
success. A terminal run returns the existing terminal winner. A missing row
or mismatched owner is a typed conflict. This prevents a cancellation race
from being rewritten as an admission failure.

**Commit-ambiguity policy:** if the transaction returns a permanent/ambiguous
error after the claim write may have committed (for example SQLite I/O at
commit/checkpoint), the caller **must reread** durable truth:

- matching `running` for this task/connection → treat as **promoted**
  success (budget was charged exactly once by that commit);
- terminal winner → preserve and replay that winner;
- still `reserving` / missing / mismatched → classify as permanent
  `admission_failed` and follow the failure helper.

Never cancel-and-settle `admission_failed` over a committed matching
`running` row.

### 2. Bounded transient retry (promote-local policy)

Promote uses a **dedicated** retry policy, not
`PersistenceRetryPolicy::production()` (which is 4 attempts / 25 ms base and
is shared by settlement). Changing the shared policy would silently alter
settlement behavior.

Production promote policy:

- three total attempts (initial + two retries);
- fixed delays of 10 ms then 25 ms between attempts;
- retries only ordinary SQLite `BUSY` (primary code 5) or `LOCKED`
  (primary code 6) that are **not** classified as `BUSY_SNAPSHOT`.

`BUSY_SNAPSHOT` (SQLite extended code 517 when extractable) should be
eliminated by write-first ordering. If observed:

- log it distinctly as an invariant regression with primary + extended codes
  when available;
- still use the same bounded retry rail (defensive);
- metric label `busy_snapshot` when extended code 517 is detected; otherwise
  fall back to ordinary `busy` and still emit the regression log if the
  error text/context strongly suggests snapshot upgrade failure.

**SQLite classification:** extract primary/extended codes by downcasting the
SeaORM/sqlx `DbErr` **before** stringification. Do not rely solely on
`is_transient_sqlite` string matching, which collapses busy/locked/snapshot.
If codes are unavailable, fall back to the existing string heuristics and
count as ordinary `busy`/`locked` without inventing a false
`busy_snapshot` metric.

Budget exhaustion, state conflict, not-found, invariant failure, and other
permanent errors are not retried.

The result reports retry count and final failure class so the broker can emit
metrics without duplicating transaction policy in generation-specific code.

### 3. Per-generation accepted timestamps

`AcceptedDelegationPrompt` will carry a fresh `prompt_accepted_at` sampled for
the current prompt after the send path accepts it. It will no longer query the
conversation row for `delegation_started_at` after prompt enqueue.

Promote inputs should carry both:

- `prompt_accepted_at` (from the accept path);
- `promote_at` (sampled at promote entry, clamped so
  `promote_at >= prompt_accepted_at`).

Whether these are separate arguments or a small promote-request struct is an
implementation detail; the timestamp math is mandatory.

The promote transaction persists:

- run `started_at = prompt_accepted_at`;
- run `reached_running_at = promote_at`;
- conversation `delegation_started_at = prompt_accepted_at` under the
  generation projection fence.

The run's pre-promote start remains provisional and is overwritten at
promotion. Live runtime statistics are rebased to the same
`prompt_accepted_at` before publication. This gives generation 2 and later a
start distinct from generation 1 and removes a post-send database lookup that
can itself fail after the external side effect.

No schema migration is required. The relevant timestamp columns are already
nullable and existing historic values remain unchanged.

### 4. Shared post-prompt failure helper

Generation 1 and continuation will call one broker helper for every promote
failure after prompt acceptance. The helper owns error classification,
cancellation, settlement, cleanup, reporting, and observability.

The classification is:

| Promote outcome | Durable terminal code | Recovery behavior |
| --- | --- | --- |
| Existing terminal winner | Preserve existing truth | Replay winner into caller report; still request cancel + disconnect on the just-accepted child (idempotent teardown); do not overwrite durable terminal |
| Matching already-running (idempotent / commit reread) | N/A — success | Return running path; no cancel |
| Budget exhausted | `budget_exhausted` | No automatic replay |
| Retry exhausted | `admission_failed` | Explicit replacement only |
| Permanent persistence/invariant failure (still reserving after reread) | `admission_failed` | Explicit replacement only |
| Unresolved ownership/CAS conflict | `admission_failed` | Explicit replacement only; log conflict class |

`spawn_failed` remains reserved for failures **before** prompt acceptance,
such as child process/session launch failure, pre-send bind failure, or
prompt delivery failure that did not cross the accepted boundary.

Wire mapping: `store_err_to_delegation_error` / cold reports / audit tables
must surface `admission_failed`, `admission_unknown`, and `budget_exhausted`
as distinct codes. Post-accept promote failures must never collapse to
`spawn_failed` on the wire.

#### Executable settlement ownership protocol

For a **newly classified** admission/budget failure (no durable terminal
winner, not already-running):

1. Claim local first-terminal disposition for this task/generation (same
   first-terminal-wins coordination used elsewhere) so a concurrent
   cancel/child-terminal path cannot install an incompatible retry payload.
2. Cancel the accepted child prompt. Cancellation is idempotent; failure is
   logged but does not block settlement.
3. Attempt durable terminal settlement with the **intended** terminal code
   (`admission_failed` or `budget_exhausted`) using the existing bounded
   settlement attempts. **Do not** use the `settle_task` path that rewrites
   exhausted settlement to `persistence_error`.
4. On settlement success (`Won` or `Existing`):
   - if `Existing`, the caller report must use the durable winner's code and
     identity, not the promote-error class when they differ;
   - disconnect the child;
   - release live coordination.
5. On transient settlement exhaustion:
   - install one process-local `PendingTerminalRetry` with the **original**
     intended terminal payload (bootstrap-style: claim ownership before
     releasing coordination — follow the reliable bootstrap settle pattern
     already used for unresumable/bootstrap, not the PE-rewrite arm);
   - if an existing retry record already owns a **different** terminal
     payload, treat that as first-terminal-wins: adopt/observe that owner and
     do not overwrite;
   - disconnect may proceed after cancel has been requested and the retry
     record (or durable terminal) owns the write;
   - release live coordination only after Won / Existing / owned retry
     record.
6. Permanent settlement failure **must still leave a process-local ownership
   marker** before coordination release. Preferred order (bootstrap-aligned):
   - claim local first-terminal disposition **and** install
     `PendingTerminalRetry` with the intended admission/budget terminal
     **before or as part of** the first settlement attempt when the helper
     cannot prove durable terminal yet; **or**
   - if settlement returns permanent failure without a prior retry record,
     install a frozen ownership record with the intended terminal immediately,
     then report sanitized `persistence_error`.
   Never release live coordination after a permanent settle miss with neither
   durable terminal nor freeze/retry ownership. Immediate caller receives
   sanitized `persistence_error` while the frozen record remains the
   process-local ownership marker.

The permanent-failure finalizer and retry worker must recognize
`admission_failed` and `budget_exhausted` as same-owner intended payloads
(alongside existing `unresumable` / `persistence_error` recognition).

Cancellation-induced `TurnComplete(cancelled)` participates via the normal
first-terminal-wins rules: if it commits first, the helper's settlement
becomes `Existing` and must replay that winner rather than overwriting it
with `admission_failed`.

The affected branches must not use `let _ = settle_terminal(...)` or
otherwise discard settlement or `put_retry` ownership results that decide
coordination release.

### 5. Settlement retry ownership

Admission-failure settlement reuses the existing persistence retry kernel and
`PendingTerminalRetry` single-flight ownership. Its retry payload must retain
the intended durable terminal (`admission_failed` or `budget_exhausted`); it
must not rewrite that payload to `persistence_error`.

If initial bounded settlement attempts exhaust on a transient error, one
process-local retry record is installed before coordination is released. Its
worker continues retrying the exact terminal payload until it wins or observes
an existing terminal winner. Metrics and terminal audit are emitted only for
the durable CAS winner.

A permanent settlement failure freezes the retry record instead of spinning.
**Startup backstop:** `PendingTerminalRetry` is **process-local** and does not
survive host restart. After restart, any still-non-terminal `reserving` run is
handled only by durable reconcile (§7): unbound → safe `host_restarted`;
bound → `admission_unknown`. Do not assume retry-record replay across process
death. Within a live process, freeze/retry ownership remains mandatory before
coordination release.

### 6. Known admission-failure recovery

Add stable error code `admission_failed` and stable replacement reason
`admission_failed`.

The replacement reason matches only a source run with all of the following:

- `status = failed`;
- `error_code = admission_failed`;
- `reached_running_at IS NULL`;
- it is the **lineage-latest replaceable source** for the work unit (see
  below);
- the existing ownership, snapshot, agent, workspace, and work-unit guards
  still pass.

**"Latest source" across replacement children:** a successful replacement
creates generation-1 on a **new** child conversation. The superseded source
must not remain replaceable forever merely because it is still the latest row
on its **old** child. Matching therefore requires:

- source is latest on its own child conversation (existing check), **and**
- source has not been superseded by a durable replacement lineage edge
  (`replaced_task_id` / lineage root): if any later run in the same work-unit
  lineage was created with `replaces_task_id = source.task_id` (or the
  platform's equivalent lineage pointer) and is not itself a pure
  pre-admission abort that left no successor, the source is stale.

If the codebase already has a work-unit / lineage-latest helper, reuse it;
otherwise add an explicit superseded-source fence tested as: source A is
replaceable → replacement B is created on another child → a further attempt
to replace A is rejected as stale.

Recovery is always an explicit `delegate_to_agent` replacement. It never
automatically resumes or resends the old prompt and must not be represented as
`unresumable`.

The existing replacement budget applies. Because budget is charged only by a
successful promote transaction, a failed pre-running replacement consumes no
budget and may itself be explicitly replaced. Exactly one eventual successful
replacement promotion consumes one replacement unit.

### 7. Crash-ambiguous recovery

There is an unavoidable crash window between prompt enqueue and durable
promote commit. Without a provider-side idempotency token or accepted-prompt
query, a restart cannot prove whether the prompt executed.

With §0 gen-1 pre-send bind, startup reconciliation splits `reserving` runs:

- An unbound run, with no `child_connection_id`, is known not to have reached
  the child send phase. It retains the existing safe pre-admission
  `host_restarted` recovery behavior.
- A bound run is settled `failed/admission_unknown`, with structured audit
  recording prior status `reserving` and restart provenance. Binding precedes
  prompt send, so this category deliberately includes some false positives
  (bound but pre-send) in order to prevent automatic duplicate execution.

`admission_unknown` is not continuable and is never automatically replayed.
Add explicit replacement reason `admission_unknown`, matching only the latest
lineage-eligible `failed/admission_unknown` source with
`reached_running_at IS NULL` and the normal replacement ownership/snapshot
guards. Superseded sources are rejected as in §6.

**Defense in depth:** `is_revision_eligible_failure` / continue eligibility
must explicitly deny-list `admission_failed` and `admission_unknown` so a
future `reached_running` invariant drift cannot silently make them
continuable. Non-continuability must not rest solely on the
`reached_running_at IS NULL` gate.

#### Caller-facing duplicate-execution warning

The design requires a concrete safety delivery path (not only prose):

1. Cold/final task reports for `error_code = admission_unknown` must include
   an explicit message that the prior prompt **may already have executed**
   and must not be auto-continued. Implement in the report builder path
   (for example `types.rs` cold-report special-cases currently limited to
   `unresumable`).
2. Successful **replacement** acknowledgements that used
   `replacement_reason = admission_unknown` must carry the same warning in
   the replacement response / tool result text so the parent agent sees it
   before relying on the new child.
3. Tool schema / companion description text should document that
   `admission_failed` and `admission_unknown` recover only via explicit
   replacement, never continue.

Acceptance tests must assert the warning string (or stable warning token)
appears on both the failed report and the replacement acknowledgement paths.

### 8. Metrics and structured logs

`accepted_count` will mean the number of run generations that crossed the
durable accepted boundary, including continuations. It increments only for
the successful `reserving -> running` winner. Add `accepted_by_agent`, keyed by
the existing stable agent labels, and use the `agent_type` parameter currently
ignored by `record_accepted`.

Idempotent already-running reread (including commit-ambiguity recovery that
observes matching `running`) must **not** double-count `accepted_count` if a
prior accepted metric for that same run generation was already emitted. If
the original promote attempt never emitted accepted metrics, the success
recovery path emits exactly once.

Add these low-cardinality snapshot maps/counters:

- `promote_retries`: `busy`, `locked`, and `busy_snapshot`;
- `promote_failures`: `cas`, `budget`, `busy_exhausted`, and `permanent`;
- `admission_failed_by_agent`;
- `settlement_retry_enqueued`;
- `settlement_retry_exhausted`.

`promote_failures.busy_exhausted` counts only lock-class retry exhaustion,
not permanent non-retryable failures after a single attempt.

`settlement_retry_enqueued` increments only when a caller acquires a **new**
single-flight retry record **because the initial bounded settlement loop
exhausted** (or because a permanent miss installs a freeze ownership record
that will not be removed by an immediate success). If a pre-settlement
ownership record is installed for race fencing and the first settlement then
wins immediately, remove/clear that record without counting
`settlement_retry_enqueued`. `settlement_retry_exhausted` increments when the
initial bounded settlement loop fails and durable truth is handed to a new or
already-existing retry owner. Therefore:

- new owner after exhaust → both counters increment;
- existing owner after exhaust → only `settlement_retry_exhausted` increments;
- ownership fence removed after immediate settle success → neither exhaust
  counter.

A later permanent worker failure is reported by the existing permanent-
persistence path and structured freeze log; transient workers otherwise
continue under their existing policy.

Do not place task ids, connection ids, work-unit keys, paths, or raw errors in
metric labels.

Snapshot serde must keep existing fields and give new maps default-empty
values so process-local consumers do not break.

Each promote retry and final failure log includes:

- `task_id`;
- `generation`;
- `agent_type`;
- `admission_class`;
- `attempt`;
- SQLite primary and extended error code when available;
- stable failure class.

Settlement retry enqueue, durable completion, and permanent freeze logs use
the same run identity fields. Logs must not include prompt bodies, secrets, or
full configuration values.

`DelegationAuditRecord.error_code` uses interned `&'static str` codes; add
constants for the new codes in the metrics/audit path.

## End-to-End Flows

### Successful generation 1 or continuation

1. Reserve the run durably.
2. Spawn or resume the child.
3. **Durable-bind `child_connection_id` while still `reserving` (fail closed).**
4. Enqueue the prompt and obtain the current `prompt_accepted_at`.
5. Execute the write-first promote transaction, with bounded ordinary-lock
   retry if required.
6. Rebase live runtime state to `prompt_accepted_at`.
7. Install the run in the broker's running state.
8. Emit accepted metrics, transition audit, and running acknowledgement.

### Promote fails after prompt acceptance

1. Classify the typed promote outcome (including durable reread for
   zero-row / ambiguous commit).
2. Preserve a durable terminal winner if one already exists; still request
   idempotent child teardown.
3. Treat matching already-running as success.
4. Otherwise claim local terminal disposition, cancel the accepted prompt,
   and reliably settle `budget_exhausted` or `admission_failed`.
5. Install retry ownership before releasing live coordination when settlement
   has not yet committed.
6. Disconnect and return a sanitized non-running result whose wire code is
   not `spawn_failed`.

### Host restarts with a reserving run

1. Startup reconciliation runs before delegation requests are accepted.
2. Unbound reserving runs follow safe pre-admission restart recovery
   (`host_restarted`) — and with §0, unbound truly means pre-send.
3. Bound reserving runs become `admission_unknown` with the caller-facing
   duplicate-execution warning.
4. No prompt is automatically replayed.
5. A caller may explicitly replace the uncertain run after considering the
   duplicate-execution risk.

## API And Compatibility

- Add `admission_failed` and `admission_unknown` to stable broker/audit error
  handling and report builders (`types.rs` cold reports included).
- Add the same two values to the `replacement_reason` tool schema
  (`tool_schema.json`), listener allow-list (`listener.rs`), and
  `run_store` validation constants / matchers.
- Companion/tool description text documents explicit-replacement-only
  recovery for the new codes.
- Reports continue carrying string `error_code`; no transport shape change is
  required beyond new code strings and warning text.
- Metrics snapshots gain fields but retain existing fields.
- Historic `spawn_failed` and `host_restarted` rows are not rewritten.
- No database migration is required.
- No provider receives special scheduling, retry, or filesystem behavior.

## Implementation Boundaries

Expected production changes:

- `src-tauri/src/acp/delegation/run_store.rs`: write-first promote, typed
  result, atomic projection (including clearable `finished_at`), recovery
  eligibility deny-list, startup bound/unbound split, lineage-latest /
  superseded-source fence helpers as needed;
- `src-tauri/src/acp/delegation/broker.rs`: gen-1 pre-send bind; continuation
  `begin_run_admission` / transfer fail-closed bind (surface `Result` /
  typed reject — includes mechanical updates to existing test call sites);
  shared promote/failure helper; reliable settlement ownership protocol;
  metrics calls; error reporting; commit-ambiguity reread;
- `src-tauri/src/acp/delegation/spawner.rs` and
  `src-tauri/src/acp/manager.rs`: per-prompt accepted timestamp and removal of
  the post-send conversation timestamp lookup;
- `src-tauri/src/acp/delegation/metrics.rs`: low-cardinality counters,
  snapshots, interned audit code constants;
- `src-tauri/src/acp/delegation/tool_schema.json` and matching validation/tests:
  explicit recovery reasons;
- `src-tauri/src/acp/delegation/listener.rs`: replacement_reason allow-list;
- `src-tauri/src/acp/delegation/types.rs`: wire/cold-report mapping and
  `admission_unknown` warning text;
- `src-tauri/src/acp/delegation/store.rs` only if a small typed SQLite
  classifier or promote-local retry constant must live next to existing
  persistence helpers (no settlement policy change).

No unrelated delegation refactor is part of this work.

## Test Strategy

Tests are deterministic. Concurrency tests use barriers, bounded test gates,
and scripted errors rather than timing-sensitive sleeps.

### RunStore transaction tests

- Use a real SQLite file and two connections to force a concurrent writer
  around promote. Verify write-first promote succeeds without
  `BUSY_SNAPSHOT` and the competing writer observes normal serialization.
- Verify one and two ordinary busy/locked failures retry the complete
  transaction and then succeed under the **promote-local** policy.
- Verify retry exhaustion returns the typed failure without a partial run,
  budget, timestamp, or conversation projection write.
- Verify budget exhaustion rolls back the claim and does not increment any
  budget counter.
- Verify a zero-row claim rereads durable truth outside the txn: matching
  running is idempotent, terminal replays the winner, and mismatched
  ownership is a typed conflict.
- Verify commit-ambiguity reread treats matching running as success and does
  not settle over it.
- Verify a successful recovery charge and promote commit together exactly
  once.
- Verify generation 2 overwrites the latest conversation projection while a
  delayed generation-1 projection is rejected and rolls back.
- Verify projection clears prior terminal `error_code` and `finished_at`.

### Broker admission tests

- Assert gen-1 durable-binds before send; bind failure does not enqueue a
  prompt.
- Assert continuation bind failure / different-connection owner mismatch is
  fail-closed: no prompt send, live registration unwound, pre-admission
  error (not silent warn-and-continue).
- For both generation 1 and continuation, script the first promote attempt as
  transient and the next as successful. Assert one prompt send, no cancel,
  no disconnect before normal ownership transfer, one running acknowledgement,
  and one accepted metric.
- Exhaust transient promote retries and assert prompt cancellation, durable
  `admission_failed`, wire code `admission_failed` (not `spawn_failed`), no
  running acknowledgement, and no discarded settlement result.
- Inject budget exhaustion and assert durable `budget_exhausted`, with no
  rewrite to `spawn_failed` or `admission_failed`.
- Race parent cancel and child terminal against promote. Assert the first
  durable terminal wins and the failure helper never overwrites it; caller
  report replays the winner.
- Fail initial settlement transiently, then allow the retry worker to win.
  Assert retry ownership exists before coordination release and the intended
  error code is eventually durable (`PendingTerminalRetry.terminal.error_code`
  remains admission/budget, not PE).
- Inject permanent settlement failure and assert a frozen ownership record,
  sanitized immediate `persistence_error`, no worker spin, and startup
  backstop compatibility.
- Existing-terminal-winner path still requests cancel/disconnect of the
  just-accepted child.

### Recovery tests

- Assert only lineage-latest `failed/admission_failed` runs that never reached
  running match `replacement_reason = admission_failed`.
- Assert only lineage-latest `failed/admission_unknown` runs that never reached
  running match `replacement_reason = admission_unknown`.
- Assert superseded source A is rejected after replacement B exists on another
  child.
- Assert neither code is automatically continuable or replayed; explicit
  deny-list in revision eligibility.
- Assert the reasons cannot be forged against completed, running, reached-
  running, stale, mismatched-agent, or incomplete-snapshot sources.
- Assert failed replacement attempts do not consume budget and exactly one
  successful replacement promote does.
- Reconcile an unbound reserving run and assert the safe pre-admission restart
  behavior remains.
- Reconcile a bound reserving run and assert `admission_unknown`, structured
  audit, no automatic continue, explicit replacement eligibility, and the
  caller-facing duplicate-execution warning.
- Gen-1: after accept and before promote, simulate crash/reconcile with bound
  connection ⇒ `admission_unknown`, **not** continuable `host_restarted`.

### Timestamp and metrics tests

- Send generation 1 and generation 2 through the same conversation and assert
  distinct current prompt timestamps.
- Assert the current run, conversation projection, and runtime statistics use
  the same `prompt_accepted_at`.
- Assert `reached_running_at >= started_at`.
- Seed a stale generation-1 conversation timestamp and verify the next prompt
  does not read or return it.
- Assert successful continuation promote increments `accepted_count` and the
  correct `accepted_by_agent` label.
- Assert idempotent/CAS-loser and commit-reread success paths do not double
  count accepted or terminal metrics.
- Assert retry, final failure, admission failure, and settlement retry metrics
  use only the documented stable labels and the enqueued/exhausted pairing
  rules above.
- Assert `busy_snapshot` is counted only when extended code 517 is extracted.

### Verification commands

Run focused tests while implementing, then complete the Rust verification
matrix from the repository instructions:

```powershell
Set-Location src-tauri
cargo fmt --check
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Frontend checks are not required unless implementation changes a generated or
hand-maintained frontend contract beyond the generic string error code.

## Acceptance Criteria

- Gen-1 **and** continuation durable-bind before prompt enqueue with
  fail-closed errors; unbound reserving after restart truly means pre-send.
- The deterministic concurrent-write regression no longer reproduces
  `BUSY_SNAPSHOT` in promote.
- An ordinary transient writer conflict can recover without canceling or
  resending the prompt.
- A post-prompt promote failure can never return `running` or be mislabeled
  `spawn_failed` on the wire.
- The affected code contains no ignored settlement result that decides
  coordination release.
- A failed settlement retains retry ownership until durable truth or a frozen
  permanent-failure marker exists.
- Automatic startup/continuation logic never replays an admission-unknown
  prompt; cold reports and admission_unknown replacement acks carry the
  duplicate-execution warning.
- Explicit known/unknown admission replacements are narrowly validated
  (including superseded-source rejection) and charged only on successful
  promote.
- Generation 2 and later never reuse generation 1's accepted timestamp.
- Accepted and failure metrics reflect all generations and actual agent type.
- All required Rust checks pass without warnings.

## Rejected Alternatives

### Retry the existing select-first transaction

This may hide some incidents but keeps `SQLITE_BUSY_SNAPSHOT` as normal
control flow. SQLite busy timeout does not solve stale snapshot upgrades, and
retrying does not repair settlement, recovery, timestamp, or observability
defects.

### Serialize all delegation promotion with a global mutex

This reduces local contention but unnecessarily couples independent runs,
does not protect against another process or connection outside the mutex, and
masks the incorrect transaction ordering.

### Automatically continue or replay every failed reserving run

This improves apparent liveness at the cost of duplicate external side
effects. The prompt may already be executing. Explicit replacement is the
only provider-neutral safe recovery contract.

### Add CodeBuddy limits or per-child home directories

Neither addresses the observed post-prompt database transition. These changes
would add provider-specific complexity while leaving the same promote and
settlement bugs available to every agent type.

### Fail-closed all reserving restarts as admission_unknown without gen-1 bind

Safer than today's unbound-as-host_restarted for post-accept crashes, but
needlessly converts true pre-send gen-1 crashes into non-continuable
failures. Prefer bind-before-send so unbound remains a precise pre-send
signal.

## Residual Risk

The write-first transaction removes the known stale-snapshot upgrade pattern,
but ordinary SQLite writer contention can still exceed the short retry window.
That case becomes a precise, recoverable `admission_failed` instead of a
misleading `spawn_failed`.

Exactly-once execution across a host crash remains impossible without a
provider-side idempotency or accepted-prompt lookup protocol. This design
chooses no automatic duplicate execution: bound reserving runs become
`admission_unknown` and require an explicit recovery decision.

`settle_terminal` may remain read-first (non-goal to refactor); its residual
snapshot risk stays behind the existing settlement retry / PendingTerminalRetry
kernel.

## Design Review Adjudication Notes (2026-07-26)

| Source | Verdict | Parent decision |
| --- | --- | --- |
| Grok r1 | Request changes (Critical C1) | **Accept C1.** Gen-1 does not call `begin_run_admission`; bind is inside promote. §0 + §7 + tests added. |
| CodeBuddy:KimiK3 r1 | Approve with fixes (I1–I5) | Accept I1–I5. Note: CodeBuddy's claim that gen-1 already binds pre-send was **incorrect** against code. |
| Codex r1 | Request changes (Important) | Accept warning path, lineage supersession, promote-local policy, settlement protocol, boundaries. |
| CodeBuddy:KimiK3 r2 | **Approve** | Clear for plan writing from this reviewer. |
| Codex r2 | Request changes (Critical bind fail-closed on continue) | **Accept.** §0 now requires fail-closed bind for both paths, typed owner mismatch, and continuation tests. Permanent settle freeze ownership tightened. |
| Grok r2 | Turn failed (non-review response) | Re-review after r2 design patch. |

Minor findings accepted where low-cost: continue deny-list, metrics pairing
rules, projection fence rollback, `InProgress` status, incident wording
accuracy, audit static codes.
