# View Sub-Agent as Tab + MRU Close Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open delegated child transcripts via main-window tabs (`openTab`) instead of `SubAgentSessionDialog`, and make every active-tab close (× / middle-click / Esc / `closeConversationTab`) activate the MRU remaining tab.

**Architecture:** Extract shared `pickMruTabId` used by `closeTab` and `detachTab`. Replace dialog mounts in `DelegatedSubThread` and `SubAgentOverlay` with a small resolver + `openTab`. Extend `TabBar` keyboard handling so Escape also closes the active conversation tab (alongside existing `close_current_tab`, default `mod+w`), reusing the same `closeTab` path.

**Tech Stack:** TypeScript, React 19, Zustand tab store (`src/stores/tab-store.ts`), Vitest + Testing Library, next-intl.

**Spec:** `docs/superpowers/specs/2026-07-22-view-subagent-as-tab-mru-close-design.md`

## Global Constraints

- Conversation tabs only; do not change file-workspace or terminal tab close semantics.
- Do not keep `SubAgentSessionDialog` as a fallback once entry points are wired.
- `closeOtherTabs` / `closeAllTabs` / `closeTabsByFolder` stay as today (not MRU successor selection).
- `openTab` preview/pin/detached-focus rules stay unchanged.
- Esc is fixed additional close key (not a new shortcut-settings entry this round); keep existing `close_current_tab` binding (`mod+w` by default).
- Prefer `git add -f` when committing under `docs/superpowers/` (directory is gitignored).

## File map

| File | Responsibility |
|------|----------------|
| `src/stores/tab-store.ts` | `pickMruTabId`, `closeTab` MRU successor |
| `src/stores/tab-store-popout.test.ts` (or new `tab-store-close-mru.test.ts`) | MRU close + detach regression |
| `src/lib/open-delegated-child-session.ts` | Resolve folder/agent + call `openTab` |
| `src/lib/open-delegated-child-session.test.ts` | Resolver unit tests |
| `src/components/message/delegated-sub-thread.tsx` | Open tab, drop dialog |
| `src/components/message/delegated-sub-thread.test.tsx` | Assert `openTab`, no dialog |
| `src/components/chat/sub-agent-overlay.tsx` | Same as inline card |
| `src/components/chat/sub-agent-overlay.test.tsx` | Same assertions |
| `src/components/tabs/tab-bar.tsx` | Escape → `closeTab(active)` |
| `src/components/message/sub-agent-session-dialog.tsx` | Delete if unreferenced |
| Dialog-only tests (if any) | Delete or rewrite if dialog file removed |

---

### Task 1: MRU successor on `closeTab`

**Files:**
- Modify: `src/stores/tab-store.ts` (helpers near `stampActiveTab`; `closeTab`; `detachTab`)
- Test: `src/stores/tab-store-popout.test.ts` (extend) **or** Create: `src/stores/tab-store-close-mru.test.ts`

**Interfaces:**
- Consumes: `TabItemInternal.activationSeq?: number`, existing `stampActiveTab`, `recomputeTabs`
- Produces:
  ```ts
  /** Highest activationSeq wins. Returns null when no tab has seq > 0. */
  export function pickMruTabId(
    tabs: ReadonlyArray<{ id: string; activationSeq?: number }>
  ): string | null
  ```

- [ ] **Step 1: Write the failing tests**

Add to a tab-store test file (mirror mocks from `tab-store-popout.test.ts` if creating a new file — `resetTabStore`, api/platform/popout mocks).

