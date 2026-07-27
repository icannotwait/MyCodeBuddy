# Delegation Work-Unit Sticky Runtime UI Design

## Status

Drafted from brainstorming on 2026-07-27 (parent card continuity + delegated
child interrupt text). Checkpoint duration for parent continuation remains the
backend constant `CONTINUATION_CHECKPOINT_MS` (**600_000 ms** as of 2026-07-27);
this design does **not** change Join ownership or remove the checkpoint.

## Problem

Users experience long multi-agent work (B2D / Join / continuation) as one
**large task**, but the parent delegation card and child transcript still follow
**single ACP turn / single `task_id` run** lifecycle:

1. Child turn interrupt injects Codex `*Conversation interrupted*` into the
   delegated child body — reads like the whole conversation died.
2. Orchestration cancel (`parent_turn_failed`, join abandon, etc.),
   `continue_delegation`, same-child replacement, and continuation **checkpoint
   re-entry** (every 600s while still running) cause the parent card to flash
   terminal / empty / cold-start chrome.
3. Elapsed time and tool counts reset or blank across `task_id` changes even
   though the same child / work unit is still active.
4. Desired steady state while the unit is open:

   `生成中 | {elapsed} | {N} 次工具调用`

   (plus optional edit rollup segments per 2026-07-19 card chrome).

Checkpoint wakes and event wakes both start a **new parent turn** via hidden
continuation prompt; sticky UI must make those re-entries **feel continuous**.

## Goals

- Parent surfaces using `useDelegationCardModel` keep **running / generating**
  operational chrome for the whole **sticky work unit**, not only the current
  `task_id`.
- Operational line while sticky-active:

  ```text
  {streamingLabel} | {elapsed} | {N tool uses} [| edit rollup…]
  ```

  where `streamingLabel` reuses `Folder.chat.liveTurnStats.streaming`
  (zh-CN: 「生成中」).
- Across orchestration cancel → continue/replace, re-seed gaps, and checkpoint
  re-entry: no flash to failed/interrupted terminal styling; no tool count flash
  to 0; elapsed keeps advancing from unit anchor.
- Delegated child sessions (`conversation.parent_id != null`): suppress
  `*Conversation interrupted*` agent text (ingest and/or render). Standalone
  (non-delegated) Codex sessions keep current Codex behavior.
- Display-only: no Join Detach, no Broker ownership change, no vendor
  `codex-acp` patch required for V1.

## Non-Goals

- Removing or re-tuning `CONTINUATION_CHECKPOINT_MS` (already 600s; separate
  change).
- Starting parent turns earlier or later than Broker/continuation already do.
- Changing child execution timeouts or provider cache policy.
- Rewriting global footer copy for all agents (child `Response interrupted`
  footer may stay V1).
- Native / non-Codeg collab cards without Codeg child conversations.
- Fabricating tool counts that were never observed.
- Backend unit-level rollup columns (optional later).

## Related specs

| Spec | Relationship |
|------|----------------|
| `2026-07-17-event-driven-delegation-join-design.md` | Join predicate, card runtime stats wire |
| `2026-07-19-delegation-card-title-and-runtime-ui-design.md` | Card model, chrome, merge precedence |
| `2026-07-19-delegation-continuation-design.md` | Suspend + 600s checkpoint + hidden parent turn |
| This document | Sticky **display** continuity across runs / interrupts |

## Selected approach

**Frontend sticky projection** keyed by work unit identity, folded into
`buildDelegationCardModel` / `useDelegationCardModel`, plus narrow suppress of
Codex interrupt markdown on delegated children.

```text
live binding / meta / snapshot / child projection
        │
        ▼
StickyRuntimeStore  (frontend module)
  key → phase, anchorStartedAt, tool fold state, lastTaskId, …
        │
        ▼
buildDelegationCardModel (+ sticky merge)
        │
        ├── lifecycleStatus running while active_sticky
        ├── elapsed from unit anchor
        ├── toolCallCount = unit fold
        └── showGeneratingSegment
        │
        ▼
DelegationCardChrome  (streaming | elapsed | tools | …)
```

## Sticky key

Priority (first available wins):

1. `work_unit_key` from meta / snapshot / run DTO when present.
2. Else `(parentConversationId, childConversationId)` when both known.
3. Else `task_id` only — **no cross-run stickiness** (single-run semantics).

React list keys for card rows that represent the same unit **must** prefer the
sticky key (not ephemeral `task_id`) so remounts do not look like cold start.

## Phase machine

```text
(none) -- first observed start / running --> active_sticky
active_sticky -- stats --> fold tools / keep anchor
active_sticky -- orchestration cancel --> stay active_sticky
active_sticky -- continue/replace new task_id --> stay active_sticky, fold tools
active_sticky -- re-seed hole --> keep last frame
active_sticky -- completed (ok) --> terminal
active_sticky -- business failed --> terminal
active_sticky -- user cancel_delegation / usercancel --> terminal
active_sticky -- orphan timeout (no live + no recovery signal) --> weak interrupt or terminal
terminal -- same sticky key new running (legal continue) --> active_sticky
            (anchor NOT reset; tools continue fold)
```

### Orchestration cancel (keep sticky)

Treat as non-terminal for display when `error_code` / cancel reason is one of
(at least):

- `parent_turn_failed`
- `parent_canceled` when caused by parent turn lifecycle (not explicit
  `cancel_delegation` usercancel)
