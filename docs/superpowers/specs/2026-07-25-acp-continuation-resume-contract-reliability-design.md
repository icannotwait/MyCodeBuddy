# ACP Continuation Resume Contract And Reliability Design

Date: 2026-07-25

Status: Approved

## Summary

Fix `continue_delegation` so a successful ACP `session/resume` or
`session/load` response may omit `sessionId`, as permitted by the ACP schema.
Codeg will use the session id from the request when the response omits an id,
accept an explicitly returned matching id, and reject only an explicitly
returned different id.

The same change makes pre-prompt bootstrap terminal settlement resilient to
transient SQLite contention and makes cold task reports preserve the real
terminal status and `error_code`. A transient persistence failure must not
leave a run in `reserving`, change `unresumable` to `parent_canceled`, or show a
failed task as a generic cache miss.

## Incident Evidence

Conversation `1675` contained 18 generation-2 continuation runs:

| Agent | Runs | Durable result |
| --- | ---: | --- |
| Codex | 6 | 6 `failed/unresumable` |
| Grok | 6 | 6 `failed/unresumable` |
| CodeBuddy | 6 | 5 `failed/unresumable`, 1 `reserving` |

All 18 runs reached the same local identity refusal:

```text
external session id missing after resume/load
```

The ACP `LoadSessionResponse` and `ResumeSessionResponse` schemas define
`modes`, `configOptions`, and `_meta`; neither response requires or defines a
`sessionId`. The bundled Codex ACP implementation likewise returns model,
mode, and configuration data without an id. In one production trace, Grok
logged `session loaded` at the same timestamp at which Codeg rejected the
successful response because raw id extraction returned `None`.

The failure is therefore after external session recovery and before continued
prompt admission. It is not a task-lineage, correlation, process-spawn, or
worktree failure. This matches the population split: all 35 generation-1 and
replacement launches completed, while no generation-2 continuation completed.

One run, `314ae290-4299-4c6a-9e7a-a22eb0d6f947`, then hit
`SQLITE_BUSY` while Codeg tried to persist the synthetic `unresumable` result.
That bootstrap path logged the write failure, unregistered the handoff, and
left the durable row in `reserving`. A later closed-handoff read had neither a
terminal row nor a parked disposition and therefore reported
`parent_canceled`.

Finally, cold task reads use one generic message for completed, failed, and
canceled rows:

```text
Result no longer cached; open child session N for the full output.
```

The structured `error_code` survives, but the human-readable response hides
the actual failure and encourages incorrect replacement guesses.

## Goals

- Conform continuation session attachment to the standard ACP response
  contract.
- Preserve strict identity protection when an agent explicitly returns an id.
- Never fall through to `session/new` under `ResumeExistingOnly`.
- Send the continued prompt after a successful request-scoped attach with an
  omitted or matching response id.
- Reuse the existing terminal persistence retry policy for bootstrap failures.
- Preserve the original terminal outcome while transient persistence is
  pending.
- Prevent bootstrap settlement races from degrading to `parent_canceled` or a
  permanent `reserving` row.
- Show failed and canceled cold task reports as failures with their stable
  `error_code`; reserve the cache-miss message for completed output.
- Cover the behavior at helper, connection-contract, broker-race, and cold
  reporting levels.

## Non-goals

- Do not weaken comparison when an agent explicitly returns a session id.
- Do not make third-party agents echo a non-standard response field.
- Do not add `session/list` verification after every resume or load.
- Do not prove that a buggy agent which returns success actually restored all
  internal context. ACP exposes request success or failure as that boundary.
- Do not change delegation correlation, lineage, generation, replacement,
  ownership, or budget rules.
- Do not change the default user-session `resume -> load -> new` fallback.
- Do not add a database column or migration for terminal error messages.
- Do not retroactively label historical non-terminal runs `unresumable` when
  their original in-memory outcome is no longer available.

## Document Precedence