```ts
describe("closeTab MRU", () => {
  beforeEach(() => {
    resetTabStore()
    useTabStore.setState({
      rawTabs: [
        {
          id: "parent",
          kind: "conversation",
          folderId: 1,
          conversationId: 10,
          agentType: "claude_code",
          title: "Parent",
          isPinned: false,
          activationSeq: 5,
        },
        {
          id: "other",
          kind: "conversation",
          folderId: 1,
          conversationId: 11,
          agentType: "claude_code",
          title: "Other",
          isPinned: false,
          activationSeq: 1,
        },
        {
          id: "child",
          kind: "conversation",
          folderId: 1,
          conversationId: 99,
          agentType: "codex",
          title: "Child",
          isPinned: false,
          activationSeq: 9,
        },
      ],
      activeTabId: "child",
      tabsHydrated: true,
    })
  })

  it("activates highest activationSeq among remaining when closing active tab", () => {
    useTabStore.getState().closeTab("child")
    expect(useTabStore.getState().activeTabId).toBe("parent")
    expect(useTabStore.getState().rawTabs.map((t) => t.id)).toEqual([
      "parent",
      "other",
    ])
  })

  it("falls back to neighbor index when no remaining tab has seq > 0", () => {
    useTabStore.setState({
      rawTabs: [
        {
          id: "a",
          kind: "conversation",
          folderId: 1,
          conversationId: 1,
          agentType: "claude_code",
          title: "A",
          isPinned: false,
        },
        {
          id: "b",
          kind: "conversation",
          folderId: 1,
          conversationId: 2,
          agentType: "claude_code",
          title: "B",
          isPinned: false,
        },
        {
          id: "c",
          kind: "conversation",
          folderId: 1,
          conversationId: 3,
          agentType: "claude_code",
          title: "C",
          isPinned: false,
        },
      ],
      activeTabId: "b",
      tabsHydrated: true,
    })
    useTabStore.getState().closeTab("b")
    // index was 1; next = Math.min(1, 1) → second of remaining ["a","c"] → "c"
    expect(useTabStore.getState().activeTabId).toBe("c")
  })

  it("does not change activeTabId when closing a non-active tab", () => {
    useTabStore.getState().closeTab("other")
    expect(useTabStore.getState().activeTabId).toBe("child")
    expect(useTabStore.getState().rawTabs.map((t) => t.id)).toEqual([
      "parent",
      "child",
    ])
  })
})

describe("pickMruTabId", () => {
  it("returns null when all seqs missing or zero", () => {
    expect(
      pickMruTabId([
        { id: "a" },
        { id: "b", activationSeq: 0 },
      ])
    ).toBeNull()
  })

  it("returns id with highest seq", () => {
    expect(
      pickMruTabId([
        { id: "a", activationSeq: 2 },
        { id: "b", activationSeq: 7 },
        { id: "c", activationSeq: 3 },
      ])
    ).toBe("b")
  })
})
```

Keep existing `detachTab` MRU test; after helper extraction it must still pass.

- [ ] **Step 2: Run tests to verify they fail**

```powershell
pnpm exec vitest run src/stores/tab-store-close-mru.test.ts
# or the extended tab-store-popout.test.ts file path
```

Expected: FAIL — `pickMruTabId` not exported / `closeTab` still picks neighbor (active would be `"other"` or neighbor, not `"parent"`).

- [ ] **Step 3: Implement `pickMruTabId` and wire `closeTab` / `detachTab`**

In `src/stores/tab-store.ts`, near `stampActiveTab`:

```ts
/**
 * MRU among tabs with a positive activationSeq. Returns null when none
 * qualify so callers can apply neighbor-index fallback.
 */
export function pickMruTabId(
  tabs: ReadonlyArray<{ id: string; activationSeq?: number }>
): string | null {
  let bestId: string | null = null
  let bestSeq = 0
  for (const t of tabs) {
    const seq = t.activationSeq ?? 0
    if (seq > bestSeq) {
      bestSeq = seq
      bestId = t.id
    }
  }
  return bestId
}
```

Replace the active-close branch in `closeTab` (currently neighbor-only):

```ts
} else if (tabId === prevState.activeTabId) {
  const mruId = pickMruTabId(next)
  const nextActive =
    mruId ??
    next[Math.min(index, next.length - 1)]?.id ??
    next[0]?.id
  set({
    rawTabs: stampActiveTab(next, nextActive),
    activeTabId: nextActive,
  })
}
```

Refactor `detachTab` loop to:

```ts
const mruId = pickMruTabId(next)
let nextActive = mruId
if (!nextActive) {
  const neighborIndex = Math.min(index, next.length - 1)
  nextActive = next[neighborIndex]?.id ?? next[0]?.id ?? null
}
```

Preserve empty-tab / replacement-draft branches unchanged.

- [ ] **Step 4: Run tests to verify they pass**

```powershell
pnpm exec vitest run src/stores/tab-store-close-mru.test.ts src/stores/tab-store-popout.test.ts
```

Expected: PASS (including existing detachTab cases).

- [ ] **Step 5: Commit**

