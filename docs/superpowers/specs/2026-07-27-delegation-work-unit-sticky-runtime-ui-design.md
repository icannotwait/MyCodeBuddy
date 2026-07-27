# Delegation Work-Unit Sticky Runtime UI Design

## Status

Drafted from brainstorming on 2026-07-27 (parent card continuity + delegated
child interrupt text). Revised after parallel Design review (Grok + Codex) on
2026-07-27: identity namespacing, latest-only projection, recovery-gated keep-
sticky, peak-by-task fold, store API, badge coercion, interrupt suppress as
presentation-only. Checkpoint duration for parent continuation remains the
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
- Delegated child sessions: suppress `*Conversation interrupted*` **assistant**
  text at presentation/local-materialization (not durable vendor transcript
  rewrite). Standalone (non-delegated) Codex sessions keep current Codex
  behavior.
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
- Replacing multi-run message-stream history with a single aggregate row in V1
  (inline cards remain one row per parent tool call).

## Related specs

| Spec | Relationship |
|------|----------------|
| `2026-07-17-event-driven-delegation-join-design.md` | Join predicate, card runtime stats wire |
| `2026-07-19-delegation-card-title-and-runtime-ui-design.md` | Card model, chrome, merge precedence |
| `2026-07-19-delegation-continuation-design.md` | Suspend + 600s checkpoint + hidden parent turn |
| This document | Sticky **display** continuity across runs / interrupts |

**Layout amendment:** operational line gains a streaming prefix while sticky-
active; source precedence, task_id guards, attention chrome, and recovery
ordering from 2026-07-19 remain.

## Selected approach

**Frontend sticky projection** keyed by namespaced work-unit identity, held in
an external store, folded into `buildDelegationCardModel` /
`useDelegationCardModel` for the **latest eligible card only** per unit, plus
narrow presentation suppress of Codex interrupt markdown on delegated children.

```text
live binding / meta / snapshot / child projection
        │
        ▼ observe() in effects / event handlers (not pure build)
StickyRuntimeStore  (useSyncExternalStore)
  namespaced key → phase, anchor, peakByTaskId, activeTaskId, …
        │
        ▼
buildDelegationCardModel (pure merge; no store mutation)
        │
        ├── latest-only: historical sibling cards keep frozen terminal snapshot
        ├── lifecycleStatus + badge status running while active_sticky
        ├── elapsed from unit anchor
        ├── toolCallCount = sum(peakByTaskId)
        └── showGeneratingSegment
        │
        ▼
DelegationCardChrome  (streaming | elapsed | tools | …)
```

## Sticky identity

### Canonical key material

Always namespace by backend + parent. Never key a module-level bucket by bare
`work_unit_key` alone (collides across parents / backends).

```text
StickyIdentity = {
  backendCacheKey: string,          // local/remote backend isolation
  parentConversationId: number,
  unit:
    | { kind: "work_unit"; workUnitKey: string }
    | { kind: "parent_child"; childConversationId: number }
    | { kind: "task"; taskId: string }   // no cross-run stickiness
}
```

`stickyKeyToString` is a stable, non-user-facing serialization for maps and
tests (not a React list key for historical stream rows).

### Resolution priority (when parent id known)

1. If a trustworthy `workUnitKey` is present on card sources **and**
   `parentConversationId` + `backendCacheKey` are known →
   `{ backend, parent, work_unit }`.
2. Else if `childConversationId` known →
   `{ backend, parent, parent_child }`.
3. Else `task_id` only → single-run semantics (no cross-run stick).

If `parentConversationId` is unknown, **do not** use bare `work_unit_key` for
cross-run stickiness; fall back to task-only.

### V1 wire reality for `work_unit_key`

As of this design, frontend `DelegationRunSnapshot` / meta / live binding
**do not** expose `work_unit_key` (workflow UI redacts it). V1 practical key is
therefore almost always **`(backendCacheKey, parentConversationId,
childConversationId)`**. Optional future read-only projection of `work_unit_key`
on run snapshot/meta is allowed as display-only data and upgrades priority 1
without changing ownership. Parsing tool-input `work_unit_key` is lowest-trust
fallback only if other fields are absent — never alone across parents.

### UI ownership: latest-only sticky apply

Inline message stream mounts **one card per parent tool call**
(`delegate_to_agent` and each `continue_delegation`), keyed by `toolCallId` /
`parentToolUseId` (existing). V1 does **not** coalesce history into one row.

Sticky generating chrome applies only to the **latest** card for a sticky
identity:

- Determine latest by durable ordering: highest known run generation when
  present, else latest observed `task_id` / parent tool-use admission order
  recorded in the sticky bucket (`activeTaskId` / `activeParentToolUseId`).
- Older sibling cards for the same unit remain **frozen** at their own terminal
  snapshot (no re-activation to generating when a later continue starts).
- Overlay surfaces that already group by child (`child:{id}`) may project
  sticky for the group’s latest run only.

Do **not** assign React list keys of historical stream siblings to the shared
sticky key (would create duplicate keys). Overlay group keys may keep
child-based stability.

### Replacement and child change

- Same child + continue/replace same unit → same sticky identity; keep anchor;
  fold tools; update `activeTaskId`.
- Explicit replacement to a **new child conversation** → **new** sticky
  identity (parent_child unit changes). Previous bucket terminalizes when
  product marks superseded (`replaced_task_id` / child-id change observed).
  Elapsed/tools do **not** carry across child ids in V1.
- Same bare `work_unit_key` with a new child is still a **new** sticky identity
  under parent_child fallback; if work_unit wire exists later, still namespace
  by parent+backend and treat child change as separate visual unit unless a
  later design says otherwise.

## Phase machine

```text
(none) -- first observed start / running --> active_sticky
active_sticky -- stats --> fold tools / keep or min-anchor
active_sticky -- keep-sticky intermediate (recovery-gated) --> stay active_sticky
active_sticky -- continue/replace new task_id same unit --> stay active_sticky
active_sticky -- re-seed hole (binding drop while recovery-owned) --> keep last frame
active_sticky -- completed (ok) --> terminal
active_sticky -- business failed / err (not recovery-owned keep-sticky) --> terminal
active_sticky -- parent_canceled (user Stop cascade) --> terminal
active_sticky -- cancel_delegation / usercancel --> terminal
active_sticky -- orphan timeout (no recovery owner) --> terminal
terminal -- same sticky identity new running (legal continue) --> active_sticky
            (anchor NOT moved later; tools continue fold via peakByTaskId)
```

### Recovery-owned keep-sticky (positive signal required)

Durable terminal child outcomes such as `parent_turn_failed`, `join_abandoned`,
`parent_disconnected`, or binding-clear re-seed holes may stay
`active_sticky` **only while a positive recovery owner exists** for that unit.
Otherwise they terminalize immediately (no 15-minute false “生成中”).

**Positive recovery signals (frontend observables, any one holds):**

1. Live `DelegationBinding` for this unit’s active `task_id` / child with
   non-terminal running state.
2. Child conversation projection status `running` (or equivalent live turn
   ownership for the delegated child).
3. Non-terminal run snapshot for the active task.
4. Open attention request on the unit’s active child/task.
5. Parent continuation / waiting-for-subagents projection that still owns this
   child (when available in existing frontend waiting projection).
6. Explicit in-flight continue/replace admission for the same sticky identity
   (new live binding or snapshot observed for a newer task on same unit).

Absent all of the above, do **not** mask a terminal orchestration outcome.

### Cancellation / error-code classification

| Code / reason | Default display | Notes |
|---------------|-----------------|-------|
| `parent_canceled` | **True terminal** | Wire code for parent Stop cascade; frontend has no lifecycle-vs-usercancel bit. Do not keep sticky on this code alone. |
| `cancel_delegation` / usercancel | **True terminal** | Explicit user cancel of that child unit. |
| `parent_turn_failed` | Keep sticky **iff** recovery-owned | Else terminal. |
| `join_abandoned` | Keep sticky **iff** recovery-owned | Real abandon without continue → terminal. |
| `parent_disconnected` | Keep sticky **iff** recovery-owned | Else terminal. |
| `parent_ended` / bare `canceled` | **True terminal** unless recovery-owned and product maps them as intermediate | Align with full parent-end code set on children. |
| Business `failed` / `err` not in keep-sticky set | **True terminal** | |
| `completed` / `ok` | **True terminal** | Release generating chrome. |

### Orphan timeout

Default **900_000 ms (15 minutes)** measured from the moment the unit is
`active_sticky` **and** has **no** positive recovery signal. Named frontend
constant `STICKY_ORPHAN_TIMEOUT_MS` (not a user setting in V1). Chosen
**above** the 600s continuation checkpoint so a quiet checkpoint cycle alone
does not trip the orphan guard.

- **Start / resume orphan clock** when recovery signals are all false while
  phase is still `active_sticky`.