This design amends the response-identity portions of
`docs/superpowers/specs/2026-07-21-delegation-session-reuse-design.md`.
Successful request-scoped resume/load is sufficient when the response omits an
id. Existing rules for continuation eligibility, durable reservation, exact
run identity, first-terminal-wins settlement, and session-new refusal remain
in force.

The correlation and provisional-cleanup rules in
`docs/superpowers/specs/2026-07-24-delegation-exact-correlation-and-provisional-cleanup-design.md`
are unchanged.

## Selected Design

### 1. ACP Session Identity Semantics

`ResumeExistingOnly` already requires a non-empty durable external session id
before making an ACP request. That request id is authoritative for a successful
request-scoped `session/resume` or `session/load` operation.

The post-response decision matrix is:

| ACP result | Returned extension id | Decision |
| --- | --- | --- |
| Success | omitted, blank, or absent | Attach with requested id |
| Success | equal to requested id | Attach with requested id |
| Success | different from requested id | Refuse as `unresumable` |
| Error | any | Follow existing resume/load error chain |

The request id remains the emitted `SessionStarted.session_id`; an extension
field never rewrites durable conversation identity.

`verify_external_session_id` remains the strict comparison helper for a
present actual id. `gate_session_started_for_attach` owns the attach-mode
semantics:

```text
Default
  -> emit requested id (unchanged)

ResumeExistingOnly
  -> response id absent: emit requested id
  -> response id present and equal: emit requested id
  -> response id present and different: refuse unresumable
```

Raw `sessionId` / `session_id` extraction remains opportunistic. It provides
additional mismatch protection for agent extensions; it is not a success
requirement.

The existing request chain remains:

```text
supports session/resume
  -> resume success: attach
  -> resume error: try session/load

session/load
  -> load success: attach
  -> load error under ResumeExistingOnly: unresumable
  -> never session/new
```

After attach succeeds, the existing prompt admission path proceeds. A running
continuation may report `reused_session: true` only after attach succeeds and
the continued prompt is admitted; a durable `reserving` row alone does not
claim session reuse.

### 2. Bootstrap Terminal Persistence

Genuine bootstrap refusal still exists for a missing request id, resume/load
RPC failure, incompatible launch configuration, or explicit returned-id
mismatch. Those paths must settle the reserved run reliably.

`settle_bootstrap_unresumable` currently calls
`RunStore::settle_terminal` directly. It must instead use the broker's
`DelegationTaskStore` terminal interface and existing persistence policy. In
production, `DbDelegationTaskStore` and the broker already share the same
`Arc<RunStore>`, so this changes retry behavior without changing durable CAS
authority.

The immediate settle uses `PersistenceRetryPolicy::production()`:

- four total attempts;
- exponential delays of 25 ms, 50 ms, and 100 ms;
- retry only `TaskStoreError::Transient` (`SQLITE_BUSY`, `SQLITE_LOCKED`, and
  their mapped forms);
- return immediately on `Won`, `Existing`, or a permanent error.

### 3. Outcome Parking And Background Compensation

Immediate retry exhaustion must not discard the intended terminal outcome.
The broker uses the existing handoff disposition and retry facilities in this
order:

1. Resolve the exact run `task_id` from the child connection incarnation.
2. Build both the typed `DelegationOutcome::Err(unresumable)` and its
   `TerminalTaskWrite` before cleanup.
3. Attempt terminal CAS through the shared task store with bounded retry.
4. On `Won` or `Existing`, clear any parked disposition/retry record, then
   unregister and unreserve the handoff.
5. On transient exhaustion, park
   `ReservingHandoffDisposition::ChildTerminal(original_outcome)` before
   unregistering the handoff, put the original terminal payload into
   `PendingTerminalRetry`, and start the existing per-task single-flight retry
   worker.
6. Insert the same original outcome into the process-local completed/status
   view while persistence is pending. Immediate and later status reads in the
   same process therefore report `unresumable`, not `reserving` or
   `parent_canceled`.
7. The background worker retries the original terminal payload until it wins,
   observes an existing terminal, or encounters a permanent error. Success
   removes the retry record and parked fallback without re-emitting duplicate
   completion events.

The current background helper is specialized to
`failed/persistence_error`. Generalize its retry accounting payload to carry
the intended terminal status and stable error code. Bootstrap compensation
must persist `failed/unresumable`; it must not rewrite the business outcome to
`failed/persistence_error` merely because an intermediate database attempt was
busy.

A permanent persistence error is different from transient contention. It is
reported and cached as `persistence_error`, logged at `ERROR`, and does not
spin a background retry loop. Raw database text remains in logs, not in the
parent-facing stable message.

### 4. Race And Cleanup Invariants

The following invariants are binding:

1. Durable `settle_terminal` remains first-terminal-wins.
2. An already durable parent-end or child terminal is returned as `Existing`;
   bootstrap refusal cannot overwrite it.
3. A handoff is not removed until its outcome is either durable or represented
   by both a process-local terminal view and a retry/park record.
4. Closed-handoff report precedence remains durable terminal, parked
   disposition, durable re-read, then last-resort `parent_canceled`.
5. A transient bootstrap write failure always supplies the parked child
   terminal, so the last-resort branch is not reached for this case.
6. Retry workers are single-flight per task id.
7. Metrics and terminal audit count the durable CAS winner exactly once.
8. A retry success does not emit duplicate sidebar, meta, attention, or
   completion events.
9. Disconnect cleanup cannot relabel a parked or durable `unresumable` outcome
   as canceled.

### 5. Cold Failure Reporting

Move cold report message selection into one shared helper used by both
`PersistedTask::to_report` and the legacy `db_report` fallback. The helper
accepts status, `error_code`, and child conversation id.

Message rules are:

| Status | Message rule |
| --- | --- |
| Running | Existing running guidance |
| Completed | Result no longer cached; open child session for full output |
| Failed | State failure, stable `error_code`, and child-session detail hint |
| Canceled | State cancellation, stable `error_code`, and child-session hint |
| Unknown | Existing unknown-task guidance |

Example:

```text
Delegation failed (unresumable): the existing agent session could not be
resumed safely. Open child session 1693 for details.
```

Known stable codes may receive concise specific text. Unknown codes use a
generic status phrase but still include the exact stable code. Messages never
include raw SQL, credentials, command lines, or agent response bodies.

`DelegationTaskReport.error_code` and `structuredContent` remain unchanged.
The MCP companion already renders a failed report's `message` and sets
`isError: true`; the frontend already reads `error_code` for status badges.
No wire-shape, frontend type, i18n, or database migration is required.

## Historical Non-terminal Rows

Startup reconciliation already settles authoritative `reserving` and
`running` run rows as `failed/host_restarted`. Therefore the known historical
row `314ae290-4299-4c6a-9e7a-a22eb0d6f947` will not remain non-terminal after
a successful restart reconciliation.

It must not be automatically rewritten to `unresumable`: after process-local
state is lost, startup can prove only that a non-terminal run survived a host
boundary. `host_restarted` is the auditable classification. No production-id
migration or direct database repair is part of this change.

## Component Changes

### `src-tauri/src/acp/session_attach.rs`

- Restore missing-response-id fallback for `ResumeExistingOnly`.
- Keep explicit mismatch refusal.
- Replace the test that expects omission to fail with the approved matrix.

### `src-tauri/src/acp/connection.rs`

- Keep raw extension-id extraction for both resume and load.
- Exercise standard no-id responses through connection-level contract tests.
- Preserve the existing no-`session/new` rule and prompt admission order.

### `src-tauri/src/acp/delegation/broker.rs`

- Route bootstrap terminal writes through bounded transient retry.
- Park the original child terminal before cleanup on retry exhaustion.
- Publish a process-local terminal view while durable compensation is pending.
- Generalize the single-flight retry worker accounting to retain the intended
  terminal code.
- Clear retry/park state after durable win or existing terminal observation.

### `src-tauri/src/acp/delegation/store.rs`

- Reuse the existing transient SQLite classification and retry record shape.
- Use shared status-aware cold message selection.

