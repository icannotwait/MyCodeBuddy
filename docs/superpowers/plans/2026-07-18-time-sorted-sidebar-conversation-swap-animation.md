# Time-Sorted Sidebar Conversation Swap Animation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every sidebar conversation bucket use the card's effective activity time, optimistically promote real user dispatches, and animate painted same-bucket root-subtree reorders without disturbing `virtua` or the reader's scroll position.

**Architecture:** Backend-owned `DbConversationSummary.updated_at` remains authoritative. A Zustand optimistic overlay supplies a temporary effective timestamp at the actual ACP dispatch boundary; grouping and labels consume the same resolver, while backend state patches reconcile the overlay monotonically. A sidebar-local WAAPI controller classifies pure root-block permutations, anchors the viewport, and animates only stable inner wrappers rendered by `virtua`.

**Tech Stack:** Next.js 16 static export, React 19, TypeScript strict mode, Zustand 5, Virtua 0.48, OverlayScrollbars 2.15, Web Animations API, Vitest 2, Testing Library, pnpm 11.

## Global Constraints

- Keep `next.config.ts` on `output: "export"`; add no dynamic routes or server-only frontend APIs.
- Do not change Rust, SQLite schema, backend `updated_at` semantics, ACP wire types, Tauri commands, or Axum handlers.
- Do not add, remove, or upgrade dependencies. Use the browser Web Animations API already available in the supported WebViews.
- Keep `DbConversationSummary.updated_at` backend-owned. No frontend path may write `new Date()` into that field after this plan.
- Keep `virtua`; never set its `shift` prop and never transform its absolute-positioned outer item.
- Keep delegation children ordered by `created_at DESC, id DESC`; child activity never promotes a root.
- Buckets are exactly `pinned`, `chat`, and `folder:<display-folder-id>`. Worktree roots use the mapped parent display folder.
- Regular/chat ordering is effective activity DESC, `created_at` DESC, `id` DESC. Pinned ordering is effective activity DESC, `pinned_at` DESC, `id` DESC.
- Only actual root prompt dispatch and root structured-question answer begin optimistic activity. Queue insertion, stream chunks, permission responses, pinning, viewing, and child answers do not.
- Assistant start/completion use existing `conversation://changed` state patches only; add no client assistant bump.
- Root/subtree movement lasts exactly 230 ms with `cubic-bezier(0.2, 0, 0, 1)`. A newly mounted offscreen promotion fades for exactly 120 ms with the same easing.
- With `prefers-reduced-motion: reduce`, preserve ordering and anchoring but create no transform or fade animation.
- Do not delete the existing ten-locale `sortBy*` strings in this change; they become unused.
- Do not add a browser-test framework. Cover pure logic and DOM orchestration in Vitest, then run the approved desktop and server-browser manual smoke.
- Every production behavior starts with a focused failing test and confirmed RED output. Every task ends with focused GREEN verification and a scoped commit.
- Preserve all unrelated dirty worktree changes. Stage only the exact paths named by the current task; use `git add -f` for ignored `docs/superpowers` files only.

---

## File Map

### Activity Authority And Reconciliation

- `src/lib/conversation-activity.ts`: owns optimistic activity types, timestamp parsing, monotonic optimistic timestamp creation, and effective-time resolution.
- `src/lib/conversation-activity.test.ts`: verifies invalid timestamps, effective-time selection, and monotonic same-millisecond dispatches.
- `src/stores/app-workspace-store.ts`: owns optimistic entries, token rollback, activity signals, monotonic state/upsert merge, refresh request ordering, and reset state.
- `src/stores/app-workspace-store.test.ts`: verifies backend authority, token races, stale state/upserts, and stale refreshes.

### Ordering And Sidebar Presentation

- `src/components/conversations/sidebar-conversation-grouping.ts`: removes created-mode root sorting, consumes effective activity, assigns root/bucket metadata, and exports stable sidebar row keys.
- `src/components/conversations/sidebar-conversation-grouping.test.ts`: verifies regular, chat, pinned, worktree, and delegation order plus root-block metadata.
- `src/lib/sidebar-view-mode-storage.ts`: removes the inert sort-mode type and load/save functions while leaving other view preferences unchanged.
- `src/components/layout/sidebar.tsx`: removes sort hydration, menu controls, and `sortMode` propagation.
- `src/components/layout/sidebar.test.tsx`: proves the view menu no longer offers a sort choice.
- `src/components/conversations/sidebar-conversation-list.tsx`: subscribes to optimistic activity, uses effective time for grouping and labels, and later integrates the animation hook.
- `src/components/conversations/sidebar-conversation-list.test.tsx`: verifies updated-only DOM order, immediate optimistic promotion/label, wrapper metadata, and existing memo/sticky/drag behavior.

### Actual Dispatch Triggers

- `src/contexts/acp-connections-context.tsx`: begins/rolls back root activity around `acpPrompt` and `acpAnswerQuestion` at the innermost dispatch boundary.
- `src/contexts/acp-connections-context.test.tsx`: verifies success retention, busy/error rollback, bound-id fallback, root question answers, and child exclusion.
- `src/hooks/use-connection-lifecycle.test.ts`: verifies a failed mode change never reaches the inner prompt boundary.

### Reorder Animation And Scroll Stability

- `src/components/conversations/sidebar-reorder-animation.ts`: pure root-order snapshot, eligibility, anchor selection, and FLIP delta helpers.
- `src/components/conversations/sidebar-reorder-animation.test.ts`: covers pure permutations, structural rejection, worktree bucket identity, anchors, and deltas.
- `src/components/conversations/use-sidebar-reorder-animation.ts`: owns geometry capture, WAAPI lifecycle, cancellation/rebase, reduced motion, offscreen fade, and programmatic-scroll suppression.
- `src/components/conversations/use-sidebar-reorder-animation.test.tsx`: deterministic DOM tests for move/fade timing, retarget, scroll cancellation, anchoring, and reduced motion.

---

## Cross-Task Interfaces

These names and shapes are fixed. Later tasks consume them without renaming.

```ts
// src/lib/conversation-activity.ts
export interface OptimisticConversationActivity {
  token: string
  baselineUpdatedAt: string
  effectiveAt: string
}

export type OptimisticActivityById = ReadonlyMap<
  number,
  OptimisticConversationActivity
>

export const EMPTY_OPTIMISTIC_ACTIVITY_BY_ID: OptimisticActivityById

export function parseActivityTimestamp(value: string | null | undefined): number

export function getEffectiveConversationUpdatedAt(
  summary: DbConversationSummary,
  optimisticActivityById: OptimisticActivityById
): string

export function nextOptimisticActivityTimestamp(
  baselineUpdatedAt: string,
  previousEffectiveMs: number,
  nowMs?: number
): { effectiveAt: string; effectiveMs: number }
```

```ts
// Additions to AppWorkspaceStoreState
optimisticActivityById: OptimisticActivityById
conversationActivitySequence: number
lastConversationActivityId: number | null

beginConversationActivity(id: number): string | null
rollbackConversationActivity(id: number, token: string): void
```

`conversationActivitySequence` advances only for an optimistic begin and an
accepted state patch whose authoritative timestamp advances. Rollback, refresh,
hydration, upsert, filtering, pinning, and local title/status patches do not
advance it.

```ts
// src/components/conversations/sidebar-conversation-grouping.ts
export type SidebarBucketKey =
  | "pinned"
  | "chat"
  | `folder:${number}`

export interface ConversationRow {
  kind: "conversation"
  conversation: DbConversationSummary
  depth: number
  rootId: number
  bucketKey: SidebarBucketKey
}

export interface SubsessionLoadingRow {
  kind: "subsession-loading"
  parentId: number
  depth: number
  rootId: number
  bucketKey: SidebarBucketKey
}

export function sidebarRowKey(row: SidebarRow): string

export function groupByFolderWithReuse(
  filtered: readonly DbConversationSummary[],
  prev: Map<number, DbConversationSummary[]>,
  childToParent?: ReadonlyMap<number, number>,
  optimisticActivityById?: OptimisticActivityById
): Map<number, DbConversationSummary[]>

export function selectPinnedWithReuse(
  conversations: readonly DbConversationSummary[],
  prev: DbConversationSummary[],
  optimisticActivityById?: OptimisticActivityById
): DbConversationSummary[]

export function selectChatConversationsWithReuse(
  conversations: readonly DbConversationSummary[],
  showCompleted: boolean,
  prev: DbConversationSummary[],
  optimisticActivityById?: OptimisticActivityById
): DbConversationSummary[]
```

