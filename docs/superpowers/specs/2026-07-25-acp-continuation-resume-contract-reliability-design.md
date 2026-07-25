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

**`reused_session` definition (amends 2026-07-21):** `true` means attach
succeeded under the §1 matrix (response id omitted/blank **or** present and
matching the requested id) **and** the continued prompt was admitted. When an
id is returned and matches, the stronger prior “verified unchanged” reading
still holds; when the id is omitted, the flag asserts request-scoped attach
success without a contradicting id, not an explicit echo from the agent.

### 2. Bootstrap Terminal Persistence

**In-scope bootstrap settlement (single owner).** All bootstrap refusal kinds
listed below **must** settle exclusively through one broker helper
(`settle_bootstrap_unresumable` or a thin rename that returns a typed
settlement result). No in-scope path may call `runs.settle_terminal` /
`task_store.settle` directly, and no generic spawn-error fallback may
re-settle after the helper already claimed or settled:

- missing durable external session id required for `ResumeExistingOnly`
  (including the pre-spawn missing-id path that today settles inline);
- resume/load RPC failure under `ResumeExistingOnly` (including failed
  `spawn_resume_existing` paths that today re-settle as generic spawn error);
- explicit returned-id mismatch (`unresumable`).

The helper returns a typed result the caller must honor: `Won`, `Existing`
(with durable winner code), `PendingCompensation` (transient exhaustion with
claim retained), or `PermanentPersistenceError`. Callers must not invent a
second settle or overwrite the claim after receiving that result. At least
one integration-style test must exercise each in-scope refusal through real
`continue_delegation` admission, not only unit calls of the helper.

**Out of scope (non-goal for this change):** other `continue_delegation`
inline settles that are **not** bootstrap attach failures — for example
incompatible launch configuration (non-resume), incarnation mismatch after
a successful attach, folder-missing, send failure after attach, or promote
failure. Those paths keep today’s single-attempt settle. Extending the same
retry+park helper to them is a follow-up.

`settle_bootstrap_unresumable` currently calls `RunStore::settle_terminal`
directly. It must instead call the broker’s existing **`settle_with_retry`**
helper (which uses the `DelegationTaskStore` trait path and
`PersistenceRetryPolicy`). In production, `DbDelegationTaskStore` and the
broker already share the same `Arc<RunStore>`, so this changes retry behavior
without changing durable CAS authority. Implementers must not treat “shared
`Arc<RunStore>`” as permission to keep calling `runs.settle_terminal`
directly.

The immediate settle uses `PersistenceRetryPolicy::production()`:

- four total attempts;
- exponential delays of 25 ms, 50 ms, and 100 ms;
- retry only `TaskStoreError::Transient` (`SQLITE_BUSY`, `SQLITE_LOCKED`, and
  their mapped forms);
- return immediately on `Won`, `Existing`, or a permanent error.

### 3. Outcome Parking And Background Compensation

#### 3.1 First-terminal claim before any persistence attempt

“First-terminal-wins” for bootstrap vs parent-end means **process-local claim
order under `pending.inner`**, not whichever durable CAS commits first after
retries.

Before the **first** `settle_with_retry` attempt, `settle_bootstrap_unresumable`
must, under the same lock / ordering authority used by parent-end handoff
claim (`take_reserving_handoffs_for_parent_end` and related paths):

1. Resolve the exact run `task_id` from a **retained identity**, in order:
   child connection incarnation when present; else the handoff registration /
   durable run row already bound at continue admission (required for pre-spawn
   missing-id refusals that have no connection yet). If no task id can be
   resolved, return a typed permanent failure without inventing a row.
2. Build both the typed `DelegationOutcome::Err(unresumable)` and its
   `TerminalTaskWrite`.
