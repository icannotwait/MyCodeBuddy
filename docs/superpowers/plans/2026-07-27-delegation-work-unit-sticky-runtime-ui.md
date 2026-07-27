# Delegation Work-Unit Sticky Runtime UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep parent Codeg delegation cards showing continuous `生成中 | elapsed | N tool uses` for an entire sticky work unit across recovery-owned orchestration cancels, continue/replace, re-seed gaps, and continuation re-entry (**latest card only**); suppress `*Conversation interrupted*` assistant body text on delegated child sessions only (presentation).

**Architecture:** Namespaced frontend sticky store (`backendCacheKey` + parent + unit) with peak-by-task tool fold and recovery-gated phase machine; observe from effects (not pure build); each card computes latest-eligibility via pure compare against bucket active fields; merge into `buildDelegationCardModel`; coerce badge + lifecycle while sticky-active on latest; prepend streaming label in `DelegationCardChrome`; presentation-only suppress in live transcript materialization + historical message list render.

**Tech Stack:** TypeScript strict, React 19, next-intl, Vitest, Testing Library, `useSyncExternalStore`, existing `useDelegationCardModel` / `DelegationCardChrome` / live-transcript + message-list stores.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-27-delegation-work-unit-sticky-runtime-ui-design.md` (**r2**).
- Card baseline: `docs/superpowers/specs/2026-07-19-delegation-card-title-and-runtime-ui-design.md`.
- `CONTINUATION_CHECKPOINT_MS` remains **600_000**; do not change it.
- Display-only: no Broker Join ownership, Detach, or vendor `codex-acp` changes.
- Never fabricate tool counts; never mutate sticky store from pure `buildDelegationCardModel`.
- `STICKY_ORPHAN_TIMEOUT_MS = 900_000`.
- `parent_canceled` / explicit usercancel / `cancel_delegation` → true terminal.
- Keep-sticky intermediates require **positive recovery** (per-child scoped wait only).
- Backend key: `getActiveBackendCacheKey()` from `@/lib/transport`; register reset with `registerBackendScopedStoreReset`.
- Prettier: no semicolons, trailing commas es5, 2-space indent, 80-char width.
- Never `git add -A`. Stage exact paths. `docs/superpowers/**` may need `git add -f`.
- Gate: `pnpm exec tsc --noEmit --incremental false` + targeted Vitest + broader `pnpm test` for touched areas.

## File Map

| Path | Responsibility |
|------|----------------|
| `src/lib/delegation-sticky-runtime.ts` | Identity, fold, phase, orphan evaluate, pure apply, `isLatestStickyCard` |
| `src/lib/delegation-sticky-runtime.test.ts` | Machine + fold + recovery + latest-only pure tests |
| `src/lib/delegation-sticky-store.ts` | External store + orphan timer + backend reset registration |
| `src/lib/delegation-sticky-store.test.ts` | Subscribers, timer, retention, backend reset |
| `src/lib/delegation-conversation-interrupted.ts` | Marker normalize + detect |
| `src/lib/delegation-conversation-interrupted.test.ts` | Marker grammar tests |
| `src/hooks/use-delegation-card-model.ts` | Observe + pure merge + badge coerce |
| `src/hooks/use-delegation-card-model.test.ts` | Latest-only multi-card + recovery cases |
| `src/components/message/delegation-card-chrome.tsx` | Generating prefix |
| `src/components/message/delegation-card-chrome.test.tsx` | Ops line tests |
| `src/components/message/delegated-sub-thread.tsx` | Pass showGeneratingSegment; keep toolCallId key |
| `src/components/chat/sub-agent-overlay.tsx` | Pass model chrome props only (no second sticky identity) |
| `src/components/chat/sub-agent-overlay.test.tsx` | Overlay uses model generating flag |
| `src/stores/live-transcript-store.ts` | Live presentation filter for interrupt marker |
| `src/stores/live-transcript-store.test.ts` | Live suppress tests |
| `src/components/message/message-list-view.tsx` | Historical render fallback hide |
| `src/components/message/message-list-view.test.tsx` (or nearest existing) | Render suppress + footer preserved |
| `src/stores/backend-scoped-store-reset.ts` | Existing registry (register only) |

## Locked contracts

### Identity

```ts
export type StickyUnit =
  | { kind: "work_unit"; workUnitKey: string }
  | { kind: "parent_child"; childConversationId: number }
  | { kind: "task"; taskId: string }

export type StickyIdentity = {
  backendCacheKey: string
  parentConversationId: number
  unit: StickyUnit
}

export type StickyKeyResult =
  | StickyIdentity
  | { kind: "task_only"; backendCacheKey: string; taskId: string }

export function resolveStickyIdentity(input: {
  backendCacheKey: string
  parentConversationId?: number | null
  childConversationId?: number | null
  workUnitKey?: string | null
  taskId?: string | null
}): StickyKeyResult | null

/** Canonical map key. Alias name for design’s stickyKeyToString. */
export function stickyIdentityToString(id: StickyKeyResult): string
export const stickyKeyToString = stickyIdentityToString
```

Never key by bare `workUnitKey` without parent+backend.

### Observation + bucket

```ts
export type RecoverySignals = {
  liveBindingRunning: boolean
  childProjectionRunning: boolean
  activeRunNonTerminal: boolean
  openAttention: boolean
  /** True only when wait set intersects this unit’s child/task — not global wait alone */
  parentWaitingForThisChild: boolean
  continueOrReplaceAdmitted: boolean
}

export type StickyObservation = {
  type: "running" | "stats" | "canceled" | "completed" | "failed" | "tick" | "reseed"
  taskId: string
  nowMs: number
  generation?: number | null
  parentToolUseId?: string | null
  startedAt?: string | null
  finishedAt?: string | null
  toolCallCount?: number | null
  errorCode?: string | null
  cancelReason?: string | null
  recovery: RecoverySignals
}

export type TaskMeta = {
  generation: number | null
  startedAtMs: number | null
  finishedAtMs: number | null
  parentToolUseId: string | null
}

export type StickyBucket = {
  identityKey: string
  phase: StickyPhase
  anchorStartedAtMs: number | null
  terminalElapsedMs: number | null
  peakByTaskId: Map<string, number>
  taskMeta: Map<string, TaskMeta>
  activeTaskId: string | null
  activeParentToolUseId: string | null
  activeGeneration: number | null
  orphanStartedAtMs: number | null
  lastDisplayToolCount: number
}

export type StickyPhase = "active_sticky" | "terminal"
export const STICKY_ORPHAN_TIMEOUT_MS = 900_000

export function hasPositiveRecovery(s: RecoverySignals): boolean
export function foldToolCount(
  state: { peakByTaskId: ReadonlyMap<string, number> },
  taskId: string,
  count: number
): { peakByTaskId: Map<string, number>; display: number }

export function applyStickyObservation(
  bucket: StickyBucket | null,
  identityKey: string,
  obs: StickyObservation
): StickyBucket

/** Pure latest-only eligibility for a mounted card. */
export function isLatestStickyCard(
  card: {
    taskId?: string | null
    parentToolUseId?: string | null
    generation?: number | null
  },
  bucket: StickyBucket
): boolean
// Rule (first match wins):
// 1. If card.generation != null && bucket.activeGeneration != null:
//    card.generation === bucket.activeGeneration
// 2. Else if card.taskId && bucket.activeTaskId:
//    card.taskId === bucket.activeTaskId
// 3. Else if card.parentToolUseId && bucket.activeParentToolUseId:
//    card.parentToolUseId === bucket.activeParentToolUseId
// 4. Else false (never invent latest)
```

**Admission / active run update:** when observation type is `running` for a task, set active fields if `generation` is greater than current activeGeneration, or if generation is null and task is newly admitted after previous active completed, or parentToolUseId is the newest observed for this unit. Older terminal events update `taskMeta` + peaks only.

**Terminal error classification:**

| code / reason | phase |
|---|---|
| `parent_canceled`, `cancel_delegation`, usercancel, `canceled` (bare), `parent_ended` | terminal always |
| `parent_turn_failed`, `join_abandoned`, `parent_disconnected` | active_sticky iff `hasPositiveRecovery`, else terminal |
| completed ok / business failed | terminal |

**Elapsed:** active = `now - anchor`; terminal = `terminalElapsedMs` or `finishedAt - anchor`; anchor = min valid starts (may move earlier, never later).

### Store

```ts
export function observeSticky(
  input: {
    backendCacheKey: string
    parentConversationId?: number | null
    childConversationId?: number | null
    workUnitKey?: string | null
  } & StickyObservation
): { identityKey: string; bucket: StickyBucket } | null

export function getStickySnapshot(identityKey: string): StickyBucket | undefined
export function subscribeSticky(listener: () => void): () => void
export function resetStickyBackend(backendCacheKey: string): void
// On module load: registerBackendScopedStoreReset(() => reset all / full clear)
// Orphan timer: schedule when orphanStartedAtMs set; cancel when recovery returns
// or phase terminal; fire evaluate tick even if startedAt invalid
// Retention: bound terminal buckets (e.g. max 200 keys / LRU) — document constant
// Snapshots: immutable per version; unchanged getSnapshot referential stability
```

### Model fields

```ts
// DelegationCardModel additions:
showGeneratingSegment: boolean
stickyKey: string | null
// When showGeneratingSegment: lifecycleStatus AND badge status → running-equivalent
```

`backendCacheKey` on observe path from `getActiveBackendCacheKey()`.

---

### Task 1: Pure sticky runtime module

**Files:**
- Create: `src/lib/delegation-sticky-runtime.ts`
- Create: `src/lib/delegation-sticky-runtime.test.ts`

**Interfaces:** Produces all pure types/functions above except store.

- [ ] **Step 1: Failing tests** (include at least):

```ts
import { describe, expect, it } from "vitest"
import {
  resolveStickyIdentity,
  stickyIdentityToString,
  foldToolCount,
  applyStickyObservation,
  isLatestStickyCard,
  hasPositiveRecovery,
  STICKY_ORPHAN_TIMEOUT_MS,
} from "@/lib/delegation-sticky-runtime"

const noRecovery = {
  liveBindingRunning: false,
  childProjectionRunning: false,
  activeRunNonTerminal: false,
  openAttention: false,
  parentWaitingForThisChild: false,
  continueOrReplaceAdmitted: false,
}

describe("identity", () => {
  it("namespaces work_unit with backend+parent", () => {
    const id = resolveStickyIdentity({
      backendCacheKey: "local",
      parentConversationId: 10,
      workUnitKey: "task|1|implementer|grok|none",
      childConversationId: 20,
      taskId: "t1",
    })
    expect(id).toMatchObject({
      backendCacheKey: "local",
      parentConversationId: 10,
      unit: { kind: "work_unit", workUnitKey: "task|1|implementer|grok|none" },
    })
    const s = stickyIdentityToString(id!)
    expect(s).toContain("local")
    expect(s).toContain("10")
  })

  it("refuses bare work_unit without parent", () => {
    expect(
      resolveStickyIdentity({
        backendCacheKey: "local",
        workUnitKey: "task|1|implementer|grok|none",
        taskId: "t1",
      })
    ).toMatchObject({ kind: "task_only", taskId: "t1" })
  })

  it("isolates parallel children keys", () => {
    const a = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 2,
        taskId: "t1",
      })!
    )
    const b = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 3,
        taskId: "t1",
      })!
    )
    expect(a).not.toEqual(b)
  })
})

describe("foldToolCount", () => {
  it("A-B-A safe", () => {
    let s = { peakByTaskId: new Map<string, number>() }
    s = foldToolCount(s, "a", 5)
    expect(s.display).toBe(5)
    s = foldToolCount(s, "b", 2)
    expect(s.display).toBe(7)
    s = foldToolCount(s, "a", 5)
    expect(s.display).toBe(7)
  })
})

describe("phase", () => {
  it("parent_turn_failed keeps sticky only with recovery", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "canceled",
      taskId: "t1",
      errorCode: "parent_turn_failed",
      nowMs: 1000,
      recovery: { ...noRecovery, continueOrReplaceAdmitted: true },
    })
    expect(b.phase).toBe("active_sticky")
    b = applyStickyObservation(b, "k", {
      type: "canceled",
      taskId: "t1",
      errorCode: "parent_turn_failed",
      nowMs: 2000,
      recovery: noRecovery,
    })
    expect(b.phase).toBe("terminal")
  })

  it("parent_canceled always terminal", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "canceled",
      taskId: "t1",
      errorCode: "parent_canceled",
      nowMs: 1,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    expect(b.phase).toBe("terminal")
  })

  it("cancel_delegation / usercancel terminal", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "canceled",
      taskId: "t1",
      cancelReason: "usercancel",
      nowMs: 1,
      recovery: noRecovery,
    })
    expect(b.phase).toBe("terminal")
  })

  it("reseed with recovery keeps last display", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 4,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "reseed",
      taskId: "t1",
      nowMs: 500,
      recovery: { ...noRecovery, parentWaitingForThisChild: true },
    })
    expect(b.phase).toBe("active_sticky")
    expect(b.lastDisplayToolCount).toBe(4)
  })

  it("late old terminal does not kill newer active", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      generation: 1,
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "running",
      taskId: "t2",
      generation: 2,
      parentToolUseId: "p2",
      startedAt: "2026-07-27T00:02:00.000Z",
      toolCallCount: 1,
      nowMs: 120_000,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "completed",
      taskId: "t1",
      generation: 1,
      finishedAt: "2026-07-27T00:03:00.000Z",
      nowMs: 180_000,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    expect(b.phase).toBe("active_sticky")
    expect(b.activeTaskId).toBe("t2")
  })

  it("completed freezes terminal elapsed from anchor", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "completed",
      taskId: "t1",
      finishedAt: "2026-07-27T00:01:00.000Z",
      nowMs: 60_000,
      recovery: noRecovery,
    })
    expect(b.phase).toBe("terminal")
    expect(b.terminalElapsedMs).toBe(60_000)
  })

  it("orphan tick terminals after timeout without recovery", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "canceled",
      taskId: "t1",
      errorCode: "parent_turn_failed",
      nowMs: 1000,
      recovery: { ...noRecovery, continueOrReplaceAdmitted: true },
    })
    // lose recovery → orphan clock starts
    b = applyStickyObservation(b, "k", {
      type: "tick",
      taskId: "t1",
      nowMs: 1000,
      recovery: noRecovery,
    })
    expect(b.orphanStartedAtMs).toBe(1000)
    b = applyStickyObservation(b, "k", {
      type: "tick",
      taskId: "t1",
      nowMs: 1000 + STICKY_ORPHAN_TIMEOUT_MS,
      recovery: noRecovery,
    })
    expect(b.phase).toBe("terminal")
  })

  it("new child identity does not share bucket state (key differs)", () => {
    const k1 = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 2,
        taskId: "t1",
      })!
    )
    const k2 = stickyIdentityToString(
      resolveStickyIdentity({
        backendCacheKey: "local",
        parentConversationId: 1,
        childConversationId: 99,
        taskId: "t1",
      })!
    )
    expect(k1).not.toEqual(k2)
  })
})

describe("isLatestStickyCard", () => {
  it("only newer generation is latest", () => {
    let b = applyStickyObservation(null, "k", {
      type: "running",
      taskId: "t1",
      generation: 1,
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    b = applyStickyObservation(b, "k", {
      type: "running",
      taskId: "t2",
      generation: 2,
      parentToolUseId: "p2",
      startedAt: "2026-07-27T00:01:00.000Z",
      toolCallCount: 1,
      nowMs: 60_000,
      recovery: { ...noRecovery, liveBindingRunning: true },
    })
    expect(isLatestStickyCard({ taskId: "t1", parentToolUseId: "p1", generation: 1 }, b)).toBe(
      false
    )
    expect(isLatestStickyCard({ taskId: "t2", parentToolUseId: "p2", generation: 2 }, b)).toBe(
      true
    )
  })
})
```

- [ ] **Step 2:** `pnpm exec vitest run src/lib/delegation-sticky-runtime.test.ts` — FAIL
- [ ] **Step 3:** Implement pure module per locked contracts
- [ ] **Step 4:** PASS same command
- [ ] **Step 5:** Commit exact paths

```bash
git add src/lib/delegation-sticky-runtime.ts src/lib/delegation-sticky-runtime.test.ts
git commit -m "$( @'
feat(ui): add pure sticky runtime for delegation work units
'@ )"
```

---

### Task 2: Sticky external store + orphan timer + backend reset

**Files:**
- Create: `src/lib/delegation-sticky-store.ts`
- Create: `src/lib/delegation-sticky-store.test.ts`
- Modify: none in registry file (call `registerBackendScopedStoreReset` from sticky store module load)

**Interfaces:** Produces store API above; consumes Task 1.

- [ ] **Step 1: Failing tests**

```ts
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest"
import {
  observeSticky,
  getStickySnapshot,
  subscribeSticky,
  resetStickyBackend,
} from "@/lib/delegation-sticky-store"
import { stickyIdentityToString, resolveStickyIdentity } from "@/lib/delegation-sticky-runtime"
import { runBackendScopedStoreResets } from "@/stores/backend-scoped-store-reset"

const recoveryOn = {
  liveBindingRunning: true,
  childProjectionRunning: false,
  activeRunNonTerminal: true,
  openAttention: false,
  parentWaitingForThisChild: false,
  continueOrReplaceAdmitted: false,
}

describe("sticky store", () => {
  it("returns identityKey and notifies subscribers", () => {
    const spy = vi.fn()
    const unsub = subscribeSticky(spy)
    const result = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      parentToolUseId: "p1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })
    expect(result?.identityKey).toBeTruthy()
    expect(getStickySnapshot(result!.identityKey)?.phase).toBe("active_sticky")
    expect(spy).toHaveBeenCalled()
    unsub()
  })

  it("resetStickyBackend clears only that backend", () => {
    const a = observeSticky({
      backendCacheKey: "backend-a",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    const b = observeSticky({
      backendCacheKey: "backend-b",
      parentConversationId: 1,
      childConversationId: 2,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    resetStickyBackend("backend-a")
    expect(getStickySnapshot(a.identityKey)).toBeUndefined()
    expect(getStickySnapshot(b.identityKey)?.phase).toBe("active_sticky")
  })

  it("registers with backend-scoped store reset", () => {
    const seeded = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 9,
      childConversationId: 9,
      type: "running",
      taskId: "t9",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    runBackendScopedStoreResets()
    expect(getStickySnapshot(seeded.identityKey)).toBeUndefined()
  })

  it("orphan timer fires with fake timers even without valid startedAt later", () => {
    vi.useFakeTimers()
    // seed active_sticky with recovery, then observe canceled with recovery, then without
    // advance STICKY_ORPHAN_TIMEOUT_MS → phase terminal
    vi.useRealTimers()
  })

  it("getSnapshot referential stability when unchanged", () => {
    const r = observeSticky({
      backendCacheKey: "local",
      parentConversationId: 3,
      childConversationId: 4,
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
      recovery: recoveryOn,
    })!
    const s1 = getStickySnapshot(r.identityKey)
    const s2 = getStickySnapshot(r.identityKey)
    expect(s1).toBe(s2)
  })
})
```

- [ ] **Step 2:** FAIL run
- [ ] **Step 3:** Implement store + timer + `registerBackendScopedStoreReset`
- [ ] **Step 4:** PASS
- [ ] **Step 5:** Commit

```bash
git add src/lib/delegation-sticky-store.ts src/lib/delegation-sticky-store.test.ts
git commit -m "$( @'
feat(ui): add sticky runtime external store with orphan timer
'@ )"
```

---

### Task 3: Conversation interrupted detector

**Files:**
- Create: `src/lib/delegation-conversation-interrupted.ts`
- Create: `src/lib/delegation-conversation-interrupted.test.ts`

**Normalize grammar:** trim whitespace; accept full-string match of  
`*Conversation interrupted*` or `_Conversation interrupted_` (optional single emphasis wrapper only). Reject bare, bold multi-marker, multi-paragraph, partial.

```ts
export function isConversationInterruptedAgentText(text: string): boolean
```

- [ ] **Step 1–5:** TDD + commit as usual (tests: star, underscore, trim, reject bare/multi/partial).

```bash
git add src/lib/delegation-conversation-interrupted.ts src/lib/delegation-conversation-interrupted.test.ts
git commit -m "$( @'
feat(ui): detect Codex Conversation interrupted agent text
'@ )"
```

---

### Task 4: Merge sticky into card model (latest-only + badge)

**Files:**
- Modify: `src/hooks/use-delegation-card-model.ts`
- Modify: `src/hooks/use-delegation-card-model.test.ts`

**Producer of latest flag:** each card call site already has `parentToolUseId` / `taskId` / optional generation. Pure:

```ts
const isLatest = bucket ? isLatestStickyCard(cardIdentity, bucket) : false
const showGeneratingSegment = bucket?.phase === "active_sticky" && isLatest
```

Hook observes via `observeSticky` in effects only; `buildDelegationCardModel` remains pure and receives `stickyBucket` + card identity fields as inputs.

- [ ] **Step 1: Failing tests**

  - two cards same unit: only generation-2 / newer task gets `showGeneratingSegment`
  - recovery-owned `parent_turn_failed` on latest → generating + badge running
  - `parent_canceled` → not generating
  - peak sum tools
  - pure build does not call observe (spy)
  - attention open: showGenerating still true when sticky-active (badge attention separate)
  - backendCacheKey from `getActiveBackendCacheKey` wiring (mock)

- [ ] **Step 2–5:** implement, pass, commit

```bash
git add src/hooks/use-delegation-card-model.ts src/hooks/use-delegation-card-model.test.ts
git commit -m "$( @'
feat(ui): merge latest-only sticky projection into delegation card model
'@ )"
```

---

### Task 5: Chrome generating prefix + overlay model pass-through

**Files:**
- Modify: `src/components/message/delegation-card-chrome.tsx`
- Modify: `src/components/message/delegation-card-chrome.test.tsx`
- Modify: `src/components/message/delegated-sub-thread.tsx` (pass prop; **keep React key = toolCallId**)
- Modify: `src/components/chat/sub-agent-overlay.tsx`
- Modify: `src/components/chat/sub-agent-overlay.test.tsx` (create if missing; else extend)

- [ ] **Step 1:** Chrome failing test prefixes streaming label when `showGeneratingSegment`
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** Implement unshift `tLive("streaming")`; overlay consumes **same model fields only** (no second sticky identity)
- [ ] **Step 4:** PASS chrome + delegated-sub-thread + overlay tests
- [ ] **Step 5:** Commit exact paths only

```bash
git add src/components/message/delegation-card-chrome.tsx src/components/message/delegation-card-chrome.test.tsx src/components/message/delegated-sub-thread.tsx src/components/chat/sub-agent-overlay.tsx src/components/chat/sub-agent-overlay.test.tsx
git commit -m "$( @'
feat(ui): show continuous generating operational line on sticky cards
'@ )"
```

---

### Task 6: Suppress Conversation interrupted (both presentation paths)

**Files (locked — no discovery left to implementer):**
- Modify: `src/stores/live-transcript-store.ts` — live local materialization filter for **assistant** chunks when conversation is delegated (`parent_id != null` or live `isDelegationChild`)
- Modify: `src/stores/live-transcript-store.test.ts`
- Modify: `src/components/message/message-list-view.tsx` — historical render fallback hide matching assistant body
- Modify: existing message-list tests (or create `src/components/message/message-list-view.interrupt.test.tsx` if cleaner)

**Interfaces:** Consumes `isConversationInterruptedAgentText`.

- [ ] **Step 1: Failing tests (both paths required)**

  1. Live: delegated child + assistant marker → not stored/shown in live transcript
  2. Live: standalone → still shown
  3. Render: historical assistant marker hidden when delegated
  4. User-role identical text not suppressed
  5. “Response interrupted” footer / outcome chrome still present when fixtures include it

- [ ] **Step 2–5:** implement both paths; commit

```bash
git add src/stores/live-transcript-store.ts src/stores/live-transcript-store.test.ts src/components/message/message-list-view.tsx src/components/message/message-list-view.interrupt.test.tsx
git commit -m "$( @'
feat(ui): suppress Conversation interrupted text on delegated children
'@ )"
```

---

### Task 7: Verification gate (named tests only)

**Files:** only re-run + fix regressions in already-owned paths from Tasks 1–6; no open-ended “remaining polish” paths.

- [ ] **Step 1:** Run matrix commands:

```bash
pnpm exec tsc --noEmit --incremental false
pnpm exec vitest run src/lib/delegation-sticky-runtime.test.ts src/lib/delegation-sticky-store.test.ts src/lib/delegation-conversation-interrupted.test.ts src/hooks/use-delegation-card-model.test.ts src/components/message/delegation-card-chrome.test.tsx src/stores/live-transcript-store.test.ts
pnpm test
pnpm eslint src/lib/delegation-sticky-runtime.ts src/lib/delegation-sticky-store.ts src/lib/delegation-conversation-interrupted.ts src/hooks/use-delegation-card-model.ts src/components/message/delegation-card-chrome.tsx src/stores/live-transcript-store.ts src/components/message/message-list-view.tsx
```

- [ ] **Step 2:** Confirm design matrix 1–20 covered by Task 1–6 tests (document in `.superpowers/sdd/task-7-report.md`)
- [ ] **Step 3:** Commit only if regression fixes needed (exact paths)

---

## Design test matrix → plan

| # | Case | Task |
|---|------|------|
| 1 | Running + tools generating | 4, 5 |
| 2 | parent_turn_failed + recovery | 1, 4 |
| 3 | parent_turn_failed no recovery | 1 |
| 4 | New task_id; historical frozen; peak sum | 1, 4 |
| 5 | Reseed + recovery last frame | 1 |
| 6 | Completed elapsed freeze | 1, 4 |
| 7 | parent_canceled | 1, 4 |
| 8 | cancel_delegation / usercancel | 1 |
| 9 | Attention + ops line | 4 |
| 10 | Orphan fake clock | 1, 2 |
| 11 | Interrupt live+render; footer | 6 |
| 12 | Standalone + user-role | 3, 6 |
| 13 | Parallel children/parents/backends | 1, 2 |
| 14 | A-B-A tools | 1 |
| 15 | Late old terminal | 1 |
| 16 | Badge + lifecycle coerce | 4 |
| 17 | Overlay + inline isolation | 5 |
| 18 | Store subscribers / stability / reset | 2 |
| 19 | No sticky-started parent turn | constraint + Task 7 report |
| 20 | Checkpoint 600_000 untouched | constraint (no Rust edit) |

## Spec coverage checklist

| Spec requirement | Task |
|------------------|------|
| Namespaced sticky identity | 1, 2 |
| Latest-only via pure `isLatestStickyCard` | 1, 4 |
| Recovery-gated keep-sticky | 1, 4 |
| parent_canceled terminal | 1, 4 |
| peakByTaskId | 1, 4 |
| taskMeta + terminal elapsed | 1 |
| External store + orphan timer + backend reset | 2 |
| Badge + lifecycle coerce | 4 |
| Generating operational line | 5 |
| No stream React key coalescing | 5 |
| Interrupt both presentation paths | 3, 6 |
| Display-only / no Join | all |

## Placeholder / consistency self-review

- No discovery TBD left in Task 6; paths locked.
- `isLatestStickyCard` producer/consumer locked.
- Observation + bucket + store APIs complete.
- Types consistent across tasks.

## Execution handoff

Plan complete: `docs/superpowers/plans/2026-07-27-delegation-work-unit-sticky-runtime-ui.md`.

Execution: **Subagent-Driven Development** — Grok implementer + Codex reviewer per task (brainstorm-to-delivery).