```ts
// src/components/conversations/sidebar-reorder-animation.ts
export interface SidebarRootOrderSnapshot {
  structuralRowKeys: readonly string[]
  rootsByBucket: ReadonlyMap<SidebarBucketKey, readonly number[]>
  blockRowKeysByRoot: ReadonlyMap<number, readonly string[]>
  bucketByRoot: ReadonlyMap<number, SidebarBucketKey>
}

export interface SidebarActivityReorder {
  conversationId: number
  bucketKey: SidebarBucketKey
  previousIndex: number
  nextIndex: number
}

export interface SidebarMeasuredRow {
  key: string
  rootId: number | null
  top: number
  bottom: number
}

export function buildSidebarRootOrderSnapshot(
  rows: readonly SidebarRow[]
): SidebarRootOrderSnapshot

export function detectSidebarActivityReorder(
  before: SidebarRootOrderSnapshot,
  after: SidebarRootOrderSnapshot,
  conversationId: number
): SidebarActivityReorder | null

export function selectSidebarAnchor(
  before: ReadonlyMap<string, SidebarMeasuredRow>,
  survivingKeys: ReadonlySet<string>,
  viewportTop: number,
  viewportBottom: number,
  promotedRootId: number
): SidebarMeasuredRow | null

export function sidebarAnchorScrollDelta(
  beforeTop: number,
  afterTop: number
): number

export function sidebarFlipDeltaY(
  beforeTop: number,
  afterTop: number
): number
```

```ts
// src/components/conversations/use-sidebar-reorder-animation.ts
export interface UseSidebarReorderAnimationOptions {
  rows: readonly SidebarRow[]
  activitySequence: number
  activityConversationId: number | null
  viewportEl: HTMLElement | null
  dragging: boolean
}

export interface SidebarReorderAnimationControls {
  handleUserScroll(): void
}

export function useSidebarReorderAnimation(
  options: UseSidebarReorderAnimationOptions
): SidebarReorderAnimationControls
```

---

### Task 1: Add Effective Activity Primitives And Token-Safe Store State

**Files:**
- Create: `src/lib/conversation-activity.ts`
- Create: `src/lib/conversation-activity.test.ts`
- Modify: `src/stores/app-workspace-store.ts:1-290,460-471`
- Modify: `src/stores/app-workspace-store.test.ts:1-154`

**Interfaces:**
- Produces: every interface in `conversation-activity.ts` and the optimistic store fields/actions fixed above.
- Preserves: `DbConversationSummary.updated_at` as authoritative data; `stats` reference stability for activity-only changes.
- Consumed by: Tasks 2-5 and Task 8.

- [ ] **Step 1: Write failing activity-helper tests**

Create `src/lib/conversation-activity.test.ts` with exact boundary cases:

```ts
import { describe, expect, it } from "vitest"
import type { DbConversationSummary } from "@/lib/types"
import {
  getEffectiveConversationUpdatedAt,
  nextOptimisticActivityTimestamp,
  parseActivityTimestamp,
} from "./conversation-activity"

const summary = {
  id: 7,
  updated_at: "2026-07-18T01:00:00.000Z",
} as DbConversationSummary

describe("conversation activity timestamps", () => {
  it("parses invalid timestamps as zero", () => {
    expect(parseActivityTimestamp("bad")).toBe(0)
    expect(parseActivityTimestamp(null)).toBe(0)
  })

  it("uses the later optimistic timestamp for presentation", () => {
    const optimistic = new Map([
      [
        7,
        {
          token: "t1",
          baselineUpdatedAt: summary.updated_at,
          effectiveAt: "2026-07-18T02:00:00.000Z",
        },
      ],
    ])
    expect(getEffectiveConversationUpdatedAt(summary, optimistic)).toBe(
      "2026-07-18T02:00:00.000Z"
    )
  })

  it("keeps two same-millisecond dispatches strictly monotonic", () => {
    const first = nextOptimisticActivityTimestamp(
      summary.updated_at,
      0,
      1_752_800_000_000
    )
    const second = nextOptimisticActivityTimestamp(
      summary.updated_at,
      first.effectiveMs,
      1_752_800_000_000
    )
    expect(second.effectiveMs).toBe(first.effectiveMs + 1)
  })
})
```

- [ ] **Step 2: Write failing optimistic-store tests**

Extend `app-workspace-store.test.ts`:

```ts
it("does not invent updated_at for local title or status patches", () => {
  const store = useAppWorkspaceStore.getState()
  store.applyConversationUpsert(makeSummary({ id: 1 }))
  const baseline = store.conversations[0].updated_at

  store.updateConversationLocal(1, { title: "Renamed" })
  store.updateConversationLocal(1, { status: "pending_review" })

  expect(useAppWorkspaceStore.getState().conversations[0].updated_at).toBe(
    baseline
  )
})

it("rolls back only the matching optimistic token", () => {
  const store = useAppWorkspaceStore.getState()
  store.applyConversationUpsert(makeSummary({ id: 1 }))
  const first = store.beginConversationActivity(1)!
  const second = store.beginConversationActivity(1)!

  store.rollbackConversationActivity(1, first)
  expect(
    useAppWorkspaceStore.getState().optimisticActivityById.get(1)?.token
  ).toBe(second)

  store.rollbackConversationActivity(1, second)
  expect(
    useAppWorkspaceStore.getState().optimisticActivityById.has(1)
  ).toBe(false)
})

it("ignores older state and acknowledges activity only past its baseline", () => {
  const store = useAppWorkspaceStore.getState()
  store.applyConversationUpsert(
    makeSummary({ id: 1, updated_at: "2026-07-18T02:00:00.000Z" })
  )
  const token = store.beginConversationActivity(1)
  expect(token).not.toBeNull()
  const sequence = useAppWorkspaceStore.getState().conversationActivitySequence

  store.applyConversationStatePatch({
    id: 1,
    status: "cancelled",
    awaiting_reply_token: null,
    updated_at: "2026-07-18T01:00:00.000Z",
  })
  expect(useAppWorkspaceStore.getState().conversations[0].status).toBe(
    "in_progress"
  )
  expect(useAppWorkspaceStore.getState().optimisticActivityById.has(1)).toBe(
    true
  )

  store.applyConversationStatePatch({
    id: 1,
    status: "pending_review",
    awaiting_reply_token: "generation-1",
    updated_at: "2026-07-18T03:00:00.000Z",
  })
  const after = useAppWorkspaceStore.getState()
  expect(after.optimisticActivityById.has(1)).toBe(false)
  expect(after.conversationActivitySequence).toBe(sequence + 1)
  expect(after.lastConversationActivityId).toBe(1)
})
```

Also assert `beginConversationActivity(999) === null` and that a summary with
`parent_id: 1`, inserted directly with `useAppWorkspaceStore.setState` to bypass
the root-only upsert filter, cannot begin activity.

- [ ] **Step 3: Run the focused tests and confirm RED**

```powershell
pnpm exec vitest run src/lib/conversation-activity.test.ts src/stores/app-workspace-store.test.ts
```

Expected: FAIL because the helper module, store fields, and store actions do not
exist; the local timestamp assertion also exposes the current client clock write.

- [ ] **Step 4: Implement the shared timestamp helpers**

Create `conversation-activity.ts`:

```ts
import type { DbConversationSummary } from "@/lib/types"

export interface OptimisticConversationActivity {
  token: string
  baselineUpdatedAt: string
  effectiveAt: string
}

export type OptimisticActivityById = ReadonlyMap<
  number,
  OptimisticConversationActivity
>

export const EMPTY_OPTIMISTIC_ACTIVITY_BY_ID: OptimisticActivityById = new Map()

export function parseActivityTimestamp(
  value: string | null | undefined
): number {
  if (!value) return 0
  const parsed = Date.parse(value)
  return Number.isNaN(parsed) ? 0 : parsed
}

export function getEffectiveConversationUpdatedAt(
  summary: DbConversationSummary,
  optimisticActivityById: OptimisticActivityById
): string {
  const optimistic = optimisticActivityById.get(summary.id)?.effectiveAt
  if (!optimistic) return summary.updated_at
  return parseActivityTimestamp(optimistic) >
    parseActivityTimestamp(summary.updated_at)
    ? optimistic
    : summary.updated_at
}

export function nextOptimisticActivityTimestamp(
  baselineUpdatedAt: string,
  previousEffectiveMs: number,
  nowMs = Date.now()
): { effectiveAt: string; effectiveMs: number } {
  const effectiveMs = Math.max(
    nowMs,
    previousEffectiveMs + 1,
    parseActivityTimestamp(baselineUpdatedAt) + 1
  )
  return { effectiveAt: new Date(effectiveMs).toISOString(), effectiveMs }
}
```

- [ ] **Step 5: Implement optimistic store actions and monotonic state patches**

In `app-workspace-store.ts`, import `randomUUID`, the helper types/functions,
add the fixed state fields/actions, and keep a module-local
`lastOptimisticActivityMs`. The state-patch branch must follow this shape:

```ts
const currentMs = parseActivityTimestamp(current.updated_at)
const patchMs = parseActivityTimestamp(patch.updated_at)
if (patchMs < currentMs) return

const optimistic = get().optimisticActivityById.get(patch.id)
const clearsOptimistic =
  optimistic !== undefined &&
  patchMs > parseActivityTimestamp(optimistic.baselineUpdatedAt)
const currentOptimistic = get().optimisticActivityById
let nextOptimistic = currentOptimistic
if (clearsOptimistic) {
  const mutable = new Map(currentOptimistic)
  mutable.delete(patch.id)
  nextOptimistic = mutable
}

const authoritativeAdvanced = patchMs > currentMs
next[index] = {
  ...current,
  status: patch.status,
  awaiting_reply_token: patch.awaiting_reply_token,
  updated_at: patch.updated_at,
}

set({
  conversations: next,
  stats: get().stats,
  optimisticActivityById: nextOptimistic,
  ...(authoritativeAdvanced
    ? {
        conversationActivitySequence:
          get().conversationActivitySequence + 1,
        lastConversationActivityId: patch.id,
      }
    : {}),
})
```

`beginConversationActivity` checks for a known root, calls
`nextOptimisticActivityTimestamp`, writes a cloned map, and increments the
activity sequence. `rollbackConversationActivity` clones/deletes only on an
exact token match and never increments the sequence. Remove the
`bumpUpdatedAt/new Date()` branch from `updateConversationLocal`.

Reset `lastOptimisticActivityMs` and the new state fields in
`resetAppWorkspaceStore` so backend switches and tests cannot inherit a prior
clock/token.

- [ ] **Step 6: Run focused GREEN verification**

```powershell
pnpm exec vitest run src/lib/conversation-activity.test.ts src/stores/app-workspace-store.test.ts
pnpm exec eslint src/lib/conversation-activity.ts src/lib/conversation-activity.test.ts src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts
```

Expected: both commands exit 0; no state test observes a frontend-authored
`updated_at`.

- [ ] **Step 7: Commit activity primitives**

```powershell
git add src/lib/conversation-activity.ts src/lib/conversation-activity.test.ts src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts
git diff --cached --check
git commit -m "feat(sidebar): add optimistic conversation activity"
```

---

### Task 2: Make Upserts And Refreshes Monotonic

**Files:**
- Modify: `src/stores/app-workspace-store.ts:120-290,460-471`
- Modify: `src/stores/app-workspace-store.test.ts`

**Interfaces:**
- Consumes: `parseActivityTimestamp`, optimistic store fields, and state-tuple authority from Task 1.
- Produces: latest-request-only refresh commits and stale-summary merge behavior used by every sidebar consumer.
- Preserves: full metadata upserts even when their activity timestamp is older.

- [ ] **Step 1: Add a deterministic API mock and deferred helper**

At the top of `app-workspace-store.test.ts`, add a hoisted API fixture exporting
every name imported by the store, with `listAllConversations` controllable:

```ts
const api = vi.hoisted(() => ({
  listAllConversations: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getFolder: vi.fn(),
  listAllConversations: api.listAllConversations,
  listAllFolderDetails: vi.fn(async () => []),
  listOpenFolderDetails: vi.fn(async () => []),
  openFolder: vi.fn(),
  openFolderById: vi.fn(),
  openWorktreeFolder: vi.fn(),
  removeFolderFromWorkspace: vi.fn(),
  reorderFolders: vi.fn(),
}))

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => {
    resolve = r
  })
  return { promise, resolve }
}
```

Add `vi` to the Vitest import and reset `api.listAllConversations` in the shared
`beforeEach`.

- [ ] **Step 2: Write failing stale-upsert and refresh-race tests**

```ts
it("merges old upsert metadata without regressing the state tuple", () => {
  const store = useAppWorkspaceStore.getState()
  store.applyConversationUpsert(
    makeSummary({
      id: 1,
      title: "Current",
      status: "pending_review",
      awaiting_reply_token: "g2",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
  )
  store.applyConversationUpsert(
    makeSummary({
      id: 1,
      title: "Metadata from old upsert",
      status: "in_progress",
      awaiting_reply_token: null,
      updated_at: "2026-07-18T02:00:00.000Z",
    })
  )

  expect(useAppWorkspaceStore.getState().conversations[0]).toMatchObject({
    title: "Metadata from old upsert",
    status: "pending_review",
    awaiting_reply_token: "g2",
    updated_at: "2026-07-18T03:00:00.000Z",
  })
})

it("does not let an in-flight refresh overwrite a newer event patch", async () => {
  const pending = deferred<DbConversationSummary[]>()
  api.listAllConversations.mockReturnValueOnce(pending.promise)
  const store = useAppWorkspaceStore.getState()
  store.applyConversationUpsert(
    makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" })
  )

  const refresh = store.refreshConversations()
  store.applyConversationStatePatch({
    id: 1,
    status: "pending_review",
    awaiting_reply_token: "g2",
    updated_at: "2026-07-18T03:00:00.000Z",
  })
  store.applyConversationUpsert(
    makeSummary({ id: 2, updated_at: "2026-07-18T03:01:00.000Z" })
  )
  pending.resolve([
    makeSummary({ id: 1, updated_at: "2026-07-18T01:00:00.000Z" }),
  ])
  await refresh

  const rows = useAppWorkspaceStore.getState().conversations
  expect(rows.find((row) => row.id === 1)).toMatchObject({
    status: "pending_review",
    updated_at: "2026-07-18T03:00:00.000Z",
  })
  expect(rows.some((row) => row.id === 2)).toBe(true)
})
```

Also add:

- two overlapping refreshes where the second resolves first and the first is
  ignored;
- a later uncontended snapshot that omits id 2 and therefore removes it;
- a refresh/upsert timestamp newer than an optimistic baseline clearing the
  matching optimistic entry without emitting an animation activity signal;
- deleting a root immediately pruning its optimistic entry without advancing
  the activity sequence;
- tombstoned ids never returning from a refresh.

- [ ] **Step 3: Run the store tests and confirm RED**

```powershell
pnpm exec vitest run src/stores/app-workspace-store.test.ts
```

Expected: FAIL because refresh still replaces the array and upsert still replaces
the newer state tuple wholesale.

- [ ] **Step 4: Implement revision and request ordering**

Add module-local counters reset by `resetAppWorkspaceStore`:

```ts
let conversationRevision = 0
let latestConversationRefreshRequest = 0

function advanceConversationRevision() {
  conversationRevision += 1
}
```

Call `advanceConversationRevision()` exactly when a state patch, upsert, delete,
or local field patch changes the conversation array. Do not call it for loading,
errors, optimistic begin/rollback, or activity-sequence changes.