3. **Claim bootstrap terminal intent** for that handoff/task under
   `pending.inner` (authoritative while durable is non-terminal):
   - Insert-if-absent
     `ReservingHandoffDisposition::ChildTerminal(original_outcome)` into
     `closed_handoff_dispositions` (cross-owner first-wins; never overwrite an
     earlier `ParentEnded` or other owner’s disposition).
   - If the insert is a **no-op** because another claim already exists, the
     helper **must not** attempt its own durable CAS. Resolve the existing
     claim / durable winner non-destructively and return typed `Existing` (or
     the parked winner’s code). This is the symmetric “lost-claim” rule to
     parent-end’s “do not invent parent_canceled over ChildTerminal.”
   - If the insert **wins**, insert the process-local completed/status overlay
     for the original outcome (see §3.3).
   - After releasing the lock as needed, mirror the original
     `TerminalTaskWrite` into `PendingTerminalRetry` via `put_retry` only when
     **this claim won**. `put_retry` is insert-if-absent for the same task id
     (do not overwrite a parent-end payload). Call `put_retry` **before**
     spawning the worker (`has_retry_record` gate). `PendingTerminalRetry` is
     the worker payload / single-flight key; the **authoritative** intent
     while durable is non-terminal remains the in-lock disposition + overlay.

Only after a **winning** claim may the broker attempt durable CAS via
`settle_with_retry`.

**Parent-end must honor an existing bootstrap claim.** Today
`take_reserving_handoffs_for_parent_end` computes disposition from early-
complete/cancel stamps and **unconditionally overwrites**
`closed_handoff_dispositions`. That is incompatible with §3.1. This change
requires:

1. When selecting parent-end disposition, **first peek** an existing
   `ChildTerminal` (or other) claim in `closed_handoff_dispositions`. If a
   child-terminal claim is already present, treat that as the earlier winner:
   do **not** invent `ParentEnded` / `parent_canceled` for that handoff, and
   do **not** CAS a parent-end terminal over it. Parent-end may still drive
   durable finalization of the **already-claimed child terminal** (see below).
2. Cross-owner park writes from parent-end and bootstrap are
   **insert-if-absent** (first-wins), never unconditional overwrite.
3. `settle_reserving_handoffs_for_parent_end` must settle the disposition that
   survived first-wins, not a freshly invented parent-end when a child claim
   already exists. When parent-end persists an already-claimed `ChildTerminal`
   and wins durable CAS, parent-end is the **finalization owner** for that
   path: clear overlay/park/retry intent, notify waiters, and perform
   exactly-once metric/audit for the durable winner. A later worker seeing
   `Existing` must not account again.

Same-owner transitions (e.g. bootstrap upgrading its own claim from
`unresumable` business intent to `persistence_error` after a permanent store
failure) are allowed via compare-and-replace keyed on the claim owner; they
are not cross-owner overwrites.

#### 3.2 Settlement and cleanup sequence

After the first-terminal claim:

1. Attempt terminal CAS through `settle_with_retry` (shared task store +
   production retry policy) **only if** §3.1 claim insert won.
2. On **`Won`**: clear parked disposition and retry record for this task,
   unregister/unreserve the handoff, keep the process-local overlay (it now
   matches durable). Do not re-emit duplicate sidebar/meta/attention/
   completion events beyond the refuse-time baseline. Perform exactly-once
   metric/audit for this durable winner.
3. On **`Existing`**: durable first-terminal already won (parent-end or prior
   child terminal). Under one critical section (or equivalent atomicity),
   **replace or invalidate** the process-local overlay **and** the disposition
   claim with the durable winner’s report/code, then clear retry/park state,
   so no peek can observe a mixed stale bootstrap claim over a durable
   `parent_canceled` (or other) winner. Unregister as appropriate. No
   duplicate completion events; no second metric count if parent-end already
   accounted on its `Won`.
4. On **transient exhaustion** (bounded retry returned only transient errors):
   the claim from §3.1 already parked `ChildTerminal` and the overlay;
   ensure `PendingTerminalRetry` holds the **original** terminal payload,
   start/ensure the per-task single-flight retry worker, then unregister the
   live handoff registration **without** dropping park/overlay/retry.
5. On **permanent** persistence error (non-transient): same-owner
   compare-and-replace claim/overlay to `persistence_error` (ordered like
   step 3: replace intent before clearing any transient retry ownership);
   freeze the retry record; see §3.4.

Park-before-unregister applies to every production exit of
`settle_bootstrap_unresumable` that leaves the durable row non-terminal while
the process stays alive. The no-`run_store` / test-only early-complete buffer
path keeps its existing buffer-then-return behavior (no live handoff
unregister there).

