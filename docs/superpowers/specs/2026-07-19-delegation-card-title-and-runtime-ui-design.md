# Delegation Card Title and Runtime UI Design

## Status

Reworked after Codex design review of the first draft (session
`codeg://session/388`, parent `codeg://session/386`). Previous “approved”
status is **superseded** by this revision until the user re-approves.

This specification **completes the frontend Delegation Card gap** for runtime
statistics already specified and largely implemented on the backend in:

- `docs/superpowers/specs/2026-07-17-event-driven-delegation-join-design.md`
  (Delegation Card UX, runtime projection, wire events)

and **adds** conversation-title display (priority **A**) on the same cards.

Backend projection, events, snapshot fields, and durable rollup columns from
the 2026-07-17 design remain authoritative for algorithms. This document
defines the **frontend projection, recovery precedence, wire type mirror,
title cache, attention chrome, and compact + expanded operational UX** that
the first draft under-specified.

## Problem

Users expect the parent conversation’s delegation UI to show:

1. A readable **title** for each Codeg child (seed or AI auto-title), not only
   the raw `task` prompt text.
2. **Elapsed runtime**, **tool-call count**, and **compact edit rollup** while
   a child runs and after it finishes.
3. A distinct **“waiting for parent decision”** attention state when the child
   asks the parent model (July 17).
4. **Expanded** retained paths / outside-workspace markers / line details when
   the user expands the operational surface (July 17).

Today:

- Child `conversation.title` is written (seed at create; AI finalize later)
  and appears in the sidebar sub-session tree, but **not** on
  `DelegatedSubThread` or `SubAgentOverlay`.
- Backend emits `DelegationStarted` / `DelegationRuntimeStatsChanged` /
  `DelegationAttentionChanged` / `DelegationCompleted` with `runtime_stats`,
  persists `delegation_runtime_stats` on child summaries, injects full
  `meta["codeg.delegation"]` via `inject_delegation_meta`, and stores live
  cards in snapshot `ActiveDelegationState` with `task_id`, `started_at`,
  `runtime_stats`, and optional `attention_request`.
- Frontend TypeScript types, `DelegationBinding`, `parseDelegationMeta`,
  snapshot seed envelopes, card model, and both card surfaces **do not ingest
  or render** those fields. `parseDelegationMeta` keeps only status / ids /
  error. Snapshot re-seed drops `task_id` / `started_at` / `runtime_stats` /
  `attention_request`. Elapsed/tool UI on the main turn (`LiveTurnStats`) is
  unrelated.

Result: the designed card metrics look “broken” even though the backend is
producing them.

## Goals

- Show **title-first secondary line** on Codeg delegation cards (message
  stream + sub-agent overlay).
- Wire **live and recovered** runtime stats into the same cards per the
  2026-07-17 Delegation Card UX (elapsed, tool-call count, compact edit
  rollup when present).
- Include **attention projection + chrome** (distinct “waiting for parent
  decision” label) as part of this slice — not deferred.
- Define **one normalized card projection** with explicit source precedence
  and anti-stale rules (`task_id` guards on every task-scoped replacement).
- Recover after refresh / mid-flight attach via **live binding**, **injected
  ToolUse meta**, and **child summary** without inventing new backend events.
- Keep **one model** (`useDelegationCardModel` + binding/projection) so
  inline card and overlay never disagree.
- Localize every new user-visible string in all ten app locales.

## Non-Goals

- Changing auto-title enrollment, capture, or InternalTitle runner.
- Changing Broker Join, attention **store**, or runtime **projection**
  algorithms on the backend.
- Showing AI title inside the parent tool-call `task` argument itself.
- Native / Codex collab cards that have no Codeg child conversation.
- Fabricating zero tool counts for historical rows without stats.
- Making **synthetic** `parent_tool_use_id` bindings receive live start /
  stats / attention / completion events (backend deliberately suppresses
  those; see Limitations).