```powershell
git add src/stores/tab-store.ts src/stores/tab-store-close-mru.test.ts src/stores/tab-store-popout.test.ts
git commit -m "fix(tabs): pick next active tab by MRU on close"
```

---

### Task 2: Resolve + open delegated child session helper

**Files:**
- Create: `src/lib/open-delegated-child-session.ts`
- Create: `src/lib/open-delegated-child-session.test.ts`

**Interfaces:**
- Consumes: `useAppWorkspaceStore.getState().conversations`, `useTabStore.getState()` (`rawTabs`, `activeTabId`, `openTab`), `AgentType`
- Produces:
  ```ts
  export type DelegatedChildOpenTarget = {
    folderId: number
    conversationId: number
    agentType: AgentType
    title?: string
  }

  export function resolveDelegatedChildOpenTarget(input: {
    childConversationId: number | null | undefined
    agentType: AgentType | null | undefined
    title?: string | null
  }): DelegatedChildOpenTarget | null

  /** Opens/focuses the child tab. Returns openTab's boolean (false = detached focused). */
  export async function openDelegatedChildSession(input: {
    childConversationId: number | null | undefined
    agentType: AgentType | null | undefined
    title?: string | null
  }): Promise<boolean>
  ```

- [ ] **Step 1: Write the failing tests**

```ts
import { beforeEach, describe, expect, it, vi } from "vitest"
import { resetTabStore, useTabStore } from "@/stores/tab-store"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import {
  openDelegatedChildSession,
  resolveDelegatedChildOpenTarget,
} from "@/lib/open-delegated-child-session"

// Reuse the same light mocks as tab-store tests if openTab hits popout imports.

describe("resolveDelegatedChildOpenTarget", () => {
  beforeEach(() => {
    resetTabStore()
    useAppWorkspaceStore.setState({
      conversations: [
        {
          id: 99,
          folder_id: 7,
          title: "Child title",
          title_locked: false,
          auto_title_finalized: false,
          agent_type: "codex",
          status: "active",
          awaiting_reply_token: null,
          kind: "delegate",
          model: null,
          git_branch: null,
          external_id: null,
          message_count: 0,
          child_count: 0,
          created_at: "",
          updated_at: "",
          pinned_at: null,
        },
      ],
    } as never)
  })

  it("prefers workspace list folder_id", () => {
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
      })
    ).toEqual({
      folderId: 7,
      conversationId: 99,
      agentType: "codex",
      title: "Child title",
    })
  })

  it("falls back to active tab folderId when child is absent from list", () => {
    useAppWorkspaceStore.setState({ conversations: [] } as never)
    useTabStore.setState({
      rawTabs: [
        {
          id: "p",
          kind: "conversation",
          folderId: 3,
          conversationId: 10,
          agentType: "claude_code",
          title: "Parent",
          isPinned: false,
          activationSeq: 1,
        },
      ],
      activeTabId: "p",
      tabsHydrated: true,
    })
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
        title: "Kickoff",
      })
    ).toEqual({
      folderId: 3,
      conversationId: 99,
      agentType: "codex",
      title: "Kickoff",
    })
  })

  it("returns null when id, agentType, or folderId missing", () => {
    useAppWorkspaceStore.setState({ conversations: [] } as never)
    useTabStore.setState({ rawTabs: [], activeTabId: null })
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: "codex",
      })
    ).toBeNull()
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: null,
        agentType: "codex",
      })
    ).toBeNull()
    expect(
      resolveDelegatedChildOpenTarget({
        childConversationId: 99,
        agentType: null,
      })
    ).toBeNull()
  })
})

describe("openDelegatedChildSession", () => {
  it("calls openTab with resolved target", async () => {
    const openTab = vi.fn(async () => true)
    useTabStore.setState({ openTab } as never)
    // Ensure resolve succeeds via list
    useAppWorkspaceStore.setState({
      conversations: [
        {
          id: 99,
          folder_id: 7,
          title: "Child title",
          // ...same required summary fields as above...
          title_locked: false,
          auto_title_finalized: false,
          agent_type: "codex",
          status: "active",
          awaiting_reply_token: null,
          kind: "delegate",
          model: null,
          git_branch: null,
          external_id: null,
          message_count: 0,
          child_count: 0,
          created_at: "",
          updated_at: "",
          pinned_at: null,
        },
      ],
    } as never)

    // Prefer testing via real store.openTab after resolve unit tests,
    // or spy: const spy = vi.spyOn(useTabStore.getState(), "openTab")
    // Implementation should call useTabStore.getState().openTab(...)
  })
})
```

If spying `openTab` on the real store is awkward, unit-test only `resolveDelegatedChildOpenTarget` thoroughly and assert `openDelegatedChildSession` returns `false`/`does nothing` when resolve is null; integration is covered in Task 3 UI tests with mocked `useTabActions`.

Minimal `openDelegatedChildSession` test:

```ts
it("no-ops when resolve fails", async () => {
  useAppWorkspaceStore.setState({ conversations: [] } as never)
  useTabStore.setState({ rawTabs: [], activeTabId: null })
  await expect(
    openDelegatedChildSession({
      childConversationId: 1,
      agentType: null,
    })
  ).resolves.toBe(false)
})
```

- [ ] **Step 2: Run tests — expect FAIL**

```powershell
pnpm exec vitest run src/lib/open-delegated-child-session.test.ts
```

- [ ] **Step 3: Implement helper**

`src/lib/open-delegated-child-session.ts`:

```ts
import type { AgentType } from "@/lib/types"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabStore } from "@/stores/tab-store"

export type DelegatedChildOpenTarget = {
  folderId: number
  conversationId: number
  agentType: AgentType
  title?: string
}

export function resolveDelegatedChildOpenTarget(input: {
  childConversationId: number | null | undefined
  agentType: AgentType | null | undefined
  title?: string | null
}): DelegatedChildOpenTarget | null {
  const conversationId = input.childConversationId
  const agentType = input.agentType
  if (conversationId == null || conversationId <= 0 || !agentType) {
    return null
  }

  const summary = useAppWorkspaceStore
    .getState()
    .conversations.find((c) => c.id === conversationId)

  let folderId = summary?.folder_id
  if (folderId == null || folderId <= 0) {
    const tabState = useTabStore.getState()
    const active = tabState.rawTabs.find((t) => t.id === tabState.activeTabId)
    folderId = active?.folderId
  }
  if (folderId == null || folderId <= 0) return null

  const title =
    (summary?.title && summary.title.trim()) ||
    (input.title && input.title.trim()) ||
    undefined

  return { folderId, conversationId, agentType, title }
}

export async function openDelegatedChildSession(input: {
  childConversationId: number | null | undefined
  agentType: AgentType | null | undefined
  title?: string | null
}): Promise<boolean> {
  const target = resolveDelegatedChildOpenTarget(input)
  if (!target) return false
  return useTabStore
    .getState()
    .openTab(
      target.folderId,
      target.conversationId,
      target.agentType,
      false,
      target.title
    )
}
```

- [ ] **Step 4: Run tests — expect PASS**

```powershell
pnpm exec vitest run src/lib/open-delegated-child-session.test.ts
```

- [ ] **Step 5: Commit**

```powershell
git add src/lib/open-delegated-child-session.ts src/lib/open-delegated-child-session.test.ts
git commit -m "feat: resolve and open delegated child as tab"
```

---

### Task 3: Wire「查看会话」on card + overlay; drop dialog mounts

**Files:**
- Modify: `src/components/message/delegated-sub-thread.tsx`
- Modify: `src/components/message/delegated-sub-thread.test.tsx`
- Modify: `src/components/chat/sub-agent-overlay.tsx`
- Modify: `src/components/chat/sub-agent-overlay.test.tsx`

**Interfaces:**
- Consumes: `openDelegatedChildSession` from Task 2; optional `useWorkbenchRoute().openConversations` like sidebar
- Produces: click openDetail → tab open; no `SubAgentSessionDialog` in tree

- [ ] **Step 1: Update failing tests first (TDD for UI contract)**

In `delegated-sub-thread.test.tsx`:

1. Remove `vi.mock("@/components/message/sub-agent-session-dialog", ...)`.
2. Mock open helper:

```ts
const openDelegatedChildSession = vi.fn(async () => true)
vi.mock("@/lib/open-delegated-child-session", () => ({
  openDelegatedChildSession: (...args: unknown[]) =>
    openDelegatedChildSession(...args),
}))
```

3. Replace dialog toggle test with:

```ts
it("opens the child conversation tab when open-conversation is clicked", async () => {
  openDelegatedChildSession.mockClear()
  mockedHook.mockReturnValue({
    binding: bindingOf({ status: "running" }),
    detail: null,
    loading: false,
    error: null,
  })
  renderWithIntl(<DelegatedSubThread parentToolUseId="pt-1" />)
  fireEvent.click(screen.getByRole("button", { name: "Open conversation" }))
  expect(openDelegatedChildSession).toHaveBeenCalledWith(
    expect.objectContaining({
      childConversationId: 99,
      agentType: "codex",
    })
  )
  expect(
    screen.queryByTestId("sub-agent-session-dialog")
  ).not.toBeInTheDocument()
})
```

Apply the same pattern in `sub-agent-overlay.test.tsx` for `data-testid="sub-agent-open"` (pass `agentType` / `childConversationId` from the row fixture).

- [ ] **Step 2: Run tests — expect FAIL**

```powershell
pnpm exec vitest run src/components/message/delegated-sub-thread.test.tsx src/components/chat/sub-agent-overlay.test.tsx
```

Expected: FAIL — still opens dialog / mock not called.

- [ ] **Step 3: Implement component wiring**

`delegated-sub-thread.tsx`:

- Remove `SubAgentSessionDialog` import and JSX.
- Remove `dialogOpen` state.
- On open button click:

```ts
const onOpenChild = useCallback(() => {
  void openDelegatedChildSession({
    childConversationId,
    agentType,
    title: conversationTitle ?? task,
  }).then((openedMain) => {
    if (openedMain) {
      // If workbench route is available in this tree, switch to conversations pane:
      // openConversations() — same as sidebar handleSelect.
    }
  })
}, [childConversationId, agentType, conversationTitle, task])
```

Prefer importing `useWorkbenchRoute` if already used nearby in chat shell; if that creates heavy coupling, call only `openDelegatedChildSession` (tabs still activate; user may already be on conversations pane when viewing parent). **Minimum required by spec:** `openTab` path. If `useWorkbenchRoute` is one-liner and used by sidebar, mirror it:

```ts
import { useWorkbenchRoute } from "@/hooks/use-workbench-route" // verify actual path
// ...
const { openConversations } = useWorkbenchRoute()
```

Check import path with grep before coding (`openConversations` definition).

Same for `SubAgentOverlayRow` in `sub-agent-overlay.tsx`: remove dialog state/import; `onClick` → `openDelegatedChildSession({ childConversationId, agentType, title: conversationTitle ?? task })`.

Update file header comments that mention `SubAgentSessionDialog`.

- [ ] **Step 4: Run tests — expect PASS**

```powershell
pnpm exec vitest run src/components/message/delegated-sub-thread.test.tsx src/components/chat/sub-agent-overlay.test.tsx
```

- [ ] **Step 5: Commit**

```powershell
git add src/components/message/delegated-sub-thread.tsx src/components/message/delegated-sub-thread.test.tsx src/components/chat/sub-agent-overlay.tsx src/components/chat/sub-agent-overlay.test.tsx
git commit -m "feat: open sub-agent session in main tab instead of dialog"
```

---

### Task 4: Escape closes active conversation tab

**Files:**
- Modify: `src/components/tabs/tab-bar.tsx` (existing `keydown` effect ~lines 106–147)
- Test: Create `src/components/tabs/tab-bar-escape-close.test.tsx` **or** extend an existing tab-bar test if present

**Interfaces:**
- Consumes: existing `closeTab`, `activeTabId`, `shouldHandleShortcut` gates, `matchShortcutEvent`
- Produces: bare Escape (no modifiers) closes active tab when conversation shortcuts apply and event is not already handled

**Note:** `close_current_tab` default remains `mod+w`. Spec asks for Esc as an additional fixed path, not a settings migration.

- [ ] **Step 1: Write / extend test**

If TabBar is heavy to mount, test a tiny extracted predicate instead:

```ts
// src/lib/should-close-tab-on-escape.ts
export function shouldCloseTabOnEscape(event: {
  key: string
  defaultPrevented: boolean
  metaKey: boolean
  ctrlKey: boolean
  altKey: boolean
  shiftKey: boolean
}): boolean {
  if (event.defaultPrevented) return false
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) {
    return false
  }
  return event.key === "Escape" || event.key === "Esc"
}
```

Unit test that pure helper; in TabBar call it then `closeTab`.

Alternatively mount TabBar with mocks (match existing component test patterns). Prefer the pure helper if TabBar test setup is large — still wire the real close path in TabBar.