Implement stale state-tuple preservation:

```ts
function mergeConversationSummary(
  current: DbConversationSummary,
  incoming: DbConversationSummary
): DbConversationSummary {
  if (
    parseActivityTimestamp(incoming.updated_at) >=
    parseActivityTimestamp(current.updated_at)
  ) {
    return incoming
  }
  return {
    ...incoming,
    status: current.status,
    awaiting_reply_token: current.awaiting_reply_token,
    updated_at: current.updated_at,
  }
}
```

Refactor refresh around `requestId` and `revisionAtStart`. When the revision is
unchanged, use the non-tombstoned snapshot. When it changed, merge snapshot ids
against current rows and append current-only, non-tombstoned rows. Only the
latest request may write conversations, errors, or `conversationsLoading=false`.
Run the same optimistic-baseline reconciliation over the final rows before
commit.

Use one exact overlay reconciliation helper for upsert, remove, and refresh:

```ts
function reconcileOptimisticActivity(
  rows: readonly DbConversationSummary[],
  current: OptimisticActivityById
): OptimisticActivityById {
  const rowsById = new Map(rows.map((row) => [row.id, row]))
  let next: Map<number, OptimisticConversationActivity> | null = null
  for (const [id, activity] of current) {
    const row = rowsById.get(id)
    const acknowledged =
      row !== undefined &&
      parseActivityTimestamp(row.updated_at) >
        parseActivityTimestamp(activity.baselineUpdatedAt)
    if (!row || acknowledged || deletedIds.has(id)) {
      next ??= new Map(current)
      next.delete(id)
    }
  }
  return next ?? current
}
```

Import `OptimisticConversationActivity`/`OptimisticActivityById` as types from
Task 1. An older metadata upsert uses `mergeConversationSummary` first, so it
cannot falsely acknowledge the overlay with its stale timestamp.
`applyConversationRemove` runs reconciliation against the remaining rows after
adding the tombstone; removing optimistic state is cleanup only and must not
advance `conversationActivitySequence`.

- [ ] **Step 5: Run focused GREEN verification**

```powershell
pnpm exec vitest run src/stores/app-workspace-store.test.ts src/contexts/app-workspace-context.test.tsx
pnpm exec eslint src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts
```

Expected: all tests pass; context event routing remains unchanged.

- [ ] **Step 6: Commit monotonic reconciliation**

```powershell
git add src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts
git diff --cached --check
git commit -m "fix(sidebar): reconcile stale conversation snapshots"
```

---

### Task 3: Make Grouping Updated-Only And Mark Root Blocks

**Files:**
- Modify: `src/components/conversations/sidebar-conversation-grouping.ts:1-620`
- Modify: `src/components/conversations/sidebar-conversation-grouping.test.ts`

**Interfaces:**
- Consumes: `OptimisticActivityById`, `EMPTY_OPTIMISTIC_ACTIVITY_BY_ID`, and `getEffectiveConversationUpdatedAt` from Task 1.
- Produces: updated-only bucket selectors, required root-block ownership metadata, and `sidebarRowKey` for Tasks 4, 6, 7, and 8.
- Preserves: shallow array reuse and original summary object identity.

- [ ] **Step 1: Replace created-mode tests with effective-time ordering tests**

Update the grouping tests so there is no sort-mode argument and add explicit
opposite created/updated fixtures:

```ts
it("sorts every folder bucket by effective updated time", () => {
  const createdNewer = conv(2, 10, {
    created_at: "2026-07-18T03:00:00.000Z",
    updated_at: "2026-07-18T01:00:00.000Z",
  })
  const activeNewer = conv(1, 10, {
    created_at: "2026-07-18T01:00:00.000Z",
    updated_at: "2026-07-18T02:00:00.000Z",
  })
  const optimistic = new Map([
    [
      2,
      {
        token: "t2",
        baselineUpdatedAt: createdNewer.updated_at,
        effectiveAt: "2026-07-18T04:00:00.000Z",
      },
    ],
  ])

  const grouped = groupByFolderWithReuse(
    [createdNewer, activeNewer],
    new Map(),
    undefined,
    optimistic
  )
  expect(grouped.get(10)!.map((row) => row.id)).toEqual([2, 1])
})

it("sorts pinned roots by activity before pinned_at", () => {
  const olderPinButActive = conv(1, 10, {
    pinned_at: "2026-07-18T01:00:00.000Z",
    updated_at: "2026-07-18T04:00:00.000Z",
  })
  const newerPin = conv(2, 10, {
    pinned_at: "2026-07-18T03:00:00.000Z",
    updated_at: "2026-07-18T02:00:00.000Z",
  })
  expect(
    selectPinnedWithReuse([newerPin, olderPinButActive], [], new Map()).map(
      (row) => row.id
    )
  ).toEqual([1, 2])
})
```

Add tie-break tests for `created_at/id` and `pinned_at/id`, and retain the merged
worktree assertion with effective timestamps interleaved across root/worktree.

- [ ] **Step 2: Add failing root-block metadata tests**

Build an expanded root with child/grandchild and assert:

```ts
expect(
  rows
    .filter((row) => row.kind === "conversation")
    .map((row) => ({
      id: row.conversation.id,
      rootId: row.rootId,
      bucketKey: row.bucketKey,
      key: sidebarRowKey(row),
    }))
).toEqual([
  { id: 1, rootId: 1, bucketKey: "folder:10", key: "conv-claude_code-1" },
  { id: 100, rootId: 1, bucketKey: "folder:10", key: "conv-claude_code-100" },
  { id: 101, rootId: 1, bucketKey: "folder:10", key: "conv-claude_code-101" },
])
```

Add corresponding pinned (`"pinned"`) and chat (`"chat"`) root assertions.
Build an expanded root whose children are still loading and assert its
`SubsessionLoadingRow` receives the same `rootId` and `bucketKey`. Update
existing literal `SidebarRow` fixtures to include the required metadata.

- [ ] **Step 3: Run grouping tests and confirm RED**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-conversation-grouping.test.ts
```

Expected: FAIL because grouping still accepts `SidebarSortMode`, Pinned uses
`pinned_at`, and flattened rows have no block metadata.

- [ ] **Step 4: Implement effective comparators and updated-only selectors**

Use the shared resolver in all root comparators:

```ts
export function compareByUpdatedAtDesc(
  left: DbConversationSummary,
  right: DbConversationSummary,
  optimistic = EMPTY_OPTIMISTIC_ACTIVITY_BY_ID
): number {
  const updatedDiff =
    parseActivityTimestamp(
      getEffectiveConversationUpdatedAt(right, optimistic)
    ) -
    parseActivityTimestamp(getEffectiveConversationUpdatedAt(left, optimistic))
  if (updatedDiff !== 0) return updatedDiff

  const createdDiff =
    parseActivityTimestamp(right.created_at) -
    parseActivityTimestamp(left.created_at)
  return createdDiff !== 0 ? createdDiff : right.id - left.id
}
```

Pinned compares only the effective-updated primary key before its own ties:

```ts
export function compareByPinnedAtDesc(
  left: DbConversationSummary,
  right: DbConversationSummary,
  optimistic = EMPTY_OPTIMISTIC_ACTIVITY_BY_ID
): number {
  const updatedDiff =
    parseActivityTimestamp(
      getEffectiveConversationUpdatedAt(right, optimistic)
    ) -
    parseActivityTimestamp(getEffectiveConversationUpdatedAt(left, optimistic))
  if (updatedDiff !== 0) return updatedDiff
  const pinnedDiff =
    parseActivityTimestamp(right.pinned_at) -
    parseActivityTimestamp(left.pinned_at)
  return pinnedDiff !== 0 ? pinnedDiff : right.id - left.id
}
```

Remove `compareByCreatedAtDesc` and the `sortMode` branch. Extend the three
selector signatures exactly as fixed in Cross-Task Interfaces while retaining
the existing reference-reuse checks.

- [ ] **Step 5: Implement root/bucket propagation and stable row keys**

Make `ConversationRow` and `SubsessionLoadingRow` carry required `rootId` and
`bucketKey` fields. Pass the root metadata through recursive
`pushConversationRow` calls, including the loading-placeholder branch. Root
call sites use:

```ts
pushConversationRow(rows, conv, 0, conv.id, "pinned", ...)
pushConversationRow(rows, conv, 0, conv.id, `folder:${folderId}`, ...)
pushConversationRow(rows, conv, 0, conv.id, "chat", ...)
```

Move the component-local row-key switch into the grouping module as
`sidebarRowKey`; preserve every existing key string exactly.

- [ ] **Step 6: Run focused GREEN verification**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-conversation-grouping.test.ts
pnpm exec eslint src/components/conversations/sidebar-conversation-grouping.ts src/components/conversations/sidebar-conversation-grouping.test.ts
```