#### 3.3 Process-local terminal intent: non-destructive until durable

While durable status is still non-terminal, the process holds a **parent-scoped
terminal-intent** composed of:

1. `closed_handoff_dispositions` claim (insert-if-absent first-wins);
2. completed-task / status overlay;
3. optional `PendingTerminalRetry` worker payload.

**All** of the following must resolve that intent **non-destructively** via one
shared terminal-intent resolver (single implementation; no parallel ad-hoc
peek paths):

- single-task and batch status (`get_task_status` / `status_from_db` /
  `assemble_reports`);
- closed-handoff continue reports (`continue_closed_handoff_report`) — **do not
  `take`/remove the park** while durable is still `reserving`;
- exact continuation / fingerprint replay while durable is `reserving` — must
  **not** return a “still admitting / running” ack when terminal intent is
  already claimed; report the claimed terminal instead;
- parent-end disposition selection (§3.1).

Clear park, overlay, and retry record **only after** a durable terminal is
observed (`Won` or `Existing`). Re-park-after-project is **not** required if
peek is used everywhere; destructive `take` is removed from the continue
report path for this state.

Overlay `CompletedTask` fields such as `agent_type` are sourced from the
durable run row / coordination identity already available on the handoff path.
After overlay insert, mirror `settle_task` waiter side effects: bump
`status_version` and `result_notify.notify_waiters()` (and clear observation /
notify supervisor as that path already does).

Tests must cover: repeated closed-handoff reads during retry; same-tool
fingerprint replay during retry; status/batch reads during retry — none may
degrade to `parent_canceled` or “still running” after a bootstrap claim.

#### 3.4 Permanent persistence failure cleanup

| State | Transient exhaustion | Permanent store error |
| --- | --- | --- |
| Process-local overlay | Original `unresumable` (first-claimed business outcome) | Replace overlay with stable `persistence_error` report (sanitized message; no raw SQL) |
| Park / disposition | Keep `ChildTerminal(unresumable)` until durable win | Same-owner compare-and-replace claim to permanent-error terminal intent (`persistence_error`); no dual “may be either” |
| Retry record | Keep original terminal in `PendingTerminalRetry`; worker runs | Mark the record **frozen** (explicit flag or equivalent ownership tombstone on the existing retry shape). `spawn_persistence_retry_worker` / `put_retry` must refuse to re-own a frozen record. Only an explicit new claim generation or process restart reconciliation may clear it. |
| Handoff registration | Unregister live registration; intent remains | Unregister live registration; intent remains until durable or restart |
| Durable row | May remain `reserving` until worker wins | May remain `reserving` until host restart reconciliation (`host_restarted`) if CAS never commits |
| Immediate parent-facing / refuse event | Business `unresumable` (refuse path already emitted) | If a connection refuse / `SessionLoadFailed` already emitted business `unresumable`, leave that event unchanged; subsequent status/report reads surface `persistence_error`. If no refuse event was emitted yet (e.g. pure pre-spawn missing-id path), the immediate `continue_delegation` response reports **`unresumable`** as the business outcome and status may additionally expose `persistence_error` only after the permanent store failure is classified — prefer a single parent-facing business code of `unresumable` for the continue response, with `persistence_error` reserved for status when durable CAS failed permanently after a successful business refuse classification. Helper typed result is `PermanentPersistenceError` so callers do not double-settle. |
| Metrics / audit | Count intended terminal once on eventual durable win | Count a logical `persistence_error` once (even without durable CAS winner); log raw DB text at `ERROR` only |

Raw database / `TaskStoreError` display text remains in logs only. Parent-facing
messages use stable phrases such as
`failed to persist terminal state (persistence_error)` — never
`format!("...{err}")` with SQLite detail.

#### 3.5 Background worker accounting

The current worker is specialized to `failed/persistence_error` at the metrics/
audit site. Bootstrap compensation must persist the **original** terminal
(`failed/unresumable`), not rewrite the business outcome because an
intermediate attempt was busy.

