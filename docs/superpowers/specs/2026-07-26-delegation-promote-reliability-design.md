# Delegation Promote Reliability Design

Date: 2026-07-26

Status: Approved in conversation on 2026-07-26; awaiting written-spec review.

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

The continuation path currently discards the original promote error, and both
generation-1 and continuation branches discard some settlement results. That
makes the final `spawn_failed` indistinguishable from a process launch or
prompt-send failure.

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
- Changing frontend delegation-card presentation.
- Refactoring unrelated spawn, continuation, or persistence code.

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

## Selected Architecture

### 1. Write-first promote transaction

`RunStore::promote_running` will execute the following sequence in one SQLite
transaction:

1. Its first SQL statement performs a claim write against the target run,
   filtered by `task_id` and `status = reserving`. Updating `updated_at` is
   sufficient to acquire the SQLite writer lock. This write is rolled back if
   any later step fails.
2. Require `rows_affected == 1`. A zero-row result is a typed state conflict,
   not an unstructured permanent database error.
3. Read the run while holding the writer lock and validate its bound child
   connection and immutable admission metadata.
4. Charge `UnexpectedContinue` or `Replacement` recovery budget according to
   the durable `admission_class`. `NormalRevision` has no charge.
5. Update the run to `running`, set its accepted and reached-running
   timestamps, and retain the bound child connection.
6. Project the current generation onto the child conversation in the same
   transaction. The projection sets the current generation, running status,
   accepted start, clears prior terminal error/finish fields, and resets the
   generation-scoped runtime rollup fields.
7. Commit.

No other writer can interpose between the claim write, read, budget charge,
run update, and conversation projection. A budget refusal, ownership
invariant failure, projection failure, or commit error rolls back the complete
transaction.

The promote API will return a typed result rather than requiring callers to
parse error strings. At minimum it must distinguish:

- promoted;
- already running for the same run/connection, if an idempotent reread proves
  that state;
- a durable terminal winner;
- budget exhausted;
- state/ownership conflict;
- retryable SQLite busy or locked;
- permanent persistence failure.

For a zero-row claim, the implementation rereads durable truth after the
failed transaction. An already-running matching run is idempotent success. A
terminal run returns the existing terminal winner. A missing row or mismatched
owner is a typed conflict. This prevents a cancellation race from being
rewritten as an admission failure.

### 2. Bounded transient retry

The shared promote operation retries the complete transaction only for
ordinary SQLite `BUSY` or `LOCKED` results. Production policy is three total
attempts: the initial attempt plus two retries, with short bounded delays of
10 ms and 25 ms.

Budget exhaustion, state conflict, not-found, invariant failure, and other
permanent errors are not retried. `BUSY_SNAPSHOT` should be eliminated by the
write-first ordering; if it is ever observed, it is logged distinctly as an
invariant regression and the complete transaction uses the same bounded retry
rail.

The result reports retry count and final failure class so the broker can emit
metrics without duplicating transaction policy in generation-specific code.

### 3. Per-generation accepted timestamps

`AcceptedDelegationPrompt` will carry a fresh `prompt_accepted_at` sampled for
the current prompt after the send path accepts it. It will no longer query the
conversation row for `delegation_started_at` after prompt enqueue.

The promote transaction persists:

- run `started_at = prompt_accepted_at`;
- run `reached_running_at = promote_at`, where `promote_at` is the promotion
  transaction's wall-clock sample clamped to be no earlier than
  `prompt_accepted_at`;
- conversation `delegation_started_at = prompt_accepted_at` under the
  generation projection fence.

The run's pre-promote start remains provisional and is overwritten at
promotion. Live runtime statistics are rebased to the same
`prompt_accepted_at` before publication. This gives generation 2 and later a
start distinct from generation 1 and removes a post-send database lookup that
can itself fail after the external side effect.

No schema migration is required. The relevant timestamp columns are already
nullable and existing historic values remain unchanged.

### 4. Shared post-prompt failure handler

Generation 1 and continuation will call one broker helper for every promote
failure after prompt acceptance. The helper owns error classification,
cancellation, settlement, cleanup, reporting, and observability.

The classification is:

| Promote outcome | Durable terminal code | Recovery behavior |
| --- | --- | --- |
| Existing terminal winner | Preserve existing truth | Replay winner; do not overwrite |
| Budget exhausted | `budget_exhausted` | No automatic replay |
| Retry exhausted | `admission_failed` | Explicit replacement only |
| Permanent persistence/invariant failure | `admission_failed` | Explicit replacement only |
| Unresolved ownership/CAS conflict | `admission_failed` | Explicit replacement only; log conflict class |

`spawn_failed` remains reserved for failures before prompt acceptance, such
as child process/session launch failure or prompt delivery failure that did
not cross the accepted boundary.

For a newly classified failure, the helper performs this sequence:

1. Cancel the accepted child prompt. Cancellation is idempotent; failure is
   logged but does not block settlement.