Expected: updated-only, pinned, worktree, child-order, metadata, and reference
reuse tests all pass.

- [ ] **Step 7: Commit grouping changes**

```powershell
git add src/components/conversations/sidebar-conversation-grouping.ts src/components/conversations/sidebar-conversation-grouping.test.ts
git diff --cached --check
git commit -m "feat(sidebar): order root blocks by activity"
```

---

### Task 4: Remove Sort Mode UI And Render Effective Activity

**Files:**
- Modify: `src/lib/sidebar-view-mode-storage.ts:1-125`
- Modify: `src/components/layout/sidebar.tsx:1-350`
- Modify: `src/components/layout/sidebar.test.tsx`
- Modify: `src/components/conversations/sidebar-conversation-list.tsx:60-1030,1935-1980`
- Modify: `src/components/conversations/sidebar-conversation-list.test.tsx`

**Interfaces:**
- Consumes: optimistic store state from Task 1 and updated-only grouping APIs from Task 3.
- Produces: a sidebar whose DOM order and time label use the same effective timestamp.
- Preserves: show-completed and section-order persistence; old locale keys remain untouched.

- [ ] **Step 1: Write failing view-menu and updated-order tests**

In `sidebar.test.tsx`, open the Funnel button and assert the sort controls are
absent while other view controls remain:

```tsx
it("does not expose a created-time sort mode", () => {
  const { getByLabelText, queryByText, getByText } = renderSidebar()
  fireEvent.click(getByLabelText("View options"))
  expect(queryByText("Sort by")).toBeNull()
  expect(queryByText("Created time")).toBeNull()
  expect(getByText("Show completed")).toBeTruthy()
  expect(getByText("Section order")).toBeTruthy()
})
```

In `sidebar-conversation-list.test.tsx`, replace the old created-mode label test
with an order assertion where created and updated times disagree. Add an
optimistic promotion assertion:

```ts
it("promotes a real optimistic activity and labels it now", () => {
  render(tree())
  const before = document.body.textContent ?? ""
  expect(before.indexOf("conv-12")).toBeGreaterThan(before.indexOf("conv-11"))

  act(() => {
    useAppWorkspaceStore.getState().beginConversationActivity(12)
  })

  const after = document.body.textContent ?? ""
  expect(after.indexOf("conv-12")).toBeLessThan(after.indexOf("conv-11"))
  const row = document.querySelector('[data-conversation-id="12"]')
  expect(row?.textContent).toContain("now")
})
```

Seed id 12 with an older authoritative `updated_at` so the test proves the
overlay, not initial data order, caused the promotion.

- [ ] **Step 2: Run focused tests and confirm RED**

```powershell
pnpm exec vitest run src/components/layout/sidebar.test.tsx src/components/conversations/sidebar-conversation-list.test.tsx
```

Expected: the menu still renders sort controls, the list prop defaults to
created mode, and the list does not subscribe to optimistic activity.

- [ ] **Step 3: Remove sort persistence and UI wiring**

Delete only these from `sidebar-view-mode-storage.ts`:

```text
SORT_MODE_KEY
SidebarSortMode
loadSortMode
saveSortMode
```

In `sidebar.tsx`, remove their imports, sort state, hydration, handler, radio
group, separator owned only by that group, and `sortMode` prop. Keep the Funnel
button because Show completed and Section order remain.

Do not edit any `i18n/messages/*.json` file.

- [ ] **Step 4: Wire one effective timestamp through list sorting and labels**

In `SidebarConversationList`:

```ts
const optimisticActivityById = useAppWorkspaceStore(
  (state) => state.optimisticActivityById
)
```

Remove `sortMode` from props. Pass the optimistic map to folder, chat, and pinned
selectors. Resolve the card label once:

```tsx
timeLabel={formatRelative(
  getEffectiveConversationUpdatedAt(conv, optimisticActivityById),
  now
)}
```

Keep `conversation={conv}` authoritative; do not spread an optimistic timestamp
into the summary. Add `data-conversation-id` to the existing inner row wrapper
for the focused test without changing accessible semantics.

- [ ] **Step 5: Update obsolete test props/comments and run GREEN verification**

Remove every `sortMode="created"` test prop and created-mode comment. Then run:

```powershell
pnpm exec vitest run src/components/layout/sidebar.test.tsx src/components/conversations/sidebar-conversation-list.test.tsx src/components/conversations/sidebar-conversation-grouping.test.ts
pnpm exec eslint src/lib/sidebar-view-mode-storage.ts src/components/layout/sidebar.tsx src/components/layout/sidebar.test.tsx src/components/conversations/sidebar-conversation-list.tsx src/components/conversations/sidebar-conversation-list.test.tsx
```

Expected: all commands exit 0; updated order, optimistic `now`, memo probes,
sticky headers, pinned migration, and folder drag tests pass.

- [ ] **Step 6: Commit updated-only presentation**

```powershell
git add src/lib/sidebar-view-mode-storage.ts src/components/layout/sidebar.tsx src/components/layout/sidebar.test.tsx src/components/conversations/sidebar-conversation-list.tsx src/components/conversations/sidebar-conversation-list.test.tsx
git diff --cached --check
git commit -m "feat(sidebar): make activity order mandatory"
```

---

### Task 5: Begin Activity Only At Real ACP Dispatch Boundaries

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx:5153-5290`
- Modify: `src/contexts/acp-connections-context.test.tsx:1-260,2780-2860`
- Modify: `src/hooks/use-connection-lifecycle.test.ts:1-170`

**Interfaces:**
- Consumes: `beginConversationActivity` and token rollback from Task 1.
- Produces: prompt and structured-question activity with exact failure rollback.
- Preserves: current `TurnBusyError` mapping, queue bounce, viewer control, and child question routing.

- [ ] **Step 1: Extend ACP provider test fixtures**

Add `acpAnswerQuestionMock` to the hoisted API fixtures and API mock. Import and
reset the real workspace store in the provider test `beforeEach`:

```ts
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"

const acpAnswerQuestionMock = vi.hoisted(() => vi.fn())

// Inside vi.mock("@/lib/api")
acpAnswerQuestion: acpAnswerQuestionMock,
```

Seed a root summary with id 2 before dispatch tests. Use the same complete
summary factory shape as `app-workspace-store.test.ts`.

- [ ] **Step 2: Write failing prompt activity tests**

Add tests proving:

```ts
it("begins root activity immediately before acpPrompt and keeps it on success", async () => {
  await mountProvider()
  await act(async () => {
    await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
  })
  expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
    false
  )

  await act(async () => {
    await h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }])
  })

  expect(acpPromptMock).toHaveBeenCalledTimes(1)
  expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
    true
  )
})