```ts
describe("shouldCloseTabOnEscape", () => {
  it("accepts bare Escape", () => {
    expect(
      shouldCloseTabOnEscape({
        key: "Escape",
        defaultPrevented: false,
        metaKey: false,
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
      })
    ).toBe(true)
  })
  it("rejects when defaultPrevented or modifiers", () => {
    expect(
      shouldCloseTabOnEscape({
        key: "Escape",
        defaultPrevented: true,
        metaKey: false,
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
      })
    ).toBe(false)
  })
})
```

- [ ] **Step 2: Run test — FAIL** (module missing)

- [ ] **Step 3: Implement helper + TabBar branch**

After the existing `close_current_tab` match block (or merged into one “should close” condition):

```ts
const isConfiguredClose = matchShortcutEvent(
  event,
  shortcuts.close_current_tab
)
const isEscapeClose = shouldCloseTabOnEscape(event)
if (!isConfiguredClose && !isEscapeClose) return
if (!activeTabId) return
// Optional: skip when a modal dialog is open
if (
  typeof document !== "undefined" &&
  document.querySelector('[role="dialog"][data-state="open"]')
) {
  return
}
event.preventDefault()
closeTab(activeTabId)
```

Keep the same `shouldHandleShortcut` mode/pane guards as next/prev/close.

- [ ] **Step 4: Run tests**

```powershell
pnpm exec vitest run src/lib/should-close-tab-on-escape.test.ts
# plus any tab-bar test file touched
```

Manual smoke (implementer): parent tab → open child via card → Esc → parent active.

- [ ] **Step 5: Commit**

```powershell
git add src/lib/should-close-tab-on-escape.ts src/lib/should-close-tab-on-escape.test.ts src/components/tabs/tab-bar.tsx
git commit -m "feat(tabs): Escape closes active conversation tab"
```

---

### Task 5: Remove dead `SubAgentSessionDialog` path

**Files:**
- Delete (if unreferenced): `src/components/message/sub-agent-session-dialog.tsx` and any `sub-agent-session-dialog.test.tsx`
- Modify: comments in `src/stores/conversation-runtime-store.ts` that name the dialog (update to “child conversation tab” / remove stale pointer)
- Grep-clean remaining imports

- [ ] **Step 1: Confirm no remaining production imports**

```powershell
pnpm exec rg "SubAgentSessionDialog|sub-agent-session-dialog" --glob "*.{ts,tsx}"
```

Expected: only comments / this plan references, or only the file itself.

- [ ] **Step 2: Delete unused module + tests; fix comments**

- [ ] **Step 3: Run focused regression suite**

```powershell
pnpm exec vitest run src/stores/tab-store-close-mru.test.ts src/stores/tab-store-popout.test.ts src/lib/open-delegated-child-session.test.ts src/components/message/delegated-sub-thread.test.tsx src/components/chat/sub-agent-overlay.test.tsx
```

Expected: PASS

- [ ] **Step 4: Commit**

```powershell
git add -A src/components/message/sub-agent-session-dialog.tsx src/components/message src/stores/conversation-runtime-store.ts
git commit -m "chore: remove SubAgentSessionDialog after tab open path"
```

---

## Spec coverage checklist

| Spec requirement | Task |
|------------------|------|
| 查看会话 → openTab focus/open | 2, 3 |
| folderId list → active tab → no-op | 2 |
| No dialog mount | 3, 5 |
| Detached focus via existing openTab | 2 (delegates to openTab) |
| Esc closes active tab | 4 |
| closeTab MRU for × / middle-click / closeConversationTab | 1 |
| Shared helper with detachTab | 1 |
| closeOther/All/ByFolder unchanged | 1 (untouched) |
| Tests for MRU, open entries, Esc | 1–4 |

## Self-review notes

- No TBD placeholders; Esc is dual-path with `mod+w`, not a silent default overwrite.
- `pickMruTabId` treats seq `> 0` as qualified (matches current detach `bestSeq <= 0` fallback).
- Neighbor fallback for close when no seqs: same formula as pre-change closeTab (`Math.min(index, next.length - 1)`).
- `openConversations` after open is recommended when the hook is cheap to import; not required for correctness of tab activation.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-23-view-subagent-as-tab-mru-close.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — execute tasks in this session with checkpoints  

Which approach?