- `join_abandoned`
- `parent_disconnected` while recovery is still expected
- continuation-related intermediate states that clear live binding without
  user abandon

### True terminal (release sticky generating chrome)

- Run `completed` / status `ok`
- Business `failed` / `err` that is not orchestration cancel above
- Explicit user cancel of the delegation (`cancel_delegation` with
  usercancel / product-equivalent stop of that child unit)
- Sticky orphan timeout (see below)

### Orphan timeout

Default **900_000 ms (15 minutes)** with no live running binding, no
in-flight continue/replace signal, and no open attention — then leave
`active_sticky` so cards cannot claim “generating” forever. Configurable as a
named frontend constant (not a user setting in V1). Chosen **above** the 600s
continuation checkpoint so a quiet checkpoint cycle alone does not trip the
orphan guard.

## Elapsed

| State | Formula |
|-------|---------|
| `active_sticky` | `now - anchorStartedAt` via existing 1s running ticker |
| re-seed hole | same; do not clear elapsed |
| true terminal | `finishedAt - anchorStartedAt` when both valid; else last observed |

`anchorStartedAt` = first observed `started_at` for the sticky key; **never
reset** on same-key continue/replace.

Invalid timestamps: omit segment (2026-07-19 rules); never `NaN` / negative.

## Tool counts

Per sticky key:

```text
on stats(taskId, count):
  if taskId != lastTaskId:
    base += peakOfLastTask
    lastTaskId = taskId
    peakOfLastTask = count
  else:
    peakOfLastTask = max(peakOfLastTask, count)
  display = base + peakOfLastTask
```

- Never invent counts without observed stats.
- Re-seed hole: keep last `display`.
- Same `task_id`: monotonic non-decreasing display for that run’s peak.

## Operational line (chrome)

When `lifecycleStatus === "running"` **or** sticky phase is `active_sticky`:

1. Prepend localized streaming label (`liveTurnStats.streaming`).
2. Then elapsed (if present).
3. Then tool count (if present).
4. Then edit rollup segments (existing).

Join with `" | "` among **present** segments only (existing chrome rule).

Status badge / primary chrome: treat `active_sticky` like running — **not**
failed/interrupted terminal colors for orchestration cancel.

Attention: open attention badge remains; operational line stays.

## Conversation interrupted suppress

| Session | Behavior |
|---------|----------|
| Delegated child (`parent_id != null`) | Suppress agent text that normalizes to exactly `*Conversation interrupted*` (trim; optional surrounding markdown emphasis only) |
| Standalone | Unchanged |

Paths:

1. **Preferred live:** drop matching chunk before durable transcript part write
   when conversation is known delegated.
2. **Render fallback:** hide matching historical parts on display.

Do not use this text to drive parent sticky phase.

## Data flow and ownership

| Layer | Responsibility |
|-------|----------------|
| `src/lib/delegation-sticky-runtime.ts` (new) | Pure store: key, fold, phase transitions, orphan clock hooks for tests |
| `src/hooks/use-delegation-card-model.ts` | Feed events into sticky store; merge into model |
| `src/components/message/delegation-card-chrome.tsx` | Render generating prefix when model asks |
| ACP ingest / message list | Child interrupt text suppress |
| Broker / continuation | **Unchanged** |

Parent agent timing remains: event wake or 600s checkpoint → hidden
continuation turn. Sticky UI does **not** call tools or start turns.

## Failure / edge cases

| Case | Handling |
|------|----------|
| Missing child id | task_id-only key; no cross-run stick |
| Two parents same child (should not happen) | key includes parent id |
| Success then continue same child | re-enter `active_sticky`; keep anchor; fold tools |
| Clock skew | same as 2026-07-19 |
| Replacement to new child | new sticky key; old key terminals as abandoned/replaced when product marks it |

## Testing

1. Running + stats → generating line + N tools.
2. `parent_turn_failed` cancel → still generating; elapsed continuous; N not 0.
3. New `task_id` same child → no remount blank; N = fold(old)+new.
4. Binding drop re-seed → last frame held.
5. Completed ok → terminal; elapsed frozen.
6. User cancel → terminal; not generating.
7. Attention open → badge + operational line.
8. Orphan timeout with fake clock → leave sticky generating.
9. Delegated child: interrupt text not shown / not stored (per path).
10. Standalone: interrupt text still shown.
11. Two parallel children: independent sticky buckets.
12. No assertion that sticky causes parent turn start (display-only).

## Acceptance criteria

- While sticky-active, parent card operational line matches
  `生成中 | time | N tools` pattern in zh-CN (and locale equivalents).
- Checkpoint / continue / orchestration cancel re-entry does not flash
  terminal failure styling or zero tools.
- Subagent completion before checkpoint still wakes parent immediately
  (existing backend; regression-safe).
- Delegated child transcripts do not present `*Conversation interrupted*` as
  assistant conclusion text.
- Standalone Codex interrupt text unchanged.
- No Broker Join ownership or Detach changes in this slice.

## Implementation notes

- Prefer pure sticky helpers + unit tests before wiring React.
- Reuse `Folder.chat.liveTurnStats.streaming` / `toolUseCount` / elapsed formatters.
- Preserve 2026-07-19 task_id guards on live event application; fold only after
  identity match into sticky key.
- Stage only task-owned paths; never `git add -A`.
- `docs/superpowers/**` may require `git add -f` when ignored.
