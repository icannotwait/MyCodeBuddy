# Automatic Title Deadline Sweep

Date: 2026-07-19

Status: Design approved; written specification awaiting final review

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

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Architecture | Coordinator periodic sweep (Approach 2) |
| Deadline | 300 seconds from first prompt capture |
| Sweep interval | 101 seconds |
| Timing precision | Coarse; ~300–401s window is acceptable |
| Deadline context | First user task + current partial assistant stream |
| Empty partial at deadline | Still promote; generate from user task only |
| Post-success refine | No; successful generate is final (`auto_title_finalized`) |
| End-turn before deadline | Still promotes immediately with full final text |
| Settings | Hard-coded constants; no new UI |
| Legacy jobs without `first_prompt_at` | Skip deadline; end-turn only |
| Startup | Run one sweep immediately, then every 101s |

## Relationship to Existing Auto-Title Design

This spec **extends** `2026-07-16-auto-title-reference-search-design.md`. Unless
explicitly overridden here, enrollment, settings, hidden runner isolation,
internal session exclusion, locale rules, bound_context, concurrency permits,
manual rename / Off cancellation, and dual-runtime cores remain unchanged.

### Overrides

| Topic | Previous | This change |
| --- | --- | --- |
| Ready trigger | Only first usable `end_turn` | Usable `end_turn` **or** deadline sweep |
| Claimability of empty assistant | Ready rows with empty assistant deleted | Empty assistant allowed; only empty user is fatal |
| Assistant context source | Only completion final text | Final text **or** live partial snapshot at promote |

## Constants

```rust
/// Minimum age of first-prompt capture before deadline promotion.
const AUTO_TITLE_DEADLINE: Duration = Duration::from_secs(300);

/// How often the coordinator scans for deadline-eligible jobs.
const AUTO_TITLE_DEADLINE_SWEEP_INTERVAL: Duration = Duration::from_secs(101);
```

Tests may inject shorter values via constructor parameters or `#[cfg(test)]`
hooks so unit tests do not sleep for minutes.

## Schema

Add to `auto_title_jobs`:

| Column | Type | Purpose |
| --- | --- | --- |
| `first_prompt_at` | nullable `DateTimeUtc` | Instant when `first_user_text` was first written; deadline origin |

- Written **write-once** in the same capture path that first sets
  `first_user_text`.
- Do **not** use `updated_at` as the deadline origin: later locale refreshes and
  other job updates would skew the 300s check.
- Index: `(state, first_prompt_at, conversation_id)` for deterministic sweeps of
  `awaiting_turn` rows past the deadline.
- Existing rows migrate with `first_prompt_at = NULL`. NULL jobs are ineligible
  for deadline promotion and remain end-turn-only. No backfill from
  `updated_at` in the first version.

## Lifecycle

### Dual ready paths

```text
awaiting_turn --usable end_turn--> ready   (first_assistant = full final text)
awaiting_turn --deadline sweep--> ready   (first_assistant = partial or "")
ready --worker claim--> running(attempts += 1)
running --valid title--> generated (job deleted, auto_title_finalized)
running(attempts = 1) --failure--> retry_wait | ready(attempt 2)  [unchanged]
retry_wait --next usable end_turn--> ready(attempt 2)
```

`retry_wait` is **never** deadline-promoted. Deadline only advances jobs still
waiting for their **first** usable completion snapshot.

### Who wins

Both paths use conditional updates on `state = awaiting_turn` (or the existing
completion transaction predicates). Exactly one path may move a given job into
`ready` for the first attempt.

| Race | Result |
| --- | --- |
| Usable `end_turn` first | Job ready with full final text; sweep CAS no-ops |
| Deadline sweep first | Job ready with partial (possibly empty); later `end_turn` does **not** overwrite `first_assistant_text` when already `Some`, and does not re-enter `awaiting_turn` |
| Job deleted / Off / rename | No row to promote or claim |

This matches the product rule: deadline success is final; full reply does not
refine.

### Capture

On the first accepted linked prompt for an enrolled job:

1. Set `first_user_text` once (existing).
2. Set `first_prompt_at = now` once (new), same write.
3. Locale behavior unchanged.

Later prompts may still refresh locale while the job lives; they must not reset
`first_prompt_at` or `first_user_text`.

## Deadline Sweep

### Loop ownership

`AutoTitleCoordinator::recover_and_start` continues to:

1. `recover_interrupted_jobs`
2. Register the live coordinator
3. Start `notification_loop` once

Additionally start a **deadline sweep loop** once per process:

```text
loop {
  promote_deadline_elapsed_jobs(now)
  if promoted_count > 0 {
    notify_ready()
  }
  sleep(AUTO_TITLE_DEADLINE_SWEEP_INTERVAL)
}
```

- Run **one promote pass immediately** on start (before the first long sleep) so
  restart recovers already-elapsed jobs without waiting another 101s.
- Sweep does **not** acquire attempt permits. Permits remain claim-time only.
- Notifications remain wake hints; the database is still the queue.

### Promote algorithm

For each candidate job:

**Select** where:

- `state = awaiting_turn`
- `first_user_text IS NOT NULL` (and trim non-empty at claim time)
- `first_prompt_at IS NOT NULL`
- `first_prompt_at <= now - AUTO_TITLE_DEADLINE`

For each `conversation_id`:

1. Resolve partial assistant text (see next section).
2. `bound_context` the partial (empty string stays empty).
3. Conditional update:

```text
UPDATE auto_title_jobs SET
  state = ready,
  first_assistant_text = <bounded partial or "">,
  updated_at = now
WHERE conversation_id = ?
  AND state = awaiting_turn
  AND first_prompt_at IS NOT NULL
  AND first_prompt_at <= now - deadline
```

4. Count successful updates; if any, `notify_ready()` after the batch (or after
   each success; coalescing is fine).

Prefer one short DB transaction per batch or per row; either is acceptable if
tests prove CAS correctness under concurrent completion.

### Partial assistant text source

Reuse the same visible-answer assembly as `TurnComplete` on `SessionState`:

- Read the target conversation's linked connection state when present.
- From `live_message.content`, take `Text` blocks after the last
  `ToolCallRef` (or all text if no tool calls).
- Ignore Thinking, Plan, tool payloads, images.
- If no connection, no live message, or only non-text progress → `""`.

Extract a shared helper (for example
`visible_assistant_text_from_live(live) -> String`) used by:

- `TurnComplete` final assembly (refactor existing logic onto the helper)
- Deadline promote snapshot

Coordinator needs a way to read live state. Production construction already has
`ConnectionManager` for the hidden runner path; hold a `ConnectionManager` (or a
narrow `PartialAssistantTextSource` trait) on the coordinator for sweeps.
Tests inject a stub that returns configured partials or empty strings.

## Claim Rule Change

Current `claim_next_ready` deletes ready jobs when
`first_assistant_text` is empty. That conflicts with deadline-empty partials.

**New rule:**

- Delete as unusable only when **`first_user_text` is empty after trim**.
- **Allow empty `first_assistant_text`**; claim and run the hidden agent with an
  empty `Final response:` section in the existing title prompt template.

Usable `end_turn` still requires non-empty final text before
`apply_usable_completion` sets `became_ready`, so end-turn quality gates are
unchanged. Empty assistant context only arrives via deadline promote.

## Failure and Retry

Unchanged from the base design except for the empty-assistant claim fix:

| Case | Behavior |
| --- | --- |
| Deadline promote → run success | Finalize, delete job, `auto_title_finalized` |
| Attempt 1 failure, no newer usable turn | `retry_wait` |
| Attempt 1 failure, newer usable turn exists | `ready` for attempt 2 |
| `retry_wait` | Wait for next usable `end_turn` only; no deadline re-promote |
| Attempt 2 failure | Delete job; stop |
| Off / manual rename / soft-delete | Delete job; cancel active runner |

Deadline does not fire repeatedly for the same conversation. Once promoted out
of `awaiting_turn`, the sweep no longer selects it.

## Frontend

No new API, settings control, or event type.

Users observe the existing conversation title update path when finalize
commits. Delegation child **seed titles** from task text remain an immediate
UX fallback until an AI title finalizes (unchanged).

## Testing

### Service / promote

1. `first_prompt_at` is written once with the first `first_user_text`.
2. Job with age ≥ 300s and `awaiting_turn` promotes to `ready` with partial
   (including empty string).
3. Job younger than 300s is not promoted.
4. Jobs in `ready` / `running` / `retry_wait` are not rewritten by promote CAS.
5. Race with `apply_usable_completion`: only one first-ready transition; if
   deadline wrote `first_assistant_text` first, end-turn does not overwrite it.

### Claim

6. Non-empty user + empty assistant → claim succeeds (not deleted).
7. Empty user → still deleted as a bad ready row.

### Coordinator

8. Successful promote results in `notify_ready` and a drain/claim opportunity.
9. Startup runs an immediate promote pass (use injected clock / short constants
   in tests).
10. Off cancellation still removes pending work; sweep is harmless on missing
    rows.

### Partial helper

11. Text after last tool call assembles; thinking-only → empty; no live → empty.

### Optional integration

12. Capture → advance time ≥ 300s → sweep → finalize title without a real
    `end_turn`.

## File Touchpoints

| Area | Change |
| --- | --- |
| DB migration | `first_prompt_at` column + index |
| `auto_title_job` entity / models | New field |
| `auto_title/service.rs` | Capture timestamp; promote function; claim empty-assistant |
| `session_state` / shared helper | Visible partial assembly |
| `auto_title/coordinator.rs` | 101s sweep loop; connection partial reads |
| `auto_title` tests + focused integration | Cases above |

## Risks and Acceptance

- Titles may appear between ~300s and ~401s after first prompt when end-turn is
  slow; accepted.
- Empty-partial titles depend on the quality of the first user task text.
- Sweep read locks on session state are infrequent (101s) and low volume.
- Coarse scanning means no exact 300s guarantee; that is intentional.

## Implementation Note

Do not implement until this spec is reviewed and an implementation plan is
written via the writing-plans skill.
`)