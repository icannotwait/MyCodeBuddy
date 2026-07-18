# Delegation Card Title and Runtime UI Design

## Status

Approved in conversation on 2026-07-19 (title display preference **A**;
combine with runtime stats frontend wiring).

This specification **completes the frontend Delegation Card gap** for runtime
statistics already specified and largely implemented on the backend in:

- `docs/superpowers/specs/2026-07-17-event-driven-delegation-join-design.md`
  (Delegation Card UX, runtime projection, wire events)

and **adds** conversation-title display (priority A) on the same cards.

Backend projection, events, snapshot fields, and durable rollup columns from
that design remain authoritative. This document does **not** re-open Broker
Join, attention, or projection algorithms.

## Problem

Users expect the parent conversation’s delegation UI to show:

1. A readable **title** for each Codeg child (seed or AI auto-title), not only
   the raw `task` prompt text.
2. **Elapsed runtime** and **tool-call count** (and compact edit rollup when
   available) while a child runs and after it finishes.

Today:

- Child `conversation.title` is written (seed at create; AI finalize later)
  and appears in the sidebar sub-session tree, but **not** on
  `DelegatedSubThread` or `SubAgentOverlay`.
- Backend emits `DelegationStarted` / `DelegationRuntimeStatsChanged` /
  `DelegationCompleted` with `runtime_stats`, persists
  `delegation_runtime_stats` on child summaries, and stores live cards in
  snapshot `ActiveDelegationState` with `started_at` + `runtime_stats`.
- Frontend TypeScript types, `DelegationBinding`, card model, and both card
  surfaces **do not ingest or render** those fields. Elapsed/tool UI that
  exists on the main turn (`LiveTurnStats`) is unrelated.

Result: the designed card metrics look “broken” even though the backend is
producing them.

## Goals

- Show **title-first secondary line** on Codeg delegation cards (message
  stream + sub-agent overlay).
- Wire **live and recovered** runtime stats into the same cards per the
  2026-07-17 Delegation Card UX (elapsed, tool-call count, compact edit
  rollup when present).
- Keep **one model** (`useDelegationCardModel` + binding) so inline card and
  overlay never disagree.
- Recover after refresh / mid-flight attach via snapshot and durable summary
  without inventing new backend events.
- Localize every new user-visible string in all ten app locales.

## Non-Goals

- Changing auto-title enrollment, capture, or InternalTitle runner.
- Changing Broker Join, attention, or runtime **projection** algorithms.
- Showing AI title inside the parent tool-call `task` argument itself.
- Native / Codex collab cards that have no Codeg child conversation.
- Per-card timers (prefer one shared ticker for running cards).
- Fabricating zero tool counts for historical rows without stats.
- Expanding the “expanded card file list” UX beyond what the compact line
  requires in this slice (full path list can follow later if not already
  present; compact line is in scope).

## Selected Approach

**Frontend-only completion** of an already-specified backend contract, plus
title resolution from child conversation metadata.

```text
Wire / snapshot / summary
        │
        ▼
DelegationBinding (+ runtime_stats, started_at, task_id)
        │
        ├── conversation title cache (child id → title)
        │
        ▼
useDelegationCardModel
  · displaySecondary = formatTitle(title) || task
  · elapsedMs, toolCallCount, edit rollup fields
        │
        ├── DelegatedSubThread (message stream)
        └── SubAgentOverlayRow (conversation sub-agent list)
```

Rejected alternatives:

| Approach | Why not |
|----------|---------|
| Only read sidebar `childrenByParent` / tab `childSummaries` | Often empty when parent card is visible but subtree not expanded / child tab not open |
| Put title only into MCP tool output | Requires backend rewrite; title often arrives **after** first tool ack |
| Re-derive tool counts by scanning child live transcript in the card | Duplicates Broker projection; breaks after detach; contradicts 2026-07-17 |

## UX

### Surfaces

| Surface | In scope |
|---------|----------|
| Message stream `DelegatedSubThread` | Yes |
| `SubAgentOverlay` Codeg rows (same model) | Yes |
| Overlay native activity rows | No (no Codeg title/stats) |
| Sidebar sub-session rows | Already show title; no change required for this feature |
| Parent tool-call raw task in other UIs | Unchanged |

### Secondary line (title priority A)

```text
displaySecondary =
  formatConversationTitle(child.title)?.trim() || task || null
```

- When non-null, render as the existing muted single-line (clamp / truncate)
  secondary text.
- `title` attribute / tooltip: prefer full title, else full task.
- While only seed title exists (task-derived), showing seed is correct; when
  AI auto-title finalizes, the secondary line updates via conversation
  upsert / summary refresh.
- If title and task are identical after formatting, still show once (no
  special “hide duplicate” rule).

Primary line stays: agent/profile label, short task id, status badge.

### Operational line (runtime)

Add one compact operational line under the secondary line (or beside status
when space is tight), following 2026-07-17:

```text
{elapsed} | {N tool uses} | {optional edit rollup}
```

Requirements:

- **Running:** `elapsed = now - started_at` (local ticker, 1s).
- **Terminal:** `elapsed = finished_at - started_at` when both present;
  else fall back to `result.duration_ms` on ok completion when available;
  else omit elapsed if no anchors.
- **Tool count:** from `runtime_stats.tool_call_count`; omit when stats
  absent (do not show `0` for pre-feature / missing data). Show `0` only
  when stats object is present and count is zero after a real start.
- **Edit rollup (compact):** when `edit_tool_call_count > 0` or touched-file
  rollup present, show the compact form from 2026-07-17 (detected edits /
  file count / `+add -del` when complete). Omit rather than invent zeros.
