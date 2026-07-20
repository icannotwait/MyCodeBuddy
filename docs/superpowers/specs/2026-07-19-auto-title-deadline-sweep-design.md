# Automatic Title Deadline Sweep

Date: 2026-07-19

Status: Design approved; Codex-reviewed revisions incorporated; awaiting final review

## Summary

Automatic conversation titles currently wait for the first usable successful
`end_turn` before a job becomes `ready`. When the first reply takes a long time
(long tool loops, cold starts, deep exploration), sidebar and delegation-card
titles stay empty or weak for far longer than users expect.

This change keeps the existing end-turn path and adds a **coordinator deadline
sweep**: jobs that remain in `awaiting_turn` for at least **300 seconds** after
the first accepted prompt are promoted to `ready` using the **first user task
plus any currently available partial assistant text** (which may be empty). The
first successful generated title is **final**; a later full response does not
refine it.

The implementation uses **periodic scanning** (not per-job timers). Sweep
interval is **101 seconds**. Trigger timing is intentionally coarse: a job may
become ready roughly **300–401 seconds** after first-prompt capture.

## Goals

- Promote stuck first-turn auto-title jobs after a 300s deadline without waiting
  for a usable final reply.
- At promote time, include streaming partial assistant text when present; if
  absent, still generate from the first user task alone.
- Preserve the existing first usable `end_turn` path when it wins the race.
- Keep title generation backend-owned, hidden, non-recursive, and limited to
  the first task plus one assistant context snapshot.
- Reuse the existing claim → hidden runner → finalize pipeline after promote.
- Survive process restart without in-memory per-job timers.
- Make promote / capture / claim / completion transitions **predicate-safe** under
  concurrent connections and concurrent end-turn vs sweep.

## Non-goals

- Secondary refinement after a deadline-generated title (no second AI title from
  the full final reply).
- Configurable deadline or sweep interval in settings UI.
- Per-project or per-agent title-agent overrides (unchanged from existing
  auto-title design).
- Retroactive generation for conversations that never received a job.
- Per-job one-shot 300s timers (rejected in favor of coordinator sweep).
- Changing native CLI title generation or manual rename precedence.
- Deadline re-promotion of `retry_wait` jobs.
- Backfilling `first_prompt_at` from `updated_at` for rows that already captured
  a prompt before this migration.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Architecture | Coordinator periodic sweep (Approach 2) |
| Deadline | 300 seconds from first_prompt_at |
| Sweep interval | 101 seconds |
| Timing precision | Coarse; ~300–401s window is acceptable |
| Deadline context | First user task + current partial assistant stream |
| Empty partial at deadline | Still promote; generate from user task only |
| Post-success refine | No; successful generate is final (`auto_title_finalized`) |
| End-turn before deadline | Still promotes immediately with full final text |
| Settings | Hard-coded constants; no new UI |
| Pre-migration rows with prompt already captured | `first_prompt_at` stays NULL → deadline skip; end-turn only |
| Pre-migration rows with no prompt yet | First post-upgrade capture sets `first_user_text` + `first_prompt_at` normally |
| Startup | Immediate sweep under the existing `started` gate, then every 101s |

## Relationship to Existing Auto-Title Design

This spec **extends** `2026-07-16-auto-title-reference-search-design.md`. Unless
explicitly overridden here, enrollment, settings, hidden runner isolation,
internal session exclusion, locale rules, bound_context, concurrency permits,
manual rename / Off cancellation, and dual-runtime cores remain unchanged.

### Overrides

| Topic | Previous | This change |
| --- | --- | --- |
| Ready trigger | Only first usable `end_turn` | Usable `end_turn` **or** deadline sweep |
| Claimability of empty assistant | Ready rows with empty assistant deleted | `Some("")` allowed; only empty **user** is fatal; `None` on Ready is invalid |
| Assistant context source | Only completion final text | Final text **or** live partial snapshot at promote |
| Completion / claim safety | Read-modify-write by PK (de facto) | Explicit conditional transitions required (see Atomicity) |

## Constants

```rust
/// Minimum age of first-prompt capture before deadline promotion.
const AUTO_TITLE_DEADLINE: Duration = Duration::from_secs(300);

/// How often the coordinator scans for deadline-eligible jobs.
const AUTO_TITLE_DEADLINE_SWEEP_INTERVAL: Duration = Duration::from_secs(101);

/// Soft cap on jobs considered per sweep pass (ordered, deterministic).
const AUTO_TITLE_DEADLINE_SWEEP_BATCH: usize = 64;
```

Tests may inject shorter durations and smaller batches via constructor
parameters or `#[cfg(test)]` hooks so unit tests do not sleep for minutes.

## Schema

Add a **new forward migration** (do not edit `m20260716_000001_auto_title`).
Register it after the current latest migration. Include a `down` path and
upgrade tests.

Add to `auto_title_jobs`:

| Column | Type | Purpose |
| --- | --- | --- |
| `first_prompt_at` | nullable timestamp with time zone | Instant when `first_user_text` was first written; deadline origin |

Rules:

- Written **write-once** only via a **conditional** capture update (see Capture).
- Do **not** use `updated_at` as the deadline origin.
- **Add** index `(state, first_prompt_at, conversation_id)` for deadline
  candidate selection.
- **Retain** the existing claim drain index
  `(state, updated_at, conversation_id)`; do not replace it.
- Migrated rows: `first_prompt_at = NULL`.
  - If the row **already** has `first_user_text` from before the upgrade: leave
    NULL permanently → **end-turn only** (no `updated_at` backfill).
  - If the row has **not** captured a prompt yet (`first_user_text` still NULL):
    the first post-upgrade capture sets both fields and becomes deadline-eligible.

No separate “legacy eligibility” flag is required: write-once capture on still-
uncaptured jobs is enough; already-captured pre-migration jobs stay NULL.

## Lifecycle

### Dual ready paths

```text
awaiting_turn --usable end_turn--> ready   (first_assistant = Some(full final text))
awaiting_turn --deadline sweep--> ready   (first_assistant = Some(partial or ""))
ready --worker claim--> running(attempts += 1)
running --valid title--> generated (job deleted, auto_title_finalized)
running(attempts = 1) --failure--> retry_wait | ready(attempt 2)  [unchanged]
retry_wait --next usable end_turn--> ready(attempt 2)
```

`retry_wait` is **never** deadline-promoted. Deadline only advances jobs still
waiting in `awaiting_turn` for their first ready transition.

### Who wins

Exactly one path may first-move a job into `ready` for attempt context.

| Race | Result |
| --- | --- |
| Usable `end_turn` first | Job ready with full final text; sweep CAS no-ops |
| Deadline sweep first | Job ready with `Some(partial)` (possibly `Some("")`); later usable completion **must not** overwrite `first_assistant_text` and **must not** re-enter `awaiting_turn` |
| Job deleted / Off / rename | No row to promote or claim |

### Atomicity requirements (overrides current de-facto RMW)

Current `apply_usable_completion` and parts of claim use primary-key
read-modify-write. This feature **requires** predicate-safe transitions so
deadline promote and end-turn cannot silently clobber each other.

#### 1) Usable completion (`apply_usable_completion`)

Split concerns inside the lifecycle transaction:

**A. Idempotent progress (any live job state):**  
If stop reason is usable `end_turn` with non-empty final text and the turn token
is new:

- Always record `last_usable_turn_token`, increment `usable_turn_seq`, and
  refresh locale on the job when the row still exists, regardless of whether
  first-ready already happened. This preserves retry sequencing when a later
  turn completes while attempt 1 is running.

**B. First-assistant + first-ready (conditional):**  
Only when promoting first context:

```text
UPDATE … SET
  first_assistant_text = <bound final>,
  state = ready,
  … token/seq/locale/updated_at as required …
WHERE conversation_id = ?
  AND state IN (awaiting_turn, retry_wait)
  AND first_assistant_text IS NULL
  AND (token predicates / usable checks as today)
```

Notes:

- For the **first** ready path from `awaiting_turn`, `first_assistant_text IS NULL`
  is the write-once guard shared with deadline promote.
- For `retry_wait` → `ready` on a later usable turn, `first_assistant_text` is
  already `Some(...)` from the first snapshot. The state transition to `ready`
  for attempt 2 must still occur **without** replacing `first_assistant_text`.
  Implement as a separate conditional update for `retry_wait → ready` that does
  not touch `first_assistant_text`, or an equivalent CAS that preserves it.
- If deadline already set `first_assistant_text = Some("")` and `state = ready`,
  end-turn must leave that snapshot alone (no refine).

#### 2) Deadline promote

```text
UPDATE … SET
  state = ready,
  first_assistant_text = <bound partial or "">,  -- always Some
  updated_at = now
WHERE conversation_id = ?
  AND state = awaiting_turn
  AND first_assistant_text IS NULL
  AND first_prompt_at IS NOT NULL
  AND first_prompt_at <= now - deadline
```

`first_assistant_text IS NULL` is mandatory so promote cannot overwrite an
end-turn snapshot that won moments earlier.

#### 3) Claim (`claim_next_ready`)

- Conditional `ready → running` must guard at least `state = ready` and the
  observed claim identity (`conversation_id`). Prefer also CAS on the observed
  `usable_turn_seq` (or use an atomic update-returning pattern that derives
  `attempt_turn_seq` from the row being claimed) so a concurrent completion that
  advances seq cannot pair a stale attempt with a newer turn sequence.
- **Empty assistant:** treat **`Some("")` as valid**. Delete as unusable only when
  `first_user_text` is missing or empty after trim.
- **`None` on a Ready row is invalid** (malformed): delete the bad ready row and
  continue to the next candidate (do not conflate with deliberate empty via
  `unwrap_or_default()`).

### Capture

On an accepted linked prompt for an enrolled job:

1. **First-fields write-once** via conditional update only when both first fields
   are still unset:

```text
UPDATE … SET
  first_user_text = <bound visible>,
  first_prompt_at = now,
  locale = <wire>,
  updated_at = now
WHERE conversation_id = ?
  AND first_user_text IS NULL
  AND first_prompt_at IS NULL
```

2. A losing concurrent capture (another connection already wrote first fields)
   **must not** write either first field; it may still refresh `locale` only.
3. Per-connection prompt locks are **not** sufficient: two connections bound to
   the same conversation can both pass local locks. Database predicates are the
   source of truth.

## Deadline Sweep

### Loop ownership

`AutoTitleCoordinator::recover_and_start` continues to:

1. `recover_interrupted_jobs`
2. Register the live coordinator
3. Start `notification_loop` once under the existing `started` CAS

Additionally start a **deadline sweep loop** once under the same `started` gate
(or a sibling once-only atomic) so double `recover_and_start` does not spawn
duplicate sweepers:

```text
loop {
  match promote_deadline_elapsed_jobs(now) {
    Ok(promoted_count) if promoted_count > 0 => notify_ready(),
    Ok(_) => {},
    Err(e) => log warning; do not exit the loop
  }
  // Also heal lost wakes: if any ready rows exist, notify_ready()
  // (cheap existence check, or fold into claim drain hints).
  sleep(AUTO_TITLE_DEADLINE_SWEEP_INTERVAL)
}
```

Requirements:

- Run **one promote pass immediately** on start (before the first long sleep).
- Sweep does **not** acquire attempt permits. Permits remain claim-time only.
- **Catch and log every pass error**; continue after the interval. A transient DB
  error must not permanently kill title deadline promotion for the process.
- Because `notification_loop` is wake-driven, each sweep pass (or a periodic
  ready-row existence check) must **re-notify when Ready rows exist**, so a lost
  wake after commit is self-healing within one sweep interval.
- Notifications fire **only after** promote transactions commit.

### Promote algorithm (two-phase)

**Phase 1 — select (read-only DB):**  
Select up to `AUTO_TITLE_DEADLINE_SWEEP_BATCH` candidates ordered by
`(first_prompt_at ASC, conversation_id ASC)` where:

- `state = awaiting_turn`
- `first_user_text IS NOT NULL`
- `first_prompt_at IS NOT NULL`
- `first_prompt_at <= now - AUTO_TITLE_DEADLINE`
- `first_assistant_text IS NULL` (optional prefilter; CAS still enforces)

**Phase 2 — snapshot partials (no DB transaction held):**  
For the candidate conversation IDs, resolve partial assistant text via a
**batch** connection lookup (see Multi-connection selection). Do not hold the
global connection map lock across per-job awaits that re-enter the map.

**Phase 3 — short CAS writes:**  
For each candidate, apply the deadline promote UPDATE above. Count
`rows_affected == 1` as success.

**Phase 4 — after commit:**  
If any promoted (or any Ready rows need drain), `notify_ready()`.

### Multi-connection selection

Do **not** rely on “first HashMap match” from
`find_connection_by_conversation_id` as the sole snapshot source when multiple
connections may share a conversation id.

**Deterministic rule when multiple states match `conversation_id`:**

1. Prefer states with a non-empty in-flight `live_message`.
2. Among those, pick the latest `live_message.started_at` (or equivalent live
   start timestamp).
3. Tie-break by connection id string ascending for stability.
4. If none have `live_message`, partial is `""`.

Prefer a narrow batch API (e.g. `snapshot_partial_assistant_text(ids)`) that
walks the connection table **once** per sweep pass and returns a map
`conversation_id → String`. Tests inject a stub `PartialAssistantTextSource`.

If product later enforces a unique-owner invariant that makes multi-match
impossible, keep the deterministic rule as a defensive fallback and still test
the multi-match case.

### Partial assistant text contract

Do **not** use `latest_live_reply` (single truncated line; thinking/tool
fallbacks). Define a pure helper:

```rust
/// Full visible answer text from a live message, matching TurnComplete assembly.
/// No truncation. No thinking/tool label fallback.
fn visible_assistant_text(live: Option<&LiveMessage>) -> String
```

Semantics:

- `None` or empty content → `""`.
- From `live.content`, take `Text` blocks after the last `ToolCallRef` (or all
  `Text` if no tool calls), concatenated in order.
- Ignore Thinking, Plan, tool payloads, images.
- Deadline path then applies `bound_context` before persist.
- TurnComplete path must use the **same** helper. When the helper result trims
  empty, TurnComplete **must clear** `last_assistant_text` (no stale prior-turn
  leak). Title completion snapshots continue to use that cleared/empty final
  text for usable-completion gates.

Equality tests: for identical `LiveMessage` content, deadline assembly (before
bound_context) and TurnComplete assembly must match.

## Claim Rule Change

| `first_user_text` | `first_assistant_text` | Claim behavior |
| --- | --- | --- |
| empty / missing | any | Delete bad Ready row; continue |
| non-empty | `Some("")` | **Claim** (deadline empty partial) |
| non-empty | `Some(text)` | Claim as today |
| non-empty | `None` | Invalid Ready; delete and continue |

Runner prompt template unchanged; empty assistant yields an empty
`Final response:` section.

## Failure and Retry

| Case | Behavior |
| --- | --- |
| Deadline promote → run success | Finalize, delete job, `auto_title_finalized` |
| Attempt 1 failure, no newer usable turn | `retry_wait` |
| Attempt 1 failure, newer usable turn exists | `ready` for attempt 2 with **same** first snapshot preserved |
| `retry_wait` | Wait for next usable `end_turn` only; no deadline re-promote |
| Attempt 2 failure | Delete job; stop |
| Off / manual rename / soft-delete | Delete job; cancel active runner |
| Soft-delete / Off between select and promote CAS | CAS no-ops; harmless |

Deadline does not fire repeatedly for the same conversation once it leaves
`awaiting_turn`.

## Frontend

No new API, settings control, or event type.

Users observe the existing conversation title update path when finalize
commits. Delegation child **seed titles** from task text remain an immediate
UX fallback until an AI title finalizes (unchanged).

## Testing

### Service / promote / capture