### `src-tauri/src/acp/delegation/types.rs`

- Host the shared stable cold-report message helper if that avoids duplication
  between store-backed and legacy DB fallback reports.
- Do not change the serialized `DelegationTaskReport` shape.

## Testing

### Session attach unit matrix

- `Default + missing` emits requested id.
- `ResumeExistingOnly + missing` emits requested id.
- `ResumeExistingOnly + blank` emits requested id.
- `ResumeExistingOnly + matching` emits requested id.
- `ResumeExistingOnly + mismatch` refuses with `unresumable`.
- Raw camel-case and snake-case extension ids remain extractable.

### Connection contract tests

Use standard ACP response bodies that contain no session id:

- resume success emits `SessionStarted` with the requested id and admits the
  prompt;
- load success emits `SessionStarted` with the requested id and admits the
  prompt;
- resume error followed by load success admits exactly one prompt;
- explicit returned-id mismatch emits load failure and admits no prompt;
- resume and load errors under `ResumeExistingOnly` admit no prompt and never
  call `session/new`.

At least one contract test must use the same response shape produced by the
bundled Codex ACP implementation rather than a synthetic `sessionId` field.

### Broker persistence tests

- One to three forced transient settle failures eventually produce durable
  `failed/unresumable` during bounded retry.
- Bounded retry exhaustion parks `ChildTerminal(unresumable)`, returns
  `unresumable`, and never returns `parent_canceled`.
- The retry worker is single-flight and eventually persists the original
  `unresumable` code.
- Status reads while the durable row is still `reserving` return the
  process-local terminal overlay.
- A durable parent-end winner remains the terminal result.
- Durable win/existing clears retry and parked state without leaking a live
  registration.
- Permanent persistence failure reports `persistence_error` without exposing
  raw SQLite text.

Prefer injected `TaskStoreError::Transient` failures over timing-dependent
real SQLite locks in deterministic unit tests. Retain at least one SQLite-backed
integration test for the shared `DbDelegationTaskStore`/`RunStore` path.

### Cold report tests

- Completed cold read retains the cache-miss guidance.
- Failed `unresumable` cold read contains `unresumable` and a child-session
  hint, not `Result no longer cached`.
- Canceled cold read names its cancellation code.
- Unknown failure code is preserved verbatim in the stable message.
- Companion rendering uses the failure message, sets `isError: true`, and
  retains structured `error_code`.

### Regression verification

Run the Rust checks required for every shared backend target:

```bash
cd src-tauri

cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings

cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Run focused frontend delegation-card tests if backend fixture text is updated;
no frontend source change is expected.

## Acceptance Criteria

- A standard successful ACP resume/load response with no id reaches continued
  prompt admission.
- An explicitly different response id remains a hard `unresumable` refusal.
- `ResumeExistingOnly` never invokes `session/new`.
- A transient bootstrap terminal write cannot leave a permanent `reserving`
  row while the process remains alive.
- The parent sees the original bootstrap terminal code during persistence
  retry and never receives a fabricated `parent_canceled` for that path.
- Cold failed/canceled reports state the real stable code; completed cache
  misses remain distinguishable.
- Existing correlation, lineage, replacement, cancellation, first-terminal-
  wins, and session-new refusal tests continue to pass.
- A rebuilt and restarted application passes a live smoke continuation for
  Codex, Grok, and CodeBuddy profiles that advertise resume or load support.

## Rollout Notes

The running executable predating this design does not contain the fix. A new
desktop/server build and process restart are required. Startup reconciliation
must complete before accepting delegation requests; it will terminalize any
old non-terminal run rows as `host_restarted`.

After rollout, monitor for:

- `external session id mismatch` (valid hard refusal);
- `external session id missing after resume/load` (must disappear as a refusal);
- `settle_bootstrap_unresumable: settle_terminal failed` (must be replaced by
  bounded retry/compensation telemetry);
- new generation-2 rows left in `reserving` after the owning process remains
  healthy;
- cold failed reports that still say only `Result no longer cached`.