it("rolls back the exact prompt token when acpPrompt rejects", async () => {
  acpPromptMock.mockRejectedValueOnce(new Error("send failed"))
  await mountProvider()
  await act(async () => {
    await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
  })

  await expect(
    h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }])
  ).rejects.toThrow("send failed")
  expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
    false
  )
})
```

Also cover explicit `opts.conversationId` taking precedence over the bound id,
an unknown context producing no activity, and a viewer root still being allowed
to begin activity through the connection-bound id. Build the viewer with the
suite's existing discovery path and deliberately omit an explicit send id:

```ts
h.acpFindConnectionForConversation.mockResolvedValueOnce({
  connection_id: "owner-conn",
  event_seq: 0,
})
await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
await h.actions!.sendPrompt(TAB, [{ type: "text", text: "viewer send" }])
expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(true)
```

- [ ] **Step 3: Write failing structured-question tests**

Test root success retention, rejection rollback, and child exclusion:

```ts
await h.actions!.answerQuestion(TAB, "q-1", {
  answers: [{ questionId: "choice", labels: ["A"] }],
  declined: false,
})
expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(true)
```

Attach a delegation child with `attachDelegationChild`, call its
`answerQuestion`, and assert no root optimistic entry was created.

```ts
h.actions!.attachDelegationChild({
  connectionId: "child-1",
  parentConnectionId: "spawned-conn",
  parentToolUseId: "tool-1",
  agentType: "codex",
})
await h.actions!.answerQuestion("child-1", "q-child", {
  answers: [{ questionId: "choice", labels: ["A"] }],
  declined: false,
})
expect(useAppWorkspaceStore.getState().optimisticActivityById.size).toBe(0)
```

- [ ] **Step 4: Add a lifecycle mode-failure regression test**

In `use-connection-lifecycle.test.ts`, reject `setMode`, invoke `handleSend` with
a different mode, and assert `sendPrompt` was never called:

```ts
it("does not reach prompt dispatch when mode change fails", async () => {
  h.setMode.mockRejectedValueOnce(new Error("mode failed"))
  const { result } = renderHook(() =>
    useConnectionLifecycle({
      contextKey: "tab-1",
      agentType: "claude_code",
      isActive: true,
    })
  )
  act(() => {
    result.current.handleSend(
      { blocks: [{ type: "text", text: "wire" }], displayText: "wire" },
      "plan"
    )
  })
  await waitFor(() => expect(h.setMode).toHaveBeenCalledWith("plan"))
  expect(h.sendPrompt).not.toHaveBeenCalled()
})
```

- [ ] **Step 5: Run focused tests and confirm RED**

```powershell
pnpm exec vitest run src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts
```

Expected: provider activity assertions fail because neither inner action touches
the workspace store. The mode-failure regression already passes and protects the
chosen boundary.

- [ ] **Step 6: Implement one root-activity guard for both actions**

Add a local helper near the actions:

```ts
function beginRootConversationActivity(
  connection: ConnectionState,
  explicitConversationId?: number | null
): { id: number; token: string } | null {
  if (connection.isDelegationChild) return null
  const id = explicitConversationId ?? connection.conversationId ?? null
  if (id == null) return null
  const token =
    useAppWorkspaceStore.getState().beginConversationActivity(id)
  return token ? { id, token } : null
}

function rollbackRootConversationActivity(
  activity: { id: number; token: string } | null
) {
  if (!activity) return
  useAppWorkspaceStore
    .getState()
    .rollbackConversationActivity(activity.id, activity.token)
}
```

In `sendPrompt`, call it only after `conn` exists and immediately before
`acpPrompt`; wrap the await in `try/catch`, rollback, and rethrow unchanged.
Use `opts?.conversationId` before `conn.conversationId`.

In `answerQuestion`, begin immediately before `acpAnswerQuestion`; rollback and
rethrow on failure. Do not change `respondPermission`.

The existing `connectAsViewer` path currently drops the persisted conversation
id even though both callers know it. Extend its signature with
`conversationId: number | null`, include that field in `CONNECTION_CREATED`, and
pass the current `conversationId` from both the discovery and route-conflict
call sites. This is local connection metadata, not a wire-contract change, and
is required for viewer prompts and structured answers to use the bound-id
fallback tested above.

- [ ] **Step 7: Run focused GREEN verification**

```powershell
pnpm exec vitest run src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts src/components/chat/ask-question-card.test.tsx src/components/message/sub-agent-session-dialog.test.tsx
pnpm exec eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts
```

Expected: all prompt/question, retry, child, and existing provider tests pass.

- [ ] **Step 8: Commit dispatch triggers**

```powershell
git add src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts
git diff --cached --check
git commit -m "feat(sidebar): promote conversations on user dispatch"
```

---

### Task 6: Add Pure Root-Permutation And Anchor Math

**Files:**
- Create: `src/components/conversations/sidebar-reorder-animation.ts`
- Create: `src/components/conversations/sidebar-reorder-animation.test.ts`

**Interfaces:**
- Consumes: `SidebarRow`, `SidebarBucketKey`, and `sidebarRowKey` from Task 3.
- Produces: all pure animation interfaces fixed in Cross-Task Interfaces.
- Does not access: DOM, React, Zustand, clocks, or WAAPI.

- [ ] **Step 1: Write failing root-order eligibility tests**

Define the complete structural fixture first:

```ts
type RejectedScenario =
  | "pin-transfer"
  | "filter-membership"
  | "folder-collapse"
  | "subtree-expansion"
  | "unrelated-structure"
  | "downward-move"

function root(
  id: number,
  bucketKey: SidebarBucketKey = "folder:10",
  folderId = 10,
  rootId = id,
  depth = 0
): ConversationRow {
  return {
    kind: "conversation",
    conversation: {
      id,
      agent_type: "claude_code",
      folder_id: folderId,
    } as DbConversationSummary,
    depth,
    rootId,
    bucketKey,
  }
}

function rowsForFolder(roots: ConversationRow[]): SidebarRow[] {
  return [
    { kind: "section", section: "folders", expanded: true, count: 1 },
    { kind: "folder", folderId: 10 },
    ...roots,
  ]
}

function scenarioFixture(scenario: RejectedScenario): {
  before: SidebarRootOrderSnapshot
  after: SidebarRootOrderSnapshot
  activityId: number
} {
  const beforeRows = rowsForFolder([root(1), root(2), root(3)])
  let afterRows: SidebarRow[] = beforeRows
  switch (scenario) {
    case "pin-transfer":
      afterRows = rowsForFolder([root(3, "pinned"), root(1), root(2)])
      break
    case "filter-membership":
      afterRows = rowsForFolder([root(1), root(2)])
      break
    case "folder-collapse":
      afterRows = rowsForFolder([])
      break
    case "subtree-expansion":
      afterRows = rowsForFolder([
        root(3),
        root(30, "folder:10", 10, 3, 1),
        root(1),
        root(2),
      ])
      break
    case "unrelated-structure":
      afterRows = [
        { kind: "section", section: "folders", expanded: true, count: 1 },
        { kind: "empty", folderId: 99, totalConversationCount: 0 },
        { kind: "folder", folderId: 10 },
        root(3),
        root(1),
        root(2),
      ]
      break
    case "downward-move":
      return {
        before: buildSidebarRootOrderSnapshot(
          rowsForFolder([root(3), root(1), root(2)])
        ),
        after: buildSidebarRootOrderSnapshot(beforeRows),
        activityId: 3,
      }
  }
  return {
    before: buildSidebarRootOrderSnapshot(beforeRows),
    after: buildSidebarRootOrderSnapshot(afterRows),
    activityId: 3,
  }
}
```

Then cover the accepted permutation and every rejection:

```ts
it("accepts only an upward same-bucket root permutation", () => {
  const before = buildSidebarRootOrderSnapshot(
    rowsForFolder([root(1), root(2), root(3)])
  )
  const after = buildSidebarRootOrderSnapshot(
    rowsForFolder([root(3), root(1), root(2)])
  )
  expect(detectSidebarActivityReorder(before, after, 3)).toEqual({
    conversationId: 3,
    bucketKey: "folder:10",
    previousIndex: 2,
    nextIndex: 0,
  })
})