- **Reset / cancel orphan clock** when any positive recovery signal returns.
- On fire: force `terminal` (release generating chrome).
- Orphan only applies to recovery-owned sticky gaps — never used to extend
  true user cancel / `parent_canceled` / business failure.

### Observation ordering (stale terminal fence)

Per sticky bucket maintain:

- `peakByTaskId: Map<taskId, peakCount>`
- `taskMeta: Map<taskId, { generation?, startedAt?, finishedAt?, parentToolUseId? }>`
- `activeTaskId` / generation of the newest admitted run for the unit

Unit phase is derived from the **newest** admitted run (highest generation, or
admission order). A late terminal event for an **older** task must:

- still update that task’s peak count if observed,
- **not** force unit phase to terminal while a newer task is running / active.

Pure builder remains side-effect free; observations enter the store only from
effects / handlers.

## Elapsed

| State | Formula |
|-------|---------|
| `active_sticky` | `now - anchorStartedAt` via existing 1s running ticker |
| re-seed hole | same; do not clear elapsed |
| true terminal | `finishedAt - anchorStartedAt` when both valid; else last observed |

`anchorStartedAt` = **earliest valid** `started_at` among accepted lineage tasks
for the sticky identity. Never move the anchor **later** on same-key
continue/replace. If a delayed older task hydrates with an earlier start,
anchor may move **earlier** once (min of valid starts).

Invalid timestamps: omit segment (2026-07-19 rules); never `NaN` / negative.

After true `completed` ok, a later legal continue on the **same** sticky
identity re-enters `active_sticky` and **keeps** the existing anchor (unit
wall-clock continuity for same child/unit). New child identity resets.

## Tool counts

Per sticky key use **peak-by-task** (replay / A-B-A safe):

```text
on stats(taskId, count):
  if count is not finite non-negative integer: ignore
  if taskId not in accepted lineage for this unit: ignore
  peakByTaskId[taskId] = max(peakByTaskId[taskId] ?? 0, count)
  display = sum(peakByTaskId values)
```

- Never invent counts without observed stats.
- Re-seed hole: keep last `display`.
- Same `task_id`: monotonic non-decreasing peak for that task.
- Out-of-order `T1=5 → T2=2 → late T1=5` → display **7**, not 12.
- Edit rollup / touched-files segments remain **current-run stats only** in V1
  (not folded across task ids).

## Operational line (chrome)

When sticky phase is `active_sticky` **and** this card is the **latest** for
the identity (or non-sticky path already has `lifecycleStatus === "running"`):

1. Prepend localized streaming label (`liveTurnStats.streaming`).
2. Then elapsed (if present).
3. Then tool count (if present).
4. Then edit rollup segments (existing, current-run only).

Join with `" | "` among **present** segments only (existing chrome rule).

### Badge and lifecycle coercion

While sticky-active on the latest card:

- Project `lifecycleStatus` to running-equivalent for chrome/ticker eligibility.
- Coerce **badge `status`** (passed to `StatusBadge`) to the same
  running/active-equivalent — not only lifecycle — so users never see red err
  badge + 「生成中」 ops line together.
- Ensure sticky anchor satisfies ticker eligibility (`startedAt` valid) for the
  generating line to advance.

Attention: open attention badge remains; operational line stays.

Historical non-latest cards: keep their terminal badge/lifecycle; no
generating segment.

## Conversation interrupted suppress

| Session | Behavior |
|---------|----------|
| Delegated child | Suppress **assistant** text that normalizes to exactly `*Conversation interrupted*` (trim; optional surrounding markdown emphasis only) |
| Standalone | Unchanged |

**Presentation / local-materialization only** (display-only V1):

1. **Live path:** filter matching **assistant** chunks when applying local
   materialization in the conversation runtime / ACP apply path for a session
   known to be a delegation child (`summary.parent_id != null`, or live
   connection / projection flags such as `isDelegationChild` /
   `liveOwnsActiveTurn` when detail is not yet hydrated).
2. **Render fallback:** hide matching historical assistant parts on display
   when delegated identity is known.

Do **not** claim durable vendor transcript deletion without a backend/vendor
change. Do **not** suppress user-authored identical text. Do **not** treat
typed “Response interrupted” footer / outcome metadata as suppress targets
(non-goal: footer may stay).

Do not use this text to drive parent sticky phase.

## Store contract