- Reuse `formatElapsedLabel` and existing `toolUseCount` i18n patterns where
  possible (e.g. `Folder.chat.liveTurnStats` units or delegation-scoped
  mirrors) so timers match the main turn chrome.

## Data Contracts (Frontend Alignment)

Mirror Rust shapes already on the wire. Exact serde names follow backend.

### `DelegationRuntimeStats` (TS)

```ts
interface DelegationRuntimeStats {
  started_at: string
  finished_at?: string | null
  tool_call_count: number
  edit_tool_call_count: number
  touched_files: DelegationTouchedFile[]
  touched_files_truncated: boolean
  additions?: number | null
  deletions?: number | null
  line_counts_complete: boolean
}
```

### AcpEvent gaps to close

| Event | Add / introduce |
|-------|-----------------|
| `delegation_started` | `task_id`, `started_at`, `runtime_stats`, optional `attention_request` (if already on wire) |
| `delegation_runtime_stats_changed` | **new** variant: `parent_tool_use_id`, `task_id`, `runtime_stats` |
| `delegation_completed` | `task_id`, `runtime_stats` (keep existing `result`) |
| `ActiveDelegationState` | `task_id`, `started_at`, `runtime_stats`, optional attention |
| `DbConversationSummary` | optional `delegation_runtime_stats` for cold recovery |

Handler rules (replace, do not accumulate):

1. `delegation_started` → set binding runtime fields from event.
2. `delegation_runtime_stats_changed` → replace `runtime_stats` only when
   `task_id` matches the live card (ignore stale).
3. `delegation_completed` → set terminal status + final `runtime_stats`.
4. Snapshot `active_delegations` seed → same fields as started.
5. Cold / completed cards without live binding → optional hydrate from
   child summary `delegation_runtime_stats` + `delegation_started_at` /
   `finished_at` when resolving by `childConversationId`.

## Title Resolution

### Source priority

1. Child title cache keyed by `childConversationId` (from
   `conversation://changed` upserts and explicit fetches).
2. Optional: summary returned by a one-shot `getFolderConversation(id)` when
   the id is known and cache miss.
3. Fallback: `task` from tool input (existing parse).

Do **not** depend on sidebar expansion or open child tabs.

### Cache behavior

- Ensure entry when `childConversationId` becomes known (binding / meta /
  tool output).
- Deduplicate in-flight fetches per id.
- On `conversation://changed` upsert for that id, update cached title
  (auto-title finalize, seed, rename).
- On deleted, drop cache entry.
- Format through `formatConversationTitle` before display.

A small dedicated module/store is preferred over overloading tab-only
`childSummaries` (which prunes closed tabs). Sharing read helpers is fine;
lifecycle must outlive “child tab open”.

## Model API (`useDelegationCardModel`)

Extend the model (names illustrative):

```ts
interface DelegationCardModel {
  // existing fields…
  /** Title-first secondary line; null if nothing to show. */
  displaySecondary: string | null
  /** Raw task text (still available for dialogs / kickoff). */
  task: string | null
  conversationTitle: string | null
  startedAt: string | null
  finishedAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  /** Derived for UI; null when cannot compute. */
  elapsedMs: number | null
  toolCallCount: number | null
  // compact edit fields as needed by the operational line
}
```

`hasModel` unchanged in spirit: still requires useful agent/task/binding
identity; absence of title/stats must not hide a card that previously showed.

## Shared ticker

- One module-level or context-level 1s interval while **any** mounted card
  reports running status with a `startedAt`.
- Cards subscribe to “tick” and recompute `elapsedMs`.
- No interval when zero running visible cards.

## Error / missing-data behavior

| Situation | Behavior |
|-----------|----------|
| No child id yet | Secondary = task; no stats line |
| Child id, title fetch pending | Secondary = task until title arrives |
| Title fetch fails | Keep task; do not error the card |
| Stats event before binding | Ignore until binding exists (or buffer only if already done for observation) |
| Stale `task_id` on stats event | Ignore |
| Pre-feature completed child without stats | No fabricated zeros; task/title only |
| Native overlay row | Unchanged |

## Testing

- Unit: `displaySecondary` priority (title / empty title / task fallback /
  formatConversationTitle).
- Unit: event handlers replace stats on matching `task_id`, ignore mismatch.
- Unit: elapsed running vs terminal formula.
- Component: `DelegatedSubThread` / overlay row render secondary title and
  operational line from model fixtures.
- Regression: card still renders with only input task and no binding;
  `hasModel` false path for inline card unchanged.
- No requirement for new Rust tests unless a wire serde name mismatch is
  discovered; fix types to match existing Rust.

## Implementation outline

1. Align TS types with Rust (`DelegationRuntimeStats`, event variants,
   snapshot, summary).
2. Extend `DelegationBinding` + `delegation-context` handlers + snapshot
   rehydrate.
3. Child title cache + ensure-on-id + conversation change subscription.
4. Extend `useDelegationCardModel` with title + runtime derived fields.
5. Shared running-card ticker.
6. Update `DelegatedSubThread` and `SubAgentOverlayRow` UI + i18n (10 locales).
7. Tests above.

## Out of scope follow-ups (optional later)

- Full expanded path list for touched files on the card.
- Attention request chrome beyond existing waiting/permission badges.
- Surfacing title in background-task chip or pet panel.

## Success criteria

- With auto-title on, after a child finishes a usable first turn and title
  finalizes, both the message-stream card and overlay row show the new title
  without refresh.
- While a Codeg child is running and the backend projects tool calls, both
  surfaces show increasing tool-call count and ticking elapsed time.
- After completion, elapsed freezes at terminal duration and tool count
  remains.
- After full page reload on a finished child with durable stats, card still
  shows title (and stats when summary carries them) without live events.
- Main turn `LiveTurnStats` behavior unchanged.