- Turning attention into a human reply control (parent model answers via
  Join; humans use the existing question tool UI).

## Selected Approach

**Frontend completion** of an already-specified backend contract, plus title
resolution from child conversation metadata. The first draft’s
“frontend-only” direction remains correct; this revision makes the recovery
and wire contracts implementable.

```text
                    ┌─────────────────────────────────────┐
                    │   Normalized DelegationCardProjection │
                    │   (task_id, timestamps, runtime_stats,│
                    │    attention, title, task text, …)    │
                    └─────────────────────────────────────┘
                                      ▲
           precedence (highest first) │
  1. matching live binding / live event (task_id match)
  2. injected parent ToolUse meta["codeg.delegation"]
  3. freshly fetched / cached child summary fields
                                      │
        Wire / snapshot / meta / summary
        │
        ├── conversation title cache (backendKey, childId → title)
        │
        ▼
useDelegationCardModel
  · displaySecondary = formatTitle(title) || task
  · elapsedMs, toolCallCount, edit rollup, attention chrome
  · expanded detail model (paths / markers / truncation)
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
| Claim “runtime only” while saying we complete July 17 frontend gap | Contradicts attention + expanded detail requirements already on the wire |

## Scope relative to 2026-07-17

This document **implements** the following July 17 card requirements rather
than superseding them:

| July 17 requirement | This slice |
|---------------------|------------|
| Compact operational line (elapsed \| tools \| edit rollup) | Yes |
| Shared 1s ticker for running cards | Yes |
| Distinct attention state / label | Yes |
| Compact shows counts only | Yes |
| Expanded: paths, outside-workspace, line details, truncation | Yes — host surface defined below |
| Soft-watchdog `stalled` / `waiting_input` | Keep existing badges; not reimplemented |

If a future product decision wants to drop expanded detail or attention, that
must be an explicit amendment to July 17 — not silent omission here.

## UX

### Surfaces

| Surface | In scope |
|---------|----------|
| Message stream `DelegatedSubThread` | Yes |
| `SubAgentOverlay` Codeg rows (same model) | Yes |
| Overlay **native** activity rows | No (no Codeg title/stats model) |
| Overlay **Codeg activity-only fallback** rows that do not use `useDelegationCardModel` | Out of scope for title/stats; leave as today |
| Sidebar sub-session rows | Already show title; no change required |
| Parent tool-call raw task in other UIs | Unchanged |

### Secondary line (title priority A)

```text
displaySecondary =
  formatConversationTitle(child.title)?.trim() || task || null