it.each([
  "pin-transfer",
  "filter-membership",
  "folder-collapse",
  "subtree-expansion",
  "unrelated-structure",
  "downward-move",
])("rejects %s", (scenario) => {
  const { before, after, activityId } = scenarioFixture(scenario)
  expect(detectSidebarActivityReorder(before, after, activityId)).toBeNull()
})
```

Add a worktree assertion using `root(4, "folder:10", 11)`: its snapshot entry
must be `bucketByRoot.get(4) === "folder:10"`, proving raw
`conversation.folder_id` is never consulted.

- [ ] **Step 2: Write failing anchor and delta tests**

```ts
it("chooses the first fully visible surviving row outside the promoted block", () => {
  const before = new Map<string, SidebarMeasuredRow>([
    ["partial", { key: "partial", rootId: 1, top: 90, bottom: 120 }],
    ["promoted", { key: "promoted", rootId: 3, top: 120, bottom: 152 }],
    ["stable", { key: "stable", rootId: 1, top: 152, bottom: 184 }],
  ])
  expect(
    selectSidebarAnchor(
      before,
      new Set(["partial", "promoted", "stable"]),
      100,
      300,
      3
    )?.key
  ).toBe("stable")
})

it("uses opposite signs for scroll correction and FLIP", () => {
  expect(sidebarAnchorScrollDelta(152, 184)).toBe(32)
  expect(sidebarFlipDeltaY(152, 184)).toBe(-32)
})
```

Also cover missing survivor, exact viewport edges, and zero delta.

- [ ] **Step 3: Run the new tests and confirm RED**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-reorder-animation.test.ts
```

Expected: FAIL because the pure animation module does not exist.

- [ ] **Step 4: Implement structural snapshots and eligibility**

`buildSidebarRootOrderSnapshot` must:

- preserve unowned structural row keys in rendered order;
- add each depth-0 root once to `rootsByBucket`;
- append every owned conversation or subsession-loading row key to its root's
  contiguous block list; and
- store the row-provided `bucketKey`, never derive from `folder_id`.

`detectSidebarActivityReorder` returns non-null only when structural row keys,
bucket keys, root membership sets, and every root's block-row keys are identical;
the activity root stays in one bucket and moves to a lower index. Implement
small exact-array/map equality helpers inside the module.

- [ ] **Step 5: Implement anchor selection and delta helpers**

`selectSidebarAnchor` iterates rows ordered by `top`, accepts only
`top >= viewportTop && bottom <= viewportBottom`, requires survival after the
reorder, and excludes `rootId === promotedRootId`. Delta helpers are exactly:

```ts
export const sidebarAnchorScrollDelta = (
  beforeTop: number,
  afterTop: number
) => afterTop - beforeTop

export const sidebarFlipDeltaY = (beforeTop: number, afterTop: number) =>
  beforeTop - afterTop
```

- [ ] **Step 6: Run focused GREEN verification**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-reorder-animation.test.ts src/components/conversations/sidebar-conversation-grouping.test.ts
pnpm exec eslint src/components/conversations/sidebar-reorder-animation.ts src/components/conversations/sidebar-reorder-animation.test.ts
```

Expected: every structural and geometry case passes without a DOM environment.

- [ ] **Step 7: Commit pure animation logic**

```powershell
git add src/components/conversations/sidebar-reorder-animation.ts src/components/conversations/sidebar-reorder-animation.test.ts
git diff --cached --check
git commit -m "feat(sidebar): classify activity reorder animations"
```

---

### Task 7: Build The DOM Animation And Scroll-Anchor Hook

**Files:**
- Create: `src/components/conversations/use-sidebar-reorder-animation.ts`
- Create: `src/components/conversations/use-sidebar-reorder-animation.test.tsx`

**Interfaces:**
- Consumes: pure snapshot/geometry helpers from Task 6 and the fixed hook options.
- Produces: `useSidebarReorderAnimation` plus `handleUserScroll` for Task 8.
- Owns: only styles/animations on `[data-sidebar-row-key]` inner wrappers.

- [ ] **Step 1: Create a deterministic hook harness and failing movement test**

The harness renders a real scrollable viewport and stable wrappers from supplied
rows. Stub `getBoundingClientRect` from each wrapper's `data-top`, stub
`matchMedia`, and install an `animate` spy returning an object with
`cancel`, `commitStyles`, `finished`, and `onfinish` support.

```tsx
it("animates painted displaced rows for 230 ms with the fixed easing", () => {
  const { rerender } = render(
    <Harness rows={beforeRows} sequence={0} activityId={null} />
  )
  rerender(<Harness rows={afterRows} sequence={1} activityId={3} />)

  expect(animateMock).toHaveBeenCalledWith(
    [
      { transform: "translateY(64px)" },
      { transform: "translateY(0px)" },
    ],
    {
      duration: 230,
      easing: "cubic-bezier(0.2, 0, 0, 1)",
      fill: "both",
    }
  )
})
```

Use deterministic before/after top maps so promoted and displaced deltas are
asserted separately.

- [ ] **Step 2: Add failing fade, anchor, cancellation, and reduced-motion tests**

Cover all of these exact outcomes:

- a promoted root absent from First but mounted in Last receives opacity
  `0 -> 1`, duration 120, fixed easing, and no translate animation;
- when `scrollTop > 0`, an anchor moving from 152 to 184 adds 32 to scrollTop
  before move animations are created;
- at `scrollTop === 0`, no anchor correction occurs;
- the programmatic scroll event is ignored until the next animation frame;
- a real user scroll cancels all active animations and clears transforms;
- a second eligible sequence calls `commitStyles`, cancels the old animation,
  and uses current visual rects as the next First snapshot;
- `dragging=true`, an ineligible structure change, and unmount each cancel;
- reduced motion creates zero animations but still applies anchor correction;
- missing `Element.prototype.animate` degrades to final layout with no throw.

- [ ] **Step 3: Run hook tests and confirm RED**

```powershell
pnpm exec vitest run src/components/conversations/use-sidebar-reorder-animation.test.tsx
```

Expected: FAIL because the hook does not exist.

- [ ] **Step 4: Implement bounded wrapper capture and active-animation cleanup**

The hook queries only the supplied `viewportEl`:

```ts
const nodes = viewportEl.querySelectorAll<HTMLElement>(
  "[data-sidebar-row-key]"
)
```

Capture key, optional numeric root id, rectangle, and element in maps. Keep refs
for the prior model/rectangles, active `Animation`s, the consumed sequence, and
programmatic-scroll suppression.

Measure viewport bounds from `viewportEl.getBoundingClientRect().top/.bottom`;
never assume that the viewport starts at client coordinate zero.

In layout-effect cleanup, feature-detect `commitStyles`, commit/cancel current
animations, capture their current visual rectangles, then clear only
controller-owned `transform` and `opacity`. The next layout effect builds Last,
classifies the activity, and always consumes the sequence even when skipped.

Use the same client-safe layout-effect alias already established by the sidebar:

```ts
const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect
```

Return a stable `handleUserScroll` callback and memoized controls object so the
existing `Virtualizer.onScroll` callback does not churn on every list render.

- [ ] **Step 5: Implement anchor correction, transform FLIP, and offscreen fade**

For eligible activity and `viewportEl.scrollTop > 0`, select the stable anchor,
apply `viewportEl.scrollTop += delta`, mark programmatic suppression through one
`requestAnimationFrame`, and recapture Last geometry.

Animate shared First/Last keys with non-zero deltas:

```ts
element.animate(
  [
    { transform: `translateY(${deltaY}px)` },
    { transform: "translateY(0px)" },
  ],
  {
    duration: 230,
    easing: "cubic-bezier(0.2, 0, 0, 1)",
    fill: "both",
  }
)
```

For destination wrappers belonging to the promoted root but absent from First,
use opacity `0 -> 1`, duration 120, the same easing, and `fill: "both"`. Clean
styles on finish/cancel. Do not animate zero deltas or wrappers without stable
keys.

`handleUserScroll` returns without cancellation while suppression is active;
otherwise it cancels/rebases every active animation. Reduced motion skips both
WAAPI branches but not anchor correction.

- [ ] **Step 6: Run focused GREEN verification**

```powershell
pnpm exec vitest run src/components/conversations/use-sidebar-reorder-animation.test.tsx src/components/conversations/sidebar-reorder-animation.test.ts
pnpm exec eslint src/components/conversations/use-sidebar-reorder-animation.ts src/components/conversations/use-sidebar-reorder-animation.test.tsx
```

Expected: all duration, easing, anchor, cancellation, retarget, fallback, and
reduced-motion assertions pass.

- [ ] **Step 7: Commit the DOM controller**

```powershell
git add src/components/conversations/use-sidebar-reorder-animation.ts src/components/conversations/use-sidebar-reorder-animation.test.tsx
git diff --cached --check
git commit -m "feat(sidebar): animate painted conversation reorders"
```

---

### Task 8: Integrate Animation With Virtua And Verify The Complete Feature

**Files:**
- Modify: `src/components/conversations/sidebar-conversation-list.tsx:550-1030,1250-1350,1935-2090`
- Modify: `src/components/conversations/sidebar-conversation-list.test.tsx`
- Modify only plan-owned frontend files required by a failure directly caused by Tasks 1-7.

**Interfaces:**
- Consumes: store activity signal, root/bucket metadata, `sidebarRowKey`, and `useSidebarReorderAnimation`.
- Produces: the complete updated-time promotion and swap-animation workflow.
- Preserves: sticky headers, `scrollToActive`, folder drag/autoscroll, memo probes, Pinned/Chat/Folder sections, and the existing Virtualizer buffer.

- [ ] **Step 1: Add failing integration assertions for wrapper metadata and scroll cancellation**

In `sidebar-conversation-list.test.tsx`, change the ScrollArea mock to pass a
real mounted viewport element rather than a detached `document.createElement`.
Assert every conversation wrapper has stable attributes:

```ts
expect(
  document.querySelector('[data-sidebar-row-key="conv-claude_code-11"]')
).toMatchObject({
  dataset: expect.objectContaining({
    sidebarRootId: "11",
    sidebarBucketKey: "folder:1",
  }),
})
```

Expose the hook's `handleUserScroll` as a spy through a focused module mock and
assert the existing `virtuaCtl.onScroll` calls it while still scheduling sticky
header recomputation.

- [ ] **Step 2: Run the list tests and confirm RED**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-conversation-list.test.tsx
```

