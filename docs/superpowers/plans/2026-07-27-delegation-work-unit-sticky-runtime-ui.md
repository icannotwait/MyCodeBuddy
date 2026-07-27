# Delegation Work-Unit Sticky Runtime UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep parent Codeg delegation cards showing continuous `生成中 | elapsed | N tool uses` for an entire sticky work unit across orchestration cancels, continue/replace, re-seed gaps, and continuation re-entry; suppress `*Conversation interrupted*` body text on delegated child sessions only.

**Architecture:** Add a pure frontend `StickyRuntimeStore` keyed by work-unit identity; fold live/meta/snapshot stats into unit-level elapsed and tool counts; merge sticky phase into `buildDelegationCardModel` so lifecycle stays running while sticky-active; prepend streaming label in `DelegationCardChrome`; suppress Codex interrupt markdown for conversations with `parent_id != null` at ingest and render.

**Tech Stack:** TypeScript strict, React 19, next-intl, Vitest, Testing Library, existing `useDelegationCardModel` / `DelegationCardChrome` / ACP runtime store.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-27-delegation-work-unit-sticky-runtime-ui-design.md`.
- Card baseline: `docs/superpowers/specs/2026-07-19-delegation-card-title-and-runtime-ui-design.md`.
- Continuation checkpoint is **600_000 ms** (`CONTINUATION_CHECKPOINT_MS`); this plan does **not** change it.
- Display-only: do **not** modify Broker Join ownership, Detach, or vendor `codex-acp` for V1.
- Never fabricate tool counts without observed stats.
- Sticky orphan timeout constant: **900_000 ms**.
- Localize only if new keys are required; prefer reusing `Folder.chat.liveTurnStats.streaming`, `toolUseCount`, elapsed keys (all ten locales already present for those).
- Prettier: no semicolons, trailing commas es5, 2-space indent, 80-char width.
- Preserve unrelated dirty worktree files. **Never** `git add -A`. Stage exact paths only.
- `docs/superpowers/**` may need `git add -f`.
- Before claiming done: `pnpm exec tsc --noEmit --incremental false`, targeted Vitest, then broader `pnpm test` for touched areas.

## File Map

| Path | Responsibility |
|------|----------------|
| `src/lib/delegation-sticky-runtime.ts` | Pure sticky key, phase, elapsed anchor, tool fold, orphan |
| `src/lib/delegation-sticky-runtime.test.ts` | Unit tests for sticky machine |
| `src/lib/delegation-conversation-interrupted.ts` | Normalize + detect `*Conversation interrupted*` text |
| `src/lib/delegation-conversation-interrupted.test.ts` | Match/reject cases |
| `src/hooks/use-delegation-card-model.ts` | Wire sticky into model; `showGeneratingSegment` |
| `src/hooks/use-delegation-card-model.test.ts` | Merge + sticky lifecycle cases |
| `src/components/message/delegation-card-chrome.tsx` | Prepend streaming segment when generating |
| `src/components/message/delegation-card-chrome.test.tsx` | Operational line with 生成中 / streaming |
| `src/components/message/delegated-sub-thread.tsx` | Stable React key by sticky key when available |
| `src/stores/conversation-runtime-store.ts` (or ACP ingest site) | Live suppress for delegated children |
| `src/components/message/message-list-view.tsx` (or parts renderer) | Render fallback hide |
| Matching `*.test.ts(x)` | Ingest/render suppress tests |

## Locked contracts

### Sticky key helper

```ts
export type StickyKey =
  | { kind: "work_unit"; key: string }
  | { kind: "parent_child"; parentId: number; childId: number }
  | { kind: "task"; taskId: string }

export function resolveStickyKey(input: {
  workUnitKey?: string | null
  parentConversationId?: number | null
  childConversationId?: number | null
  taskId?: string | null
}): StickyKey | null
```

### Tool fold

```ts
export type ToolFoldState = {
  lastTaskId: string | null
  base: number
  peakOfLastTask: number
}

export function foldToolCount(
  state: ToolFoldState,
  taskId: string,
  count: number
): ToolFoldState & { display: number }
```

### Phase

```ts
export type StickyPhase = "active_sticky" | "terminal"

export const ORCHESTRATION_CANCEL_ERROR_CODES: ReadonlySet<string> = new Set([
  "parent_turn_failed",
  "join_abandoned",
  "parent_disconnected",
  // parent_canceled: only when not explicit usercancel — see applyTerminal
])

export const STICKY_ORPHAN_TIMEOUT_MS = 900_000
```

### Model fields (additions)

```ts
// On DelegationCardModel:
showGeneratingSegment: boolean
stickyKey: string | null // stable string form for React key
```

---

### Task 1: Pure sticky runtime module

**Files:**
- Create: `src/lib/delegation-sticky-runtime.ts`
- Create: `src/lib/delegation-sticky-runtime.test.ts`

**Interfaces:**
- Produces: `resolveStickyKey`, `stickyKeyToString`, `foldToolCount`, `createStickyBucket`, `applyStickyObservation`, `STICKY_ORPHAN_TIMEOUT_MS`

- [ ] **Step 1: Write failing tests** for key resolution priority, tool fold across task ids, orchestration cancel keeps active, completed releases, orphan timeout with injected `now`.

```ts
import { describe, expect, it } from "vitest"
import {
  resolveStickyKey,
  foldToolCount,
  applyStickyObservation,
  STICKY_ORPHAN_TIMEOUT_MS,
} from "@/lib/delegation-sticky-runtime"

describe("resolveStickyKey", () => {
  it("prefers work_unit_key", () => {
    expect(
      resolveStickyKey({
        workUnitKey: "task|1|implementer|grok|none",
        parentConversationId: 1,
        childConversationId: 2,
        taskId: "t1",
      })
    ).toEqual({
      kind: "work_unit",
      key: "task|1|implementer|grok|none",
    })
  })

  it("falls back to parent_child then task", () => {
    expect(
      resolveStickyKey({
        parentConversationId: 1,
        childConversationId: 2,
        taskId: "t1",
      })
    ).toEqual({ kind: "parent_child", parentId: 1, childId: 2 })
    expect(resolveStickyKey({ taskId: "t1" })).toEqual({
      kind: "task",
      taskId: "t1",
    })
  })
})

describe("foldToolCount", () => {
  it("folds peak across task id change", () => {
    let s = { lastTaskId: null as string | null, base: 0, peakOfLastTask: 0 }
    s = foldToolCount(s, "a", 3)
    expect(s.display).toBe(3)
    s = foldToolCount(s, "a", 5)
    expect(s.display).toBe(5)
    s = foldToolCount(s, "b", 2)
    expect(s.display).toBe(7)
  })
})

describe("applyStickyObservation", () => {
  it("keeps active_sticky on parent_turn_failed", () => {
    const bucket = applyStickyObservation(null, {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
    })
    const next = applyStickyObservation(bucket, {
      type: "canceled",
      taskId: "t1",
      errorCode: "parent_turn_failed",
      nowMs: 1000,
    })
    expect(next.phase).toBe("active_sticky")
  })

  it("terminals on completed ok", () => {
    let b = applyStickyObservation(null, {
      type: "running",
      taskId: "t1",
      startedAt: "2026-07-27T00:00:00.000Z",
      toolCallCount: 1,
      nowMs: 0,
    })
    b = applyStickyObservation(b, {
      type: "completed",
      taskId: "t1",
      finishedAt: "2026-07-27T00:01:00.000Z",
      nowMs: 60_000,
    })
    expect(b.phase).toBe("terminal")
  })
})
```

- [ ] **Step 2: Run tests — expect FAIL** (module missing)

```bash
pnpm exec vitest run src/lib/delegation-sticky-runtime.test.ts
```

- [ ] **Step 3: Implement pure module** matching the locked contracts and spec § sticky key / phase / fold / orphan.

- [ ] **Step 4: Run tests — expect PASS**

```bash
pnpm exec vitest run src/lib/delegation-sticky-runtime.test.ts
```

- [ ] **Step 5: Commit**

```bash
git add src/lib/delegation-sticky-runtime.ts src/lib/delegation-sticky-runtime.test.ts
git commit -m "$( @'
feat(ui): add pure sticky runtime store for delegation work units
'@ )"
```

---

### Task 2: Conversation interrupted detector

**Files:**
- Create: `src/lib/delegation-conversation-interrupted.ts`
- Create: `src/lib/delegation-conversation-interrupted.test.ts`

**Interfaces:**
- Produces: `isConversationInterruptedAgentText(text: string): boolean`

- [ ] **Step 1: Failing tests**

```ts
import { describe, expect, it } from "vitest"
import { isConversationInterruptedAgentText } from "@/lib/delegation-conversation-interrupted"

describe("isConversationInterruptedAgentText", () => {
  it("matches exact italic marker", () => {
    expect(isConversationInterruptedAgentText("*Conversation interrupted*")).toBe(
      true
    )
    expect(
      isConversationInterruptedAgentText("  *Conversation interrupted* \n")
    ).toBe(true)
  })

  it("rejects partial or real content", () => {
    expect(
      isConversationInterruptedAgentText("Conversation interrupted")
    ).toBe(false)
    expect(
      isConversationInterruptedAgentText(
        "*Conversation interrupted*\n\nAlso more text"
      )
    ).toBe(false)
  })
})
```

- [ ] **Step 2: Run — expect FAIL**

```bash
pnpm exec vitest run src/lib/delegation-conversation-interrupted.test.ts
```

- [ ] **Step 3: Implement narrow normalize + equality** (trim; require full string match after trim).

- [ ] **Step 4: Run — expect PASS**

- [ ] **Step 5: Commit**

```bash
git add src/lib/delegation-conversation-interrupted.ts src/lib/delegation-conversation-interrupted.test.ts
git commit -m "$( @'
feat(ui): detect Codex Conversation interrupted agent text
'@ )"
```

---

### Task 3: Merge sticky into card model

**Files:**
- Modify: `src/hooks/use-delegation-card-model.ts`
- Modify: `src/hooks/use-delegation-card-model.test.ts`
- Modify: pure `buildDelegationCardModel` if it lives in the same file

**Interfaces:**
- Consumes: sticky runtime helpers from Task 1
- Produces: `DelegationCardModel.showGeneratingSegment`, `stickyKey`, sticky-adjusted `lifecycleStatus`, `elapsedMs`, `toolCallCount`

- [ ] **Step 1: Add failing pure tests** on `buildDelegationCardModel` (or exported test harness) for:
  - orchestration cancel → `lifecycleStatus === "running"` and `showGeneratingSegment === true`
  - tool fold across task ids
  - completed → not generating

- [ ] **Step 2: Run targeted hook tests — expect FAIL**

```bash
pnpm exec vitest run src/hooks/use-delegation-card-model.test.ts
```

- [ ] **Step 3: Implement**

  - Maintain a module-level or context-level map `Map<string, StickyBucket>` keyed by `stickyKeyToString` (prefer pure map updated inside `buildDelegationCardModel` inputs if possible; if React needed, store on `DelegationProvider` — prefer pure fold from props first).
  - Recommended V1: pure function `mergeStickyIntoCardModel(modelInput, stickyBucket) → { model fields, nextBucket }` called from `buildDelegationCardModel`.
  - When sticky active: force lifecycle running for ticker; `showGeneratingSegment = true`; elapsed from sticky anchor; tools from fold display.
  - When true terminal: `showGeneratingSegment = false`; normal terminal chrome.

- [ ] **Step 4: Run tests — expect PASS**

- [ ] **Step 5: Commit**

```bash
git add src/hooks/use-delegation-card-model.ts src/hooks/use-delegation-card-model.test.ts
git commit -m "$( @'
feat(ui): merge work-unit sticky projection into delegation card model
'@ )"
```

---

### Task 4: Chrome generating prefix + stable keys

**Files:**
- Modify: `src/components/message/delegation-card-chrome.tsx`
- Modify: `src/components/message/delegation-card-chrome.test.tsx`
- Modify: `src/components/message/delegated-sub-thread.tsx` (and overlay host if it sets React `key`)

**Interfaces:**
- Consumes: `showGeneratingSegment: boolean` on chrome props

- [ ] **Step 1: Failing chrome test**

```ts
it("prefixes streaming label when generating", () => {
  render(
    <DelegationCardChrome
      displaySecondary={null}
      elapsedMs={5000}
      toolCallCount={3}
      editRollup={{ mode: "omit" }}
      attentionRequest={null}
      runtimeStats={null}
      filesExpanded={false}
      onToggleFilesExpanded={() => {}}
      showGeneratingSegment
    />
  )
  const ops = screen.getByTestId("delegation-operational")
  expect(ops).toHaveTextContent(/生成中|Generating|streaming/i)
  expect(ops.textContent).toMatch(/\|/)
})
```

(Use the same locale harness other chrome tests use; assert against `tLive("streaming")` if the suite mocks next-intl with en `streaming`.)

- [ ] **Step 2: Run — expect FAIL**

```bash
pnpm exec vitest run src/components/message/delegation-card-chrome.test.tsx
```

- [ ] **Step 3: Implement** — when `showGeneratingSegment`, unshift `tLive("streaming")` into `operationalSegments`. Pass prop from `DelegatedSubThread` / overlay row from card model. Prefer React `key={model.stickyKey ?? parentToolUseId}` on stable hosts.

- [ ] **Step 4: Run chrome + delegated-sub-thread tests — PASS**

```bash
pnpm exec vitest run src/components/message/delegation-card-chrome.test.tsx src/components/message/delegated-sub-thread.test.tsx
```

- [ ] **Step 5: Commit**

```bash
git add src/components/message/delegation-card-chrome.tsx src/components/message/delegation-card-chrome.test.tsx src/components/message/delegated-sub-thread.tsx src/components/chat/sub-agent-overlay.tsx
git commit -m "$( @'
feat(ui): show continuous generating operational line on sticky cards
'@ )"
```

---

### Task 5: Suppress Conversation interrupted on delegated children

**Files:**
- Modify: ingest path in `src/stores/conversation-runtime-store.ts` and/or ACP message apply site (locate via search for agent text append / `recordAssistant` patterns)
- Modify: render path in `src/components/message/message-list-view.tsx` or content parts filter
- Create/modify matching tests

**Interfaces:**
- Consumes: `isConversationInterruptedAgentText`, conversation `parent_id`

- [ ] **Step 1: Find exact ingest function** with ripgrep:

```bash
# from repo root
rg -n "agent_message|appendAssistant|MessageChunk|session_update" src/stores src/contexts --glob "*.ts*"
```

Document the chosen function names in the PR description.

- [ ] **Step 2: Write failing tests**

  - When conversation has `parent_id: 1`, applying interrupt text does not add a visible text part (or is filtered at render).
  - When `parent_id: null`, text remains.

- [ ] **Step 3: Implement live drop + render fallback**

  - Live: if delegated and text matches detector → skip part append.
  - Render: if part text matches and conversation delegated → do not render body (footer may remain).

- [ ] **Step 4: Run tests — PASS**

- [ ] **Step 5: Commit**

```bash
git add src/lib/delegation-conversation-interrupted.ts src/stores/conversation-runtime-store.ts src/components/message/message-list-view.tsx # plus real paths/tests
git commit -m "$( @'
feat(ui): suppress Conversation interrupted text on delegated children
'@ )"
```

---

### Task 6: Integration polish + verification gate

**Files:**
- Adjust any remaining overlay paths from Tasks 3–5
- Docs only if acceptance notes needed (optional)

- [ ] **Step 1: Manual scenario checklist** (document results in commit body or `.superpowers/sdd` only if project requires; otherwise leave in PR notes)

  1. Parent delegates Codex long task; card shows generating | time | tools.
  2. Wait past nothing required if unit test covers cancel; simulate `parent_turn_failed` meta → still generating.
  3. Continue same child with new task_id → tools fold, no blank card.
  4. Open delegated child after interrupt → no `*Conversation interrupted*` body.
  5. Standalone Codex interrupt → text still visible.

- [ ] **Step 2: Typecheck**

```bash
pnpm exec tsc --noEmit --incremental false
```

Expected: clean

- [ ] **Step 3: Targeted tests**

```bash
pnpm exec vitest run src/lib/delegation-sticky-runtime.test.ts src/lib/delegation-conversation-interrupted.test.ts src/hooks/use-delegation-card-model.test.ts src/components/message/delegation-card-chrome.test.tsx
```

- [ ] **Step 4: Broader frontend gate**

```bash
pnpm test
pnpm eslint .
```

- [ ] **Step 5: Final commit** only if polish diffs remain

```bash
git add <exact paths>
git commit -m "$( @'
test(ui): cover sticky delegation runtime and interrupt suppress
'@ )"
```

---

## Spec coverage checklist

| Spec requirement | Task |
|------------------|------|
| Sticky key priority | Task 1 |
| active_sticky on orchestration cancel | Task 1, 3 |
| Elapsed from unit anchor | Task 1, 3 |
| Tool fold across task_id | Task 1, 3 |
| Generating operational line | Task 4 |
| Stable React key | Task 4 |
| Orphan timeout 15m | Task 1 |
| Suppress interrupt on delegated child | Task 2, 5 |
| Standalone unchanged | Task 5 tests |
| No Join/Broker change | All tasks (constraint) |
| 600s checkpoint unchanged | Out of scope (already shipped) |

## Placeholder / consistency self-review

- No TBD steps; concrete files and test sketches included.
- Types `StickyKey`, `ToolFoldState`, `showGeneratingSegment` consistent across tasks.
- Chrome prop name matches model field.

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-delegation-work-unit-sticky-runtime-ui.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session with executing-plans checkpoints  

Which approach?