```

- When non-null, render as the existing muted single-line (clamp / truncate)
  secondary text.
- `title` attribute / tooltip on the secondary line: prefer full title, else
  full task.
- While only seed title exists (task-derived), showing seed is correct; when
  AI auto-title finalizes, the secondary line updates via conversation
  upsert / summary refresh.
- If title and task are identical after formatting, still show once (no
  special “hide duplicate” rule).
- Title alone must **not** force `hasModel` true for an otherwise empty
  inline card; identity still comes from agent/task/binding/meta.

### Primary line

Unchanged in structure: agent/profile label, short task id (or tool id
fallback), status badge.

**Attention chrome:** when projection has open `attention_request`, show a
distinct badge/label (localized “Waiting for parent decision”) and treat it
as non-terminal status chrome. Do **not** reuse permission or
`waiting_input` styling as if they were parent-decision requests.

### Operational line (runtime) — layout by surface

Always a **separate second operational line** under the secondary title/task
line (never “beside status” — that was ambiguous across surfaces).

```text
{elapsed} | {N tool uses} | {optional edit rollup segment}
```

| Surface | Placement |
|---------|-----------|
| `DelegatedSubThread` | Full-width line under secondary text, above action row; truncate with ellipsis; full text in tooltip on the operational line container |
| `SubAgentOverlayRow` | Same stacked structure; narrower width → prefer wrapping at `|` boundaries only if single-line would clip all segments; otherwise single-line truncate with tooltip |

- Icons: reuse existing live-turn / tool-use icon patterns; no new glyph set.
- Accessible name: include agent label + secondary + operational segments +
  attention label when present.

### Elapsed time

- **Running:** `elapsedMs = now - startedAt` via shared ticker (1s).
- **Terminal:** prefer `finishedAt - startedAt` when both parse as valid
  timestamps; else fall back to `completedDurationMs` retained from an OK
  `DelegationResultSummary.duration_ms` / result duration when present; else
  omit elapsed.
- Invalid / unparsable timestamps → treat that anchor as missing (omit or
  fall through); never render `NaN` / negative durations (clamp negative to
  omit).
- **Clock skew:** running elapsed uses the client clock against server
  `started_at` strings. Remote-server mode may show slight skew; acceptable
  for V1 (no NTP correction). Document only — no special remote path.

### Tool count

- Source: `runtime_stats.tool_call_count`.
- Omit the segment when `runtime_stats` is absent (pre-feature / missing
  data). Do **not** show `0` for missing stats.
- Show `0` only when a stats object is present and the count is zero after a
  real start.

### Edit rollup (compact segment) — exact rules

`touched_files` is **always an array** (often `[]`). Do not treat “array
present” as “rollup present”.

| Condition | Compact segment |
|-----------|-----------------|
| `touched_files.length > 0` | File count = unique retained length; render as `200+` when `touched_files_truncated` is true; otherwise exact count (localized “N files” / July 17 wording) |
| else if `edit_tool_call_count > 0` | Detected edit-call count only (do **not** claim a file count) |
| else | **Omit** the edit segment entirely |
| Line totals | Append `+add -del` only when `line_counts_complete` and both `additions` / `deletions` are non-null numbers; never render `+0 -0` as a stand-in for unknown |

### Expanded operational detail (July 17)

Compact card remains count-only. Expansion host:

| Surface | Expanded host |
|---------|----------------|
| Message stream | Expand/collapse control on the operational line (card-local). Expanded body lists retained paths. |
| Overlay | Same expand control on the row; keep expanded state local to the row while mounted. |

Expanded body shows:

- Retained paths from `runtime_stats.touched_files`
- Outside-workspace marker per file (`outside_workspace`)
- Per-file additions/deletions when present
- Truncation notice when `touched_files_truncated`
- Empty expanded body if no paths (do not open empty expand solely for
  edit-call counts without paths)

`SubAgentSessionDialog` remains the **child transcript** host; it is **not**
required to host expanded file lists in V1 (card expansion is sufficient).

### Shared ticker

- One **external-store or context** ticker (StrictMode-safe reference
  counting).
- Cards **register** while mounted **and** projection status is running
  **and** `startedAt` is valid.
- Unmount (including collapsed overlay that unmounts rows) unregisters;
  duplicate inline + overlay projections for the same card may both register
  — ref-count handles double subscription without double intervals.
- Interval runs only while ref-count > 0; tick forces recompute of
  `elapsedMs` for subscribers.
- Terminal / non-running cards never hold a registration.
- Prefer “mounted + running” over “visible only” so layout thrashing does
  not thrash the interval; overlay collapse unmounts rows so those stop
  ticking.

## Data contracts (exact frontend mirror)

Mirror Rust shapes already on the wire. Serde names are **snake_case** on
JSON / `EventEnvelope`.

### `DelegationTouchedFile`

```ts
interface DelegationTouchedFile {
  path: string
  outside_workspace: boolean
  additions?: number | null
  deletions?: number | null
}
```

### `DelegationRuntimeStats`

```ts
interface DelegationRuntimeStats {
  started_at: string // ISO datetime
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

### `AttentionRequestSummary`

```ts
interface AttentionRequestSummary {
  request_id: string
  task_id: string
  message: string
  created_at: string // ISO datetime
}
```

### AcpEvent / snapshot / summary alignment

| Wire / shape | Required fields for this feature |
|--------------|----------------------------------|
| `delegation_started` | `parent_connection_id`, `parent_tool_use_id`, `child_connection_id`, `child_conversation_id`, `agent_type`, **`task_id`**, **`started_at`**, **`runtime_stats`**, optional **`attention_request`**, existing observation fields when present |
| `delegation_runtime_stats_changed` | **`parent_tool_use_id`**, **`task_id`**, **`runtime_stats`** (new TS variant) |
| `delegation_attention_changed` | **`parent_tool_use_id`**, **`task_id`**, optional **`attention_request`** (`None` / missing means **clear**, not preserve) |
| `delegation_completed` | existing ids + `agent_type` + **`task_id`** + **`runtime_stats`** + `result` (retain `result.duration_ms` when present) |
| `ActiveDelegationState` | `task_id`, `started_at`, `runtime_stats`, optional `attention_request`, observation fields |
| `DbConversationSummary` | `delegation_runtime_stats?`, `delegation_started_at?`, `delegation_finished_at?`, `delegation_attention_request?`, `auto_title_finalized`, existing title fields |

### Raw ACP reducer

In `acp-connections-context` (or equivalent event switch), list the new
operational events as **known no-store mutations** that still fan out to
subscribers. Unknown-event warnings for these variants are **bugs**.

### `parseDelegationMeta` / ToolUse meta

`meta["codeg.delegation"]` is a full `DelegationMetaSnapshot` on the wire:

- `status`, `task_id`, `child_connection_id?`, `child_conversation_id`,
  `error_code?`, `text_preview?`, `started_at`, `finished_at?`,
  `runtime_stats?`, `attention_request?`

**Design requirement:** `parseDelegationMeta` (or a successor) must forward
**every projection field**, not only status/ids/error.

### Snapshot seed

`buildDelegationSeedEnvelopes` must forward `task_id`, `started_at`,
`runtime_stats`, and `attention_request` from `ActiveDelegationState` onto
synthetic `delegation_started` envelopes so reattach matches live start.

## Normalized projection and source precedence

### Projection fields (conceptual)

```ts
interface DelegationCardProjection {
  parentToolUseId: string
  taskId: string | null
  childConversationId: number | null
  childConnectionId: string | null
  agentType: AgentType | null
  status: DelegationStatus // running | ok | err
  errorCode: string | null
  startedAt: string | null
  finishedAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  attentionRequest: AttentionRequestSummary | null
  completedDurationMs: number | null // from OK result; terminal fallback
  conversationTitle: string | null
  task: string | null
  observation?: TaskObservation | null
  // …
}
```

### Precedence (highest wins)

When merging sources for the same card identity (`parent_tool_use_id`, or
recovered completed card keyed by tool-use id / child id):

1. **Live binding / live event** with matching `task_id` (or first writer that
   establishes `task_id` for that tool-use id).
2. **Injected parent ToolUse** `meta["codeg.delegation"]` (cold completed or
   running meta on the parent transcript).
3. **Child summary** fields (`delegation_runtime_stats`, timestamps,
   `delegation_attention_request`, title) fetched or cached for
   `childConversationId`.

Rules:

- Never let a **running** cached summary override newer **terminal** live or
  meta data.
- Never let an older fetch overwrite a newer upsert (generation / sequence
  guard on title and summary caches).
- Pre-feature rows with null stats → omit stats UI; no fabricated zeros.

### `DelegationBinding` extensions

```ts
interface DelegationBinding {
  // existing…
  taskId: string
  startedAt: string
  runtimeStats: DelegationRuntimeStats
  attentionRequest?: AttentionRequestSummary | null
  finishedAt?: string | null
  completedDurationMs?: number | null
}
```

### Task-id guards (all task-scoped replacements)

Store `taskId` on the binding. For each of:

- `delegation_runtime_stats_changed`
- `delegation_attention_changed`
- `delegation_observation_changed`
- `delegation_completed`

apply only when `event.task_id === binding.taskId`. Mismatches are ignored
(stale previous task on the same `parent_tool_use_id`).

Exceptions:

- `delegation_started` always installs/replaces the binding for that
  `parent_tool_use_id` (new task wins).
- `delegation_completed` may **synthesize** a terminal binding when **no**
  binding exists (mid-flight mount / reconnect), using event fields including
  `task_id` and `runtime_stats`. If a binding exists with a different
  `task_id`, ignore the completion.

## Title resolution

### Source priority

1. Title cache entry for `(backendCacheKey, childConversationId)`.
2. One-shot fetch of child conversation projection on cache miss (see fetch
  policy).
3. Fallback: `task` from tool input.

Do **not** depend on sidebar expansion or open child tabs.

### Cache identity and lifecycle

| Rule | Detail |
|------|--------|
| Key | `(getActiveBackendCacheKey(), childConversationId)` — numeric id alone is **unsafe** across backends |
| Ensure | When `childConversationId` becomes known (binding / meta / tool output) |
| Update | On `conversation://changed` upsert for that id → apply title if generation ≥ entry |
| Delete | On deleted conversation event → tombstone id, drop entry, cancel in-flight |
| In-flight | Deduplicate per key; always clear in-flight map entry in `finally` |
| Race | Each ensure increments generation; ignore fetch results with generation &lt; current or after tombstone |
| Reconnect | After WebSocket / transport reconnect, refetch **tracked** ids still referenced by mounted cards or live bindings (same pattern as workspace conversation cache) |
| Eviction | LRU or ref-count by mounted card interest; bound memory; never retain forever for every historical child |
| Format | `formatConversationTitle` before display |

### Fetch policy

- Prefer the lightest available API that returns title + durable delegation
  fields. If only `getFolderConversation` exists today, reuse **one** fetch
  result for both title and cold runtime/attention hydrate; cap concurrent
  fetches; allow a small number of transient retries.
- Runtime stats writes do **not** always broadcast conversation upserts —
  live stats must continue to come from events / meta, not from polling the
  summary.
- Never block card render on title fetch.

## Model API (`useDelegationCardModel`)

```ts
interface DelegationCardModel {
  // existing fields…
  displaySecondary: string | null
  task: string | null
  conversationTitle: string | null
  startedAt: string | null
  finishedAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  attentionRequest: AttentionRequestSummary | null
  completedDurationMs: number | null
  elapsedMs: number | null
  toolCallCount: number | null
  // compact + expanded edit helpers as needed
}
```

`hasModel` unchanged in spirit: requires useful agent/task/binding/meta
identity; absence of title/stats must not hide a card that previously showed.
Title alone does not create a card.

## Limitations (synthetic parent_tool_use_id)

When the broker uses a **synthetic** `parent_tool_use_id`, it suppresses live
emit of start / runtime stats / attention / completion / observation and live
meta writes for that id (`is_synthetic_parent_tool_use_id` /
`emit_runtime_stats_changed_if_real`).

| Mode | Behavior |
|------|----------|
| Live updates | **Not guaranteed** for synthetic bindings; frontend-only wiring cannot fix this |
| Cold recovery | May still reconstruct from durable child row / inject paths that use real task identity later |
| Success criteria | Live success criteria apply only to **real** tool-use bindings |

Covering synthetic cards with live ticks requires backend work — out of scope.

## Error / missing-data behavior

| Situation | Behavior |
|-----------|----------|
| No child id yet | Secondary = task; no stats line |
| Child id, title fetch pending | Secondary = task until title arrives |
| Title fetch fails | Keep task; do not error the card |
| Stats event before binding | Ignore (or buffer only if product already buffers observations similarly) |
| Stale `task_id` on any task-scoped event | Ignore |
| Pre-feature completed child without stats | No fabricated zeros; task/title only |
| Synthetic tool-use id | Cold-only / limited live; see Limitations |
| Native / activity-only overlay row | Unchanged |

## Testing

### Unit

- `displaySecondary` priority (title / empty title / task fallback /
  formatConversationTitle).
- Event handlers: replace stats/attention on matching `task_id`; ignore
  mismatch for stats, attention, observation, completion.
- Completion synthesizes terminal binding only when none exists; does not
  clobber different `task_id`.
- Elapsed: running formula; terminal `finished - started`;
  `completedDurationMs` fallback; invalid dates omit.
- Edit rollup: paths + truncation `200+`; edit calls without paths; omit when
  zero edits; line totals only when complete.
- `parseDelegationMeta` forwards full projection fields.
- Snapshot seed envelopes forward `task_id` / `started_at` / `runtime_stats` /
  `attention_request`.
- Title cache: backend key scoping, fetch-after-delete, generation race,
  reconnect refetch of tracked ids.

### Component

- `DelegatedSubThread` / overlay row: secondary title, operational line,
  attention badge, expand paths.
- Regression: card still renders with only input task and no binding;
  `hasModel` false path unchanged.
- Raw reducer: operational events do not log as unknown.

### Out of scope for this design

- New Rust tests unless a wire serde name mismatch is discovered; fix types to
  match existing Rust.

## Implementation outline

1. Align TS types with Rust (`DelegationTouchedFile`, `DelegationRuntimeStats`,
   `AttentionRequestSummary`, all event variants, snapshot, summary including
   `auto_title_finalized` + `delegation_attention_request`).
2. Raw ACP reducer known-event cases for new operational variants.
3. Extend `parseDelegationMeta` + `buildDelegationSeedEnvelopes` to full
   projection.
4. Extend `DelegationBinding` + `delegation-context` handlers with task-id
   guards, duration retention, attention, runtime stats.
5. Normalized projection merge (live > meta > summary) used by the card model.
6. Title cache: backend-scoped key, generation/tombstone, reconnect, bounded
   eviction.
7. Extend `useDelegationCardModel` + shared ticker (ref-counted external
   store).
8. UI: secondary title, operational line layout, attention chrome, expanded
   paths, i18n (10 locales).
9. Tests above.

## Out of scope follow-ups (optional later)

- Surfacing title in background-task chip or pet panel.
- Backend live events for synthetic parent_tool_use_id.
- NTP / clock-skew compensation for remote servers.
- Hosting expanded file list inside `SubAgentSessionDialog`.

## Success criteria

- With auto-title on, after a child finishes a usable first turn and title
  finalizes, both the message-stream card and overlay row show the new title
  without refresh.
- While a Codeg child is running on a **real** `parent_tool_use_id` and the
  backend projects tool calls, both surfaces show increasing tool-call count
  and ticking elapsed time.
- After completion, elapsed freezes at terminal duration and tool count
  remains; attention chrome clears when the backend clears attention.
- Open parent-decision attention shows a distinct waiting label on both
  surfaces.
- Expanded surface shows retained paths, outside-workspace markers, and
  truncation notice when paths exist.
- After full page reload on a finished **real** child with durable meta or
  summary stats, card still shows title and stats without live events.
- Stale `task_id` events never clobber a newer binding on the same tool-use
  id.
- Main turn `LiveTurnStats` behavior unchanged.

## Review history

- **2026-07-19 first draft:** conversation approval of title priority A +
  runtime wiring; later Codex review (`codeg://session/388`) verdict
  **Major rework**.
- **This revision:** incorporates Codex must-fix / should-fix items:
  recovery precedence, full wire types, task-id guards, duration retention,
  attention + expanded detail in-scope, title cache identity/reconnect/races,
  synthetic fallback limitation, edit-rollup/layout/ticker rules, expanded
  test matrix.