**Blast radius:** the only production entry is `spawn_persistence_retry_worker`.
All callers (existing persistence_error exhaustion paths and the new bootstrap
compensation path) must supply a correct `TerminalTaskWrite` in
`PendingTerminalRetry`. The worker becomes payload-driven: it never hardcodes
`persistence_error` when the payload says otherwise. No other code path may
spawn a second worker for the same task id.

**Source of status/code:** **copy** `status` and `error_code` from
`PendingTerminalRetry.terminal` (`TerminalTaskWrite`) **before** moving the
payload into `store.settle`, then use those copies for `Won` metrics/audit and
overlay maintenance. `PersistenceRetryAccounting` need **not** gain duplicate
status/code fields for this change.

On worker `Won` or observation of `Existing`, apply §3.2 overlay/disposition
replacement rules and clear park/retry without re-emitting duplicate
completion events. On `Existing`, skip metric/audit if a prior finalization
owner already counted. Tests must lock event/metric counts at the refuse-time
baseline + exactly one durable terminal accounting increment.

### 4. Race And Cleanup Invariants

The following invariants are binding:

1. Durable `settle_terminal` remains first-terminal-wins for durable CAS.
2. Process-local first-terminal claim (§3.1) decides bootstrap vs parent-end
   intent **before** retry delays; insert-if-absent on dispositions.
3. An already durable parent-end or child terminal is returned as `Existing`;
   bootstrap refusal cannot overwrite it; overlay is replaced with the
   durable winner on `Existing`.
4. A handoff is not removed until its outcome is either durable or represented
   by both a process-local terminal view and a park and/or retry record
   (permanent error: overlay + no spin; see §3.4).
5. Closed-handoff report precedence remains durable terminal, non-destructive
   parked disposition / terminal intent, durable re-read, then last-resort
   `parent_canceled`.
6. A bootstrap refusal that claimed terminal intent always supplies that
   intent (or a durable terminal) for status, replay, closed-handoff, and
   parent-end resolvers, so the last-resort `parent_canceled` and
   “still admitting” branches are not reached for this case while the process
   is alive.
7. Retry workers are single-flight per task id.
8. Metrics and terminal audit count the durable CAS winner exactly once.
9. A retry success does not emit duplicate sidebar, meta, attention, or
   completion events.
10. Disconnect cleanup cannot relabel a parked or durable `unresumable`
    outcome as canceled.

### 5. Cold Failure Reporting

Move cold report message selection into one shared helper used by both
`PersistedTask::to_report` and the legacy `db_report` fallback. The helper
accepts status, optional `error_code`, and child conversation id.

Message rules are:

| Status | Message rule |
| --- | --- |
| Running | Existing running guidance |
| Completed | Result no longer cached; open child session for full output |
| Failed | State failure, stable `error_code` (or `unknown` if missing), and child-session detail hint |
| Canceled | State cancellation, stable `error_code` (legacy `db_report` may still synthesize `"canceled"` when the row has no code — preserve that fallback in the shared helper), and child-session hint |
| Unknown | Existing unknown-task guidance |

Example:

```text
Delegation failed (unresumable): the existing agent session could not be
resumed safely. Open child session 1693 for details.
```

Known stable codes may receive concise specific text. Unknown codes use a
generic status phrase but still include the exact stable code. For failed rows
with a missing code, the **message** may embed `unknown` while the structured
`error_code` field remains `None` (no invented field). For canceled rows with a
missing code, the legacy `db_report` path continues to synthesize field
`"canceled"` for compatibility — intentional asymmetry between failed-missing
and canceled-missing. Messages never include raw SQL, credentials, command
lines, or agent response bodies.

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

- Restore missing-response-id fallback for `ResumeExistingOnly` in
  `gate_session_started_for_attach` (omitted/blank → emit requested id).
- Keep explicit mismatch refusal via `verify_external_session_id` when an
  actual id is present.
- Treat blank extension ids as absent (existing extract filter); matrix row
  “omitted, blank, or absent” is one case.
- After the gate change, remove or stop calling the unreachable
  `MissingActual → RefuseUnresumable` path in `decide_session_started` so
  clippy `-D warnings` stays clean; update/replace
  `gate_resume_existing_refuses_when_agent_omits_id` and any
  `decide_session_started_refuses_on_missing_actual` tests to the approved
  matrix.