| Layer | Responsibility |
|-------|----------------|
| `src/lib/delegation-sticky-runtime.ts` (new) | Pure helpers + external store: identity, fold, phase, orphan clock hooks |
| `src/hooks/use-delegation-card-model.ts` | `observe` in effects/handlers; pure merge into model; latest-only flag |
| `src/components/message/delegation-card-chrome.tsx` | Render generating prefix when model asks |
| Runtime materialization / message list | Child interrupt text suppress (presentation) |
| Broker / continuation | **Unchanged** |

Store requirements:

- `subscribe` / `getSnapshot` via `useSyncExternalStore`.
- Mutations only from effects, event handlers, or explicit observe APIs — **not**
  from pure `buildDelegationCardModel`.
- Backend-scoped reset when backend cache key changes; bounded retention /
  eviction for terminal buckets.
- Injected clock for orphan timeout tests; timer must fire even when ticker is
  ineligible (invalid `startedAt`).
- Cold reload: empty memory → no sticky until live/meta observations rebuild
  (acceptable V1; no durable frontend sticky persistence).

Parent agent timing remains: event wake or 600s checkpoint → hidden
continuation turn. Sticky UI does **not** call tools or start turns.

## Failure / edge cases

| Case | Handling |
|------|----------|
| Missing child id | task-only key; no cross-run stick |
| Two parents / backends same work_unit_key | namespaced; isolated buckets |
| Success then continue same child | re-enter `active_sticky`; keep/min anchor; fold tools; latest card only |
| Clock skew | same as 2026-07-19 |
| Replacement to new child | new sticky identity; old terminalizes on superseded mark |
| Late old terminal after new running | counts update; unit phase follows newest |
| Multi-card stream same child | historical frozen; only latest generating |
| Cross-backend switch | reset / isolated maps by `backendCacheKey` |

## Testing

1. Running + stats → generating line + N tools.
2. `parent_turn_failed` **with** recovery owner → still generating; elapsed
   continuous; N not 0.
3. `parent_turn_failed` **without** recovery owner → terminal immediately.
4. New `task_id` same child → latest card continuous; historical card frozen;
   N = sum(peaks).
5. Binding drop re-seed with recovery owner → last frame held.
6. Completed ok → terminal; elapsed frozen.
7. User Stop / `parent_canceled` → terminal; not generating.
8. Explicit `cancel_delegation` → terminal; not generating.
9. Attention open → badge + operational line.
10. Orphan timeout with fake clock → leave sticky generating.
11. Delegated child: interrupt assistant marker hidden on live + render paths;
    **footer** “Response interrupted” still shown.
12. Standalone: interrupt text still shown; user-role identical text not
    suppressed.
13. Two parallel children / two parents / two backends: independent buckets.
14. A-B-A tool count replay → no double count.
15. Late old terminal after new running → phase stays sticky-active.
16. Badge + lifecycle both coerced on latest sticky-active card.
17. Overlay latest grouping + inline multi-card isolation.
18. Store: two subscribers, unmount/remount, backend reset, StrictMode-safe
    observe.
19. No assertion that sticky causes parent turn start (display-only).
20. Rust: `CONTINUATION_CHECKPOINT_MS` remains `600_000` (existing test; do not
    add frontend checkpoint).

## Acceptance criteria

- While sticky-active on the **latest** card for a unit, operational line
  matches `生成中 | time | N tools` pattern in zh-CN (and locale equivalents).
- Historical sibling cards for earlier runs on the same unit do not re-enter
  generating chrome when a later continue starts.
- Checkpoint / continue / recovery-owned orchestration cancel re-entry does not
  flash terminal failure styling or zero tools on the latest card.
- User Stop (`parent_canceled`) and explicit cancel do not leave generating
  chrome.
- Subagent completion before checkpoint still wakes parent immediately
  (existing backend; regression-safe).
- Delegated child UIs do not present `*Conversation interrupted*` as assistant
  conclusion text (presentation suppress).
- Standalone Codex interrupt text unchanged; footer/outcome interrupt chrome
  unchanged.
- No Broker Join ownership or Detach changes in this slice.
- Sticky buckets isolated across parent conversations and backends.

## Implementation notes

- Prefer pure sticky helpers + unit tests before wiring React store.
- Reuse `Folder.chat.liveTurnStats.streaming` / `toolUseCount` / elapsed
  formatters.
- Preserve 2026-07-19 task_id guards on live event application; fold only after
  identity match into sticky key; unit phase follows newest admitted run.
- Stage only task-owned paths; never `git add -A`.
- `docs/superpowers/**` may require `git add -f` when ignored.