2. Start durable terminal settlement with the intended terminal code.
3. Disconnect the child after cancellation has been requested.
4. Release live run coordination only after settlement succeeds, an existing
   terminal winner is loaded, or a deduplicated `PendingTerminalRetry` record
   owns the intended write.
5. Return a sanitized report. Raw database errors remain in structured logs.

The affected branches must not use `let _ = settle_terminal(...)` or otherwise
discard settlement results.

### 5. Settlement retry ownership

Admission-failure settlement will reuse the existing persistence retry kernel
and `PendingTerminalRetry` single-flight ownership. Its retry payload must
retain the intended durable terminal (`admission_failed` or
`budget_exhausted`); it must not rewrite that payload to `persistence_error`.

If initial bounded settlement attempts exhaust on a transient error, one
process-local retry record is installed before coordination is released. Its
worker continues retrying the exact terminal payload until it wins or observes
an existing terminal winner. Metrics and terminal audit are emitted only for
the durable CAS winner.

A permanent settlement failure freezes the retry record instead of spinning.
The immediate caller receives a sanitized `persistence_error`, while the
frozen record remains the process-local ownership marker. Startup
reconciliation is the durable backstop if the process exits before the record
can be resolved.

### 6. Known admission-failure recovery

Add stable error code `admission_failed` and stable replacement reason
`admission_failed`.

The replacement reason matches only a source run with all of the following:

- `status = failed`;
- `error_code = admission_failed`;
- `reached_running_at IS NULL`;
- it is the latest run for the child/thread;
- the existing ownership, snapshot, agent, workspace, and work-unit guards
  still pass.

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

Startup reconciliation will split `reserving` runs as follows:

- An unbound run, with no `child_connection_id`, is known not to have reached
  the child send phase. It retains the existing safe pre-admission
  `host_restarted` recovery behavior.
- A bound run is settled `failed/admission_unknown`, with structured audit
  recording prior status `reserving` and restart provenance. Binding precedes
  prompt send, so this category deliberately includes some false positives in
  order to prevent automatic duplicate execution.

`admission_unknown` is not continuable and is never automatically replayed.
Add explicit replacement reason `admission_unknown`, matching only the latest
`failed/admission_unknown` source with `reached_running_at IS NULL` and the
normal replacement ownership/snapshot guards. The caller-facing response must
state that the prior prompt may already have executed.

The replacement uses the same success-only replacement budget rule as
`admission_failed`.

### 8. Metrics and structured logs

`accepted_count` will mean the number of run generations that crossed the
durable accepted boundary, including continuations. It increments only for
the successful `reserving -> running` winner. Add `accepted_by_agent`, keyed by
the existing stable agent labels, and use the `agent_type` parameter currently
ignored by `record_accepted`.

Add these low-cardinality snapshot maps/counters:

- `promote_retries`: `busy`, `locked`, and `busy_snapshot`;
- `promote_failures`: `cas`, `budget`, `busy_exhausted`, and `permanent`;
- `admission_failed_by_agent`;
- `settlement_retry_enqueued`;
- `settlement_retry_exhausted`.

`settlement_retry_enqueued` increments only when a caller acquires a new
single-flight retry record. `settlement_retry_exhausted` increments when the
initial bounded settlement loop fails and durable truth is handed to a new or
already-existing retry owner. A later permanent worker failure is reported by
the existing permanent-persistence path and structured freeze log; transient
workers otherwise continue under their existing policy.

Do not place task ids, connection ids, work-unit keys, paths, or raw errors in
metric labels.

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

## End-to-End Flows

### Successful generation 1 or continuation

1. Reserve the run durably.
2. Spawn or resume the child and bind the child connection.
3. Enqueue the prompt and obtain the current `prompt_accepted_at`.
4. Execute the write-first promote transaction, with bounded ordinary-lock
   retry if required.
5. Rebase live runtime state to `prompt_accepted_at`.
6. Install the run in the broker's running state.
7. Emit accepted metrics, transition audit, and running acknowledgement.

### Promote fails after prompt acceptance

1. Classify the typed promote outcome.
2. Preserve a durable terminal winner if one already exists.
3. Otherwise cancel the accepted prompt.
4. Reliably settle `budget_exhausted` or `admission_failed`.
5. Install retry ownership before releasing live coordination when settlement
   has not yet committed.
6. Disconnect and return a sanitized non-running result.

### Host restarts with a reserving run

1. Startup reconciliation runs before delegation requests are accepted.
2. Unbound reserving runs follow safe pre-admission restart recovery.
3. Bound reserving runs become `admission_unknown`.
4. No prompt is automatically replayed.
5. A caller may explicitly replace the uncertain run after considering the
   duplicate-execution risk.

## API And Compatibility

- Add `admission_failed` and `admission_unknown` to stable broker/audit error
  handling.
- Add the same two values to the `replacement_reason` tool schema and its
  validation constants.
- Reports continue carrying string `error_code`; no transport shape change is
  required.