Expected: wrapper metadata and animation scroll integration are absent.

- [ ] **Step 3: Wire store signals and the animation hook**

Subscribe narrowly:

```ts
const conversationActivitySequence = useAppWorkspaceStore(
  (state) => state.conversationActivitySequence
)
const lastConversationActivityId = useAppWorkspaceStore(
  (state) => state.lastConversationActivityId
)

const reorderAnimation = useSidebarReorderAnimation({
  rows,
  activitySequence: conversationActivitySequence,
  activityConversationId: lastConversationActivityId,
  viewportEl,
  dragging: dragging !== null,
})
```

Call `reorderAnimation.handleUserScroll()` at the start of
`handleVirtuaScroll`, before coalescing the existing sticky recompute. Add it to
the callback dependency list.

- [ ] **Step 4: Put metadata on the inner child, never the Virtua item**

Replace the local row-key function with the exported helper and render:

```tsx
{(row: SidebarRow) => {
  const ownedRow =
    row.kind === "conversation" || row.kind === "subsession-loading"
      ? row
      : null
  return (
    <div
      key={sidebarRowKey(row)}
      data-sidebar-row-key={sidebarRowKey(row)}
      data-sidebar-root-id={ownedRow?.rootId}
      data-sidebar-bucket-key={ownedRow?.bucketKey}
      data-conversation-id={
        row.kind === "conversation" ? row.conversation.id : undefined
      }
    >
      {renderRow(row)}
    </div>
  )
}}
```

Do not pass a custom `item`, `shift`, transform, transition, or animation prop to
`Virtualizer`. Keep `itemSize={32}` and `bufferSize={400}` unchanged.

- [ ] **Step 5: Run all focused feature tests**

```powershell
pnpm exec vitest run src/lib/conversation-activity.test.ts src/stores/app-workspace-store.test.ts src/contexts/app-workspace-context.test.tsx src/components/conversations/sidebar-conversation-grouping.test.ts src/components/layout/sidebar.test.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts src/components/conversations/sidebar-reorder-animation.test.ts src/components/conversations/use-sidebar-reorder-animation.test.tsx src/components/conversations/sidebar-conversation-list.test.tsx
```

Expected: every listed file passes with zero failures and no unhandled promise
rejection.

- [ ] **Step 6: Run complete frontend verification**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: ESLint, the full Vitest suite, strict TypeScript/static export, and
Next build all exit 0. No Rust command is required because this plan changes no
Rust file or wire contract.

- [ ] **Step 7: Run the desktop large-list manual smoke**

Start the normal desktop development app from a separate PowerShell terminal:

```powershell
pnpm tauri dev
```

Using the existing local database with at least 100 roots, execute the approved
manual matrix:

1. visible non-top regular root with expanded descendants;
2. offscreen root while scrolled in the middle;
3. pinned, chat, and worktree-mapped roots;
4. queued send, fork-send, legacy question, and structured question;
5. forced busy send and transport failure;
6. another conversation becoming active during a long turn;
7. pin/unpin, completed filter, section/folder/subtree collapse, refresh, and
   folder drag;
8. two promotions within 230 ms;
9. operating-system reduced motion enabled.

Expected: behavior matches all ten acceptance criteria in the approved design;
no action auto-scrolls to the promoted root, no structural action animates, and
no row retains a transform after completion/cancellation.

- [ ] **Step 8: Run server-browser cross-client smoke**

Build the static frontend and start server mode on its documented default port:

```powershell
pnpm build
$env:CODEG_STATIC_DIR = (Resolve-Path 'out').Path
Set-Location src-tauri
cargo run --no-default-features --bin codeg-server
```

Open `http://127.0.0.1:3080` in two clients. Send from each client, including a
structured question answer, and verify both sidebars converge after the
authoritative patch while only the sending client uses a temporary optimistic
timestamp. Repeat one offscreen promotion to verify independent scroll anchors.

- [ ] **Step 9: Review scope and commit integration**

```powershell
git diff --check
git status --short
git diff -- src/lib/conversation-activity.ts src/stores/app-workspace-store.ts src/components/conversations/sidebar-conversation-grouping.ts src/components/layout/sidebar.tsx src/contexts/acp-connections-context.tsx src/components/conversations/sidebar-reorder-animation.ts src/components/conversations/use-sidebar-reorder-animation.ts src/components/conversations/sidebar-conversation-list.tsx
git add src/components/conversations/sidebar-conversation-list.tsx src/components/conversations/sidebar-conversation-list.test.tsx
git diff --cached --check
git commit -m "feat(sidebar): integrate conversation swap animation"
```

Expected: the commit contains only list integration/tests. Any scoped correction
made after Steps 5-8 must be staged with its exact plan-owned path in a separate
fix commit; never stage the pre-existing unrelated worktree changes.

---

## Final Review Checklist

- The card label and all three root bucket comparators call the same effective
  timestamp resolver.
- No frontend code writes to authoritative `summary.updated_at`.
- Prompt and root question failures use token-CAS rollback; a stale failure
  cannot erase a newer activity.
- State patch, upsert, and refresh paths cannot regress the state tuple.
- Created-time mode has no state, prop, storage read, or visible control.
- Pinned, chat, folder, and mapped-worktree buckets never exchange rows.
- Child order and child-to-root non-promotion remain unchanged.
- Subsession loading placeholders inherit their root block and cannot become a
  false stable anchor.
- Viewer connections retain their known root id for prompt/question activity.
- Removing a root immediately prunes any optimistic activity without signaling
  a reorder.
- Pure eligibility rejects pin/filter/collapse/expand/drag/refresh changes.
- Only inner wrappers receive WAAPI styles; Virtua outer positioning is untouched.
- Anchor correction never calls `scrollToActive` and never runs at absolute top.
- User scroll and drag cancel/rebase; programmatic anchor scroll does not.
- Retargeted animations do not stack; completed/cancelled styles are cleared.
- Offscreen promotions fade only when mounted; no fake movement path is created.
- Reduced motion creates no WAAPI animation.
- Focused tests, full lint, full Vitest, static build, desktop smoke, and
  two-client server smoke have fresh passing evidence before completion is
  claimed.

---

*Execution starts only after choosing one of the handoff options presented when
this plan is handed back.*