- Do not probe `_meta.sessionId` in this change (opportunistic top-level
  extraction only; nested meta is a non-goal).

### `src-tauri/src/acp/connection.rs`

- Keep raw extension-id extraction for both resume and load.
- Exercise standard no-id responses through connection-level contract tests.
- Preserve the existing no-`session/new` rule and prompt admission order.

### `src-tauri/src/acp/delegation/broker.rs`

- Route **every** in-scope bootstrap refusal through one helper that returns
  a typed settlement result; remove direct settles from pre-spawn missing-id
  and `spawn_resume_existing` failure paths; callers must not re-settle.
- Route helper durable writes through **`settle_with_retry`**.
- **Claim first-terminal intent** under `pending.inner` (park insert-if-absent
  + overlay; then `put_retry` before worker spawn) **before** the first
  persistence attempt.
- **Parent-end:** peek existing claims; insert-if-absent only; never invent
  `parent_canceled` over an earlier `ChildTerminal` claim.
- Shared **non-destructive** terminal-intent resolver for status, batch,
  closed-handoff reports, fingerprint replay, and parent-end. Remove
  destructive `take` while durable is non-terminal.
- On `Existing`, replace overlay with durable winner before clearing intent.
- On transient exhaustion, keep intent + worker; unregister live handoff only.
- On permanent error: freeze retry record; status shows sanitized
  `persistence_error`; already-emitted refuse event stays business
  `unresumable`.
- Worker: copy status/code from `retry.terminal` **before** move into settle.
- Clear intent only after durable `Won`/`Existing`; no duplicate events.

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
- Each in-scope refusal (missing external id, resume/load RPC failure,
  explicit id mismatch) settles only via the helper through real
  `continue_delegation` (or the broker continue path under test), with no
  second direct settle.
- Bootstrap claim happens before the first settle attempt: parent-end during
  the retry window cannot produce `parent_canceled` when bootstrap claimed
  first (gated bootstrap-first race test; parent-end peeks claim).
- Parent-end-first race: parent disposition wins; bootstrap claim insert is a
  no-op and bootstrap does **not** attempt its own CAS; overlay/disposition
  resolve to the parent-end (or durable) winner. Also inject delayed parent-end
  durable commit and assert bootstrap still does not invert claim order.
- Bounded retry exhaustion keeps non-destructive terminal intent + retry
  payload; status, closed-handoff, and fingerprint replay all report
  `unresumable`, never `parent_canceled` or “still admitting”.
- Repeated closed-handoff reads and same-tool replays during retry do not
  consume the park or degrade.
- The retry worker is single-flight and eventually persists the original
  `unresumable` code.
- After worker success, completion/sidebar/meta event counts stay at the
  refuse-time baseline and terminal metrics increment exactly once.
- Status reads (single-task and batch) while the durable row is still
  `reserving` return the process-local terminal overlay / park peek.
- Repeated status reads after divergent `Existing` show the durable winner,
  not a stale bootstrap overlay.
- Durable win/existing clears retry and parked state without leaking a live
  registration.
- Permanent persistence failure: status shows sanitized `persistence_error`,
  no worker spin, no raw SQLite text; refuse event may remain business
  `unresumable` if already emitted.
- Parked-but-unpersisted bootstrap outcome does not survive process restart as
  `unresumable`; startup reconcile classifies still-`reserving` as
  `failed/host_restarted` (no retroactive labeling).

Prefer injected `TaskStoreError::Transient` failures over timing-dependent
real SQLite locks in deterministic unit tests. Retain at least one SQLite-backed
integration test for the shared `DbDelegationTaskStore`/`RunStore` path.

### Cold report tests

- Completed cold read retains the cache-miss guidance.
- Failed `unresumable` cold read contains `unresumable` and a child-session
  hint, not `Result no longer cached`.
- Canceled cold read names its cancellation code; missing code on the legacy
  path still synthesizes `"canceled"` via the shared helper.
- Failed row with missing code uses a stable placeholder (e.g. `unknown`) in
  the message while keeping `error_code` field semantics unchanged.
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