1. Conditional capture sets `first_user_text` + `first_prompt_at` once.
2. Two-connection barrier: concurrent captures → exactly one first-field write;
   loser may update locale only.
3. Age ≥ 300s + `awaiting_turn` + NULL assistant → promote to Ready with
   `Some(partial)` including `Some("")`.
4. Younger than 300s → not promoted.
5. Ready / Running / RetryWait → promote CAS no-ops.
6. Barrier races both orders: sweep-then-end-turn and end-turn-then-sweep;
   only one first-assistant snapshot; no refine after deadline.
7. End-turn between Ready read and claim CAS: claim either wins cleanly or
   retries without pairing stale `attempt_turn_seq` incorrectly.
8. Migrated row with pre-existing `first_user_text` and NULL `first_prompt_at`:
   never deadline-promoted; end-turn still works.
9. Migrated/enrolled row with no prompt yet: first capture sets both fields and
   can later deadline-promote.

### Claim

10. Non-empty user + `Some("")` → claim succeeds.
11. Non-empty user + `None` → delete bad Ready; do not run.
12. Empty user → delete bad Ready.

### Coordinator / liveness

13. Successful promote commits then `notify_ready`; worker can claim.
14. Startup immediate promote pass (injected clock / short constants).
15. Transient promote error: loop continues; next interval runs again.
16. Double `recover_and_start`: single sweep loop / single notification loop.
17. Ready row with lost wake: next sweep (or ready existence check) re-notifies.
18. Off / rename / soft-delete between candidate select and CAS: no promote,
    no panic.
19. Running attempt recovery with and without newer usable turn; retry keeps
    deadline snapshot.

### Partial helper / multi-connection

20. Text after last tool call; thinking-only → empty; no live → empty.
21. Equality: TurnComplete assembly vs deadline helper for same live content.
22. Stale prior-turn text cleared when current turn has no answer text.
23. Multiple connections for one conversation: deterministic selection prefers
    newest live_message; batch API does not hold map lock across nested awaits.

### Migration

24. New migration upgrades a DB that already has auto_title_jobs; adds column +
    deadline index; preserves claim index; down migration restores prior shape.

### Optional integration

25. Capture → advance time ≥ 300s → sweep → finalize title without a real
    `end_turn`.

Prefer barrier-controlled concurrent tests (WAL / shared DB) for items 2, 6, 7,
and 18 rather than only sequential state transitions.

## File Touchpoints

| Area | Change |
| --- | --- |
| **New** DB migration | `first_prompt_at` + deadline index; down + upgrade tests |
| `auto_title_job` entity | New field |
| `auto_title/service.rs` | Conditional capture; promote; claim empty/`None`; completion CAS |
| `session_state` / shared helper | `visible_assistant_text`; TurnComplete refactor; clear-on-empty |
| `acp/manager` (or narrow trait) | Batch partial snapshot; multi-connection selection |
| `auto_title/coordinator.rs` | 101s sweep loop; error isolation; ready re-notify; ConnectionManager/trait |
| Tests | Cases above |

## Risks and Acceptance

- Titles may appear between ~300s and ~401s after first prompt when end-turn is
  slow; accepted.
- Empty-partial titles depend on the quality of the first user task text.
- Sweep is low frequency (101s) with a bounded batch; connection map scans must
  stay short and non-reentrant under lock.
- Coarse scanning means no exact 300s guarantee; intentional.
- Pre-migration jobs that already captured a prompt never get deadline
  promotion; accepted (YAGNI backfill).

## Review History

- 2026-07-19: Product brainstorm approved Approach 2 (300s / 101s / empty
  partial OK / no refine).
- 2026-07-19: Codex design review (`task_id` b6e6f897-b9dd-49a6-ad59-8de476d860c0)
  raised Important findings on CAS, multi-connection capture/snapshot, partial
  helper contract, sweep liveness, migration shape, and test gaps. Spec updated
  to incorporate those fixes.

## Implementation Note

Do not implement until this spec is reviewed and an implementation plan is
written via the writing-plans skill.