- Metrics snapshots gain fields but retain existing fields.
- Historic `spawn_failed` and `host_restarted` rows are not rewritten.
- No database migration is required.
- No provider receives special scheduling, retry, or filesystem behavior.

## Implementation Boundaries

Expected production changes are limited to:

- `src-tauri/src/acp/delegation/run_store.rs`: write-first promote, typed
  result, atomic projection, recovery eligibility, and startup split;
- `src-tauri/src/acp/delegation/broker.rs`: shared promote/failure handling,
  reliable settlement ownership, metrics calls, and error reporting;
- `src-tauri/src/acp/delegation/spawner.rs` and
  `src-tauri/src/acp/manager.rs`: per-prompt accepted timestamp and removal of
  the post-send conversation timestamp lookup;
- `src-tauri/src/acp/delegation/metrics.rs`: low-cardinality counters and
  snapshots;
- `src-tauri/src/acp/delegation/tool_schema.json` and matching validation/tests:
  explicit recovery reasons.

No unrelated delegation refactor is part of this work.

## Test Strategy

Tests are deterministic. Concurrency tests use barriers, bounded test gates,
and scripted errors rather than timing-sensitive sleeps.

### RunStore transaction tests

- Use a real SQLite file and two connections to force a concurrent writer
  around promote. Verify write-first promote succeeds without
  `BUSY_SNAPSHOT` and the competing writer observes normal serialization.
- Verify one and two ordinary busy/locked failures retry the complete
  transaction and then succeed.
- Verify retry exhaustion returns the typed failure without a partial run,
  budget, timestamp, or conversation projection write.
- Verify budget exhaustion rolls back the claim and does not increment any
  budget counter.
- Verify a zero-row claim rereads durable truth: matching running is
  idempotent, terminal replays the winner, and mismatched ownership is a typed
  conflict.
- Verify a successful recovery charge and promote commit together exactly
  once.
- Verify generation 2 overwrites the latest conversation projection while a
  delayed generation-1 projection is rejected.

### Broker admission tests

- For both generation 1 and continuation, script the first promote attempt as
  transient and the next as successful. Assert one prompt send, no cancel,
  no disconnect before normal ownership transfer, one running acknowledgement,
  and one accepted metric.
- Exhaust transient promote retries and assert prompt cancellation, durable
  `admission_failed`, no running acknowledgement, and no discarded settlement
  result.
- Inject budget exhaustion and assert durable `budget_exhausted`, with no
  rewrite to `spawn_failed` or `admission_failed`.
- Race parent cancel and child terminal against promote. Assert the first
  durable terminal wins and the failure helper never overwrites it.
- Fail initial settlement transiently, then allow the retry worker to win.
  Assert retry ownership exists before coordination release and the intended
  error code is eventually durable.
- Inject permanent settlement failure and assert a frozen ownership record,
  sanitized immediate `persistence_error`, no worker spin, and startup
  backstop compatibility.

### Recovery tests

- Assert only latest `failed/admission_failed` runs that never reached running
  match `replacement_reason = admission_failed`.
- Assert only latest `failed/admission_unknown` runs that never reached running
  match `replacement_reason = admission_unknown`.
- Assert neither code is automatically continuable or replayed.
- Assert the reasons cannot be forged against completed, running, reached-
  running, stale, mismatched-agent, or incomplete-snapshot sources.
- Assert failed replacement attempts do not consume budget and exactly one
  successful replacement promote does.
- Reconcile an unbound reserving run and assert the safe pre-admission restart
  behavior remains.
- Reconcile a bound reserving run and assert `admission_unknown`, structured
  audit, no automatic continue, and explicit replacement eligibility.

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
- Assert idempotent/CAS-loser paths do not double count accepted or terminal
  metrics.
- Assert retry, final failure, admission failure, and settlement retry metrics
  use only the documented stable labels.

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

- The deterministic concurrent-write regression no longer reproduces
  `BUSY_SNAPSHOT` in promote.
- An ordinary transient writer conflict can recover without canceling or
  resending the prompt.
- A post-prompt promote failure can never return `running` or be mislabeled
  `spawn_failed`.
- The affected code contains no ignored settlement result.
- A failed settlement retains retry ownership until durable truth or a frozen
  permanent-failure marker exists.
- Automatic startup/continuation logic never replays an admission-unknown
  prompt.
- Explicit known/unknown admission replacements are narrowly validated and
  charged only on successful promote.
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

## Residual Risk

The write-first transaction removes the known stale-snapshot upgrade pattern,
but ordinary SQLite writer contention can still exceed the short retry window.
That case becomes a precise, recoverable `admission_failed` instead of a
misleading `spawn_failed`.

Exactly-once execution across a host crash remains impossible without a
provider-side idempotency or accepted-prompt lookup protocol. This design
chooses no automatic duplicate execution: bound reserving runs become
`admission_unknown` and require an explicit recovery decision.
