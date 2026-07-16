# Uncached Conversation Initial Scroll Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a newly mounted persisted conversation appear at the bottom immediately after its first successful history render, while cached tabs and manual reloads preserve their current scroll.

**Architecture:** `ConversationTabView` freezes mount-time eligibility, `MessageListView` owns a one-shot pending latch, and a new controller inside `MessageThread` owns instant placement, dimension stabilization, and user-cancel listeners through the existing `use-stick-to-bottom` context. The controller is separate from `VirtualizedMessageThread`, preserving the current live-footer coordinator and avoiding overlap with work already in progress there. A delegated sub-agent dialog opts in on each body mount because closing it discards that uncached view.

**Tech Stack:** React 19, TypeScript, use-stick-to-bottom, Virtua, Vitest, Testing Library, requestAnimationFrame DOM measurements.

## Global Constraints

- Eligibility is captured only when a view mounts already bound to a persisted conversation.
- Binding a still-mounted draft after its first send never enables the latch.
- Cached tab activation changes and manual detail reloads never reset the latch.
- Closing and reopening a tab creates a new uncached view and runs initialization again.
- While pending, history resize behavior is `instant`; after completion it returns to existing history `smooth` behavior, while live transcript resize remains `instant`.
- Initial placement and the final correction both use instant scrolling.
- Completion requires unchanged content height and viewport `scrollHeight` for two consecutive animation frames.
- Wheel, touch, transcript pointer, `PageUp`, `Home`, or `ArrowUp` cancels immediately and calls `stopScroll`.
- A failed history load leaves the latch pending so a later successful retry can initialize it.
- Completion, cancellation, and unmount dispose all listeners and pending animation frames.
- Do not modify `src/components/message/virtualized-message-thread.tsx` for this feature.
- Preserve unrelated worktree changes and stage only files named by each task.

---

### Task 1: Build the Mount Eligibility Hook and Scroll Controller

**Files:**
- Create: `src/components/message/initial-history-scroll-controller.tsx`
- Create: `src/components/message/initial-history-scroll-controller.test.tsx`

**Interfaces:**
- Produces: `useInitialHistoryScrollEligibility(conversationId: number | null): boolean`.
- Produces: `InitialHistoryScrollController({ pending, historyReady, hasHistoryRows, onFinish }): ReactNode`.
- Consumes: `useStickToBottomContext()` fields `contentRef`, `scrollRef`, `scrollToBottom`, and `stopScroll`.

- [ ] **Step 1: Write failing eligibility and controller tests**

Create `initial-history-scroll-controller.test.tsx`:

```tsx
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
} from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  scrollToBottom: vi.fn(),
  stopScroll: vi.fn(),
}))

let scrollElement: HTMLDivElement
let contentElement: HTMLDivElement
let scrollHeight = 0
let contentHeight = 0

vi.mock("use-stick-to-bottom", () => ({
  useStickToBottomContext: () => ({
    scrollRef: { current: scrollElement },
    contentRef: { current: contentElement },
    scrollToBottom: mocks.scrollToBottom,
    stopScroll: mocks.stopScroll,
  }),
}))

import {
  InitialHistoryScrollController,
  useInitialHistoryScrollEligibility,
} from "./initial-history-scroll-controller"

let nextFrameId = 1
let frames = new Map<number, FrameRequestCallback>()

function flushNextFrame(): void {
  const entry = frames.entries().next().value as
    | [number, FrameRequestCallback]
    | undefined
  if (!entry) throw new Error("expected a queued animation frame")
  frames.delete(entry[0])
  act(() => entry[1](0))
}

beforeEach(() => {
  scrollElement = document.createElement("div")
  contentElement = document.createElement("div")
  scrollHeight = 500
  contentHeight = 100
  Object.defineProperty(scrollElement, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  })
  vi.spyOn(contentElement, "getBoundingClientRect").mockImplementation(
    () =>
      ({
        x: 0,
        y: 0,
        top: 0,
        right: 0,
        bottom: contentHeight,
        left: 0,
        width: 0,
        height: contentHeight,
        toJSON: () => ({}),
      }) as DOMRect
  )
  nextFrameId = 1
  frames = new Map()
  mocks.scrollToBottom.mockReset()
  mocks.stopScroll.mockReset()
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    const id = nextFrameId
    nextFrameId += 1
    frames.set(id, callback)
    return id
  })
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
    frames.delete(id)
  })
})

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

describe("useInitialHistoryScrollEligibility", () => {
  it("does not become eligible when a mounted draft later binds", () => {
    const { result, rerender } = renderHook(
      ({ conversationId }: { conversationId: number | null }) =>
        useInitialHistoryScrollEligibility(conversationId),
      { initialProps: { conversationId: null as number | null } }
    )
    expect(result.current).toBe(false)
    rerender({ conversationId: 42 })
    expect(result.current).toBe(false)
  })

  it("stays eligible for a view mounted with persisted history", () => {
    const { result, rerender } = renderHook(
      ({ conversationId }: { conversationId: number | null }) =>
        useInitialHistoryScrollEligibility(conversationId),
      { initialProps: { conversationId: 42 as number | null } }
    )
    expect(result.current).toBe(true)
    rerender({ conversationId: 43 })
    expect(result.current).toBe(true)
  })
})

describe("InitialHistoryScrollController", () => {
  it("places instantly, waits for two stable frames, then corrects instantly", () => {
    const onFinish = vi.fn()
    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )

    expect(mocks.scrollToBottom).toHaveBeenCalledTimes(1)
    expect(mocks.scrollToBottom).toHaveBeenLastCalledWith({
      animation: "instant",
    })

    flushNextFrame()
    contentHeight = 140
    scrollHeight = 700
    flushNextFrame()
    flushNextFrame()
    expect(onFinish).not.toHaveBeenCalled()
    flushNextFrame()

    expect(mocks.scrollToBottom).toHaveBeenCalledTimes(2)
    expect(mocks.scrollToBottom).toHaveBeenLastCalledWith({
      animation: "instant",
    })
    expect(onFinish).toHaveBeenCalledTimes(1)
    expect(frames.size).toBe(0)
  })

  it("waits through a failed load and starts on a later successful retry", () => {
    const onFinish = vi.fn()
    const view = render(
      <InitialHistoryScrollController
        pending
        historyReady={false}
        hasHistoryRows={false}
        onFinish={onFinish}
      />
    )
    expect(mocks.scrollToBottom).not.toHaveBeenCalled()
    expect(onFinish).not.toHaveBeenCalled()

    view.rerender(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )
    expect(mocks.scrollToBottom).toHaveBeenCalledTimes(1)
    expect(frames.size).toBe(1)
  })

  it("finishes an empty successful history without scrolling", () => {
    const onFinish = vi.fn()
    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows={false}
        onFinish={onFinish}
      />
    )
    expect(mocks.scrollToBottom).not.toHaveBeenCalled()
    expect(onFinish).toHaveBeenCalledTimes(1)
  })

  it.each(["wheel", "touchstart", "pointerdown", "PageUp", "Home", "ArrowUp"])(
    "cancels initialization on %s user input",
    (input) => {
      const onFinish = vi.fn()
      render(
        <InitialHistoryScrollController
          pending
          historyReady
          hasHistoryRows
          onFinish={onFinish}
        />
      )
      expect(mocks.scrollToBottom).toHaveBeenCalledTimes(1)

      if (input === "wheel") fireEvent.wheel(scrollElement)
      else if (input === "touchstart") fireEvent.touchStart(scrollElement)
      else if (input === "pointerdown") {
        fireEvent.pointerDown(scrollElement, { button: 0 })
      } else {
        fireEvent.keyDown(scrollElement, { key: input })
      }

      expect(mocks.stopScroll).toHaveBeenCalledTimes(1)
      expect(onFinish).toHaveBeenCalledTimes(1)
      expect(mocks.scrollToBottom).toHaveBeenCalledTimes(1)
      expect(frames.size).toBe(0)
    }
  )

  it("cancels its pending frame on unmount without completing", () => {
    const onFinish = vi.fn()
    const view = render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )
    expect(frames.size).toBe(1)
    view.unmount()
    expect(frames.size).toBe(0)
    expect(onFinish).not.toHaveBeenCalled()
  })
})
```

- [ ] **Step 2: Run the controller test and confirm the module is missing**

```powershell
pnpm exec vitest run src/components/message/initial-history-scroll-controller.test.tsx
```

Expected: FAIL because `initial-history-scroll-controller.tsx` does not exist.

- [ ] **Step 3: Implement the mount-local eligibility and controller**

Create `initial-history-scroll-controller.tsx`:

```tsx
"use client"

import { useLayoutEffect, useRef, useState } from "react"
import { useStickToBottomContext } from "use-stick-to-bottom"
import { SCROLL_FOLLOW_INTERACTIVE_SELECTOR } from "./message-scroll-context"

export function useInitialHistoryScrollEligibility(
  conversationId: number | null
): boolean {
  const [eligible] = useState(() => conversationId != null)
  return eligible
}

export interface InitialHistoryScrollControllerProps {
  pending: boolean
  historyReady: boolean
  hasHistoryRows: boolean
  onFinish: () => void
}

export function InitialHistoryScrollController({
  pending,
  historyReady,
  hasHistoryRows,
  onFinish,
}: InitialHistoryScrollControllerProps) {
  const { contentRef, scrollRef, scrollToBottom, stopScroll } =
    useStickToBottomContext()
  const scrollToBottomRef = useRef(scrollToBottom)
  const stopScrollRef = useRef(stopScroll)
  const onFinishRef = useRef(onFinish)
  scrollToBottomRef.current = scrollToBottom
  stopScrollRef.current = stopScroll
  onFinishRef.current = onFinish

  useLayoutEffect(() => {
    if (!pending || !historyReady) return
    if (!hasHistoryRows) {
      onFinishRef.current()
      return
    }

    const viewport = scrollRef.current
    if (!viewport) return

    let disposed = false
    let frameId: number | null = null
    let previousContentHeight: number | null = null
    let previousScrollHeight: number | null = null
    let stableFrames = 0

    const removeListeners = () => {
      viewport.removeEventListener("wheel", cancelForUser)
      viewport.removeEventListener("touchstart", cancelForUser)
      viewport.removeEventListener("pointerdown", onPointerDown)
      viewport.removeEventListener("keydown", onKeyDown)
    }

    const finish = (cancelledByUser: boolean) => {
      if (disposed) return
      disposed = true
      if (frameId != null) {
        cancelAnimationFrame(frameId)
        frameId = null
      }
      removeListeners()
      if (cancelledByUser) stopScrollRef.current()
      onFinishRef.current()
    }

    function cancelForUser() {
      finish(true)
    }

    function onPointerDown(event: PointerEvent) {
      if (event.button !== 0 || event.ctrlKey) return
      const target = event.target as Element | null
      if (target?.closest(SCROLL_FOLLOW_INTERACTIVE_SELECTOR)) return
      cancelForUser()
    }

    function onKeyDown(event: KeyboardEvent) {
      if (
        event.key === "PageUp" ||
        event.key === "Home" ||
        event.key === "ArrowUp"
      ) {
        cancelForUser()
      }
    }

    const measure = () => {
      frameId = null
      if (disposed) return
      const content = contentRef.current
      const currentViewport = scrollRef.current
      if (!content || !currentViewport) {
        frameId = requestAnimationFrame(measure)
        return
      }

      const currentContentHeight = content.getBoundingClientRect().height
      const currentScrollHeight = currentViewport.scrollHeight
      if (
        currentContentHeight === previousContentHeight &&
        currentScrollHeight === previousScrollHeight
      ) {
        stableFrames += 1
      } else {
        stableFrames = 0
      }
      previousContentHeight = currentContentHeight
      previousScrollHeight = currentScrollHeight

      if (stableFrames >= 2) {
        void scrollToBottomRef.current({ animation: "instant" })
        finish(false)
        return
      }
      frameId = requestAnimationFrame(measure)
    }

    viewport.addEventListener("wheel", cancelForUser, { passive: true })
    viewport.addEventListener("touchstart", cancelForUser, { passive: true })
    viewport.addEventListener("pointerdown", onPointerDown)
    viewport.addEventListener("keydown", onKeyDown)

    void scrollToBottomRef.current({ animation: "instant" })
    frameId = requestAnimationFrame(measure)

    return () => {
      if (disposed) return
      disposed = true
      if (frameId != null) cancelAnimationFrame(frameId)
      removeListeners()
    }
  }, [contentRef, hasHistoryRows, historyReady, pending, scrollRef])

  return null
}
```

- [ ] **Step 4: Run the focused test and lint**

```powershell
pnpm exec vitest run src/components/message/initial-history-scroll-controller.test.tsx
pnpm eslint src/components/message/initial-history-scroll-controller.tsx src/components/message/initial-history-scroll-controller.test.tsx
```

Expected: all controller tests and lint pass. Four measurement callbacks are
required in the changing-dimensions case: baseline, changed baseline, stable
frame one, stable frame two.

- [ ] **Step 5: Commit the isolated controller**

```powershell
git add src/components/message/initial-history-scroll-controller.tsx src/components/message/initial-history-scroll-controller.test.tsx
git commit -m "feat(chat): add initial history scroll controller"
```

---

### Task 2: Own the One-Shot Latch in MessageListView

**Files:**
- Modify: `src/components/message/message-list-view.tsx:99,726,1180`
- Modify: `src/components/message/message-list-view.test.tsx`

**Interfaces:**
- Extends: `MessageListViewProps.initialHistoryScrollEligible?: boolean`.
- Extends: `MessageListViewProps.historyLoadComplete?: boolean`.
- Owns: `initialHistoryScrollPending`, initialized once from eligibility and cleared only by the controller.

- [ ] **Step 1: Add a failing resize/latch lifecycle test**

Add `fireEvent` to the Testing Library imports in
`message-list-view.test.tsx`. Add this focused controller mock before importing
`MessageListView`:

```tsx
const initialScrollControllerSpy = vi.fn()
vi.mock("./initial-history-scroll-controller", () => ({
  InitialHistoryScrollController: (props: {
    pending: boolean
    historyReady: boolean
    hasHistoryRows: boolean
    onFinish: () => void
  }) => {
    initialScrollControllerSpy(props)
    return props.pending ? (
      <button
        type="button"
        data-testid="finish-initial-history-scroll"
        onClick={props.onFinish}
      />
    ) : null
  },
}))
```

Add this describe block after the simple helper tests and before the existing
live-footer isolation block:

```tsx
describe("MessageListView initial history scroll latch", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    initialScrollControllerSpy.mockClear()
    listScrollToBottom.mockClear()
    seedHistory()
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  const ui = (isActive: boolean, detailLoading: boolean) => (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageListView
        conversationId={CID}
        agentType="codex"
        connStatus="connected"
        isActive={isActive}
        detailLoading={detailLoading}
        initialHistoryScrollEligible
        historyLoadComplete
        showMessageNav={false}
      />
    </NextIntlClientProvider>
  )

  it("uses instant resize once and does not reset for cache switches or reloads", () => {
    const view = render(ui(true, false))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "instant"
    )
    expect(screen.getByTestId("finish-initial-history-scroll")).toBeInTheDocument()

    fireEvent.click(screen.getByTestId("finish-initial-history-scroll"))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "smooth"
    )
    expect(
      screen.queryByTestId("finish-initial-history-scroll")
    ).not.toBeInTheDocument()

    view.rerender(ui(false, false))
    view.rerender(ui(true, true))
    view.rerender(ui(true, false))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "smooth"
    )
    expect(
      screen.queryByTestId("finish-initial-history-scroll")
    ).not.toBeInTheDocument()

    view.unmount()
    render(ui(true, false))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "instant"
    )
    expect(screen.getByTestId("finish-initial-history-scroll")).toBeInTheDocument()
  })
})
```

- [ ] **Step 2: Run the focused list test and confirm the props are missing**

```powershell
pnpm exec vitest run src/components/message/message-list-view.test.tsx
```

Expected: TypeScript compilation fails because the eligibility and successful
history-load props do not exist.

- [ ] **Step 3: Add the props and one-shot state**

Import the controller:

```tsx
import { InitialHistoryScrollController } from "./initial-history-scroll-controller"
```

Add to `MessageListViewProps`:

```tsx
/** Immutable mount-time eligibility supplied by the owning conversation view. */
initialHistoryScrollEligible?: boolean
/** True only after a persisted detail payload has loaded successfully. */
historyLoadComplete?: boolean
```

Add defaults in the component parameters:

```tsx
initialHistoryScrollEligible = false,
historyLoadComplete = false,
```

Initialize the latch and stable completion callback near the other local state:

```tsx
const [initialHistoryScrollPending, setInitialHistoryScrollPending] = useState(
  () => initialHistoryScrollEligible
)
const finishInitialHistoryScroll = useCallback(() => {
  setInitialHistoryScrollPending(false)
}, [])
```

Do not add an effect that copies later prop changes into this state.

- [ ] **Step 4: Derive persisted-row readiness and mount the controller**

After `threadItems` is built, derive only persisted history rows:

```tsx
const hasPersistedHistoryRows = threadItems.some(
  (item) => item.kind === "turn" && item.phase === "persisted"
)
```

Change `MessageThread.resize` to:

```tsx
resize={
  hasLiveTranscript || initialHistoryScrollPending ? "instant" : "smooth"
}
```

Mount the controller as the first child of `MessageThread`, before
`AutoScrollOnSend`:

```tsx
<InitialHistoryScrollController
  pending={initialHistoryScrollPending}
  historyReady={historyLoadComplete}
  hasHistoryRows={hasPersistedHistoryRows}
  onFinish={finishInitialHistoryScroll}
/>
```

The existing early loading and error returns stay unchanged. Therefore an
initial failure never mounts a ready controller or clears the latch; a later
successful detail render does.

- [ ] **Step 5: Run focused tests and lint**

```powershell
pnpm exec vitest run src/components/message/initial-history-scroll-controller.test.tsx src/components/message/message-list-view.test.tsx
pnpm eslint src/components/message/initial-history-scroll-controller.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx
```

Expected: both suites and lint pass. The list returns to `smooth` exactly once,
does not reset across inactive/active or loading transitions, and reinitializes
only after unmount/remount.

- [ ] **Step 6: Commit MessageListView ownership**

```powershell
git add src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx
git commit -m "feat(chat): initialize uncached history at bottom"
```

---

### Task 3: Wire Mount-Time Eligibility from Every Uncached Conversation View

**Files:**
- Modify: `src/components/conversations/conversation-detail-panel.tsx:191,1374`
- Modify: `src/components/conversations/conversation-detail-panel-layout.test.ts`
- Modify: `src/components/message/sub-agent-session-dialog.tsx:285,378`
- Modify: `src/components/message/sub-agent-session-dialog.test.tsx:206,436`

**Interfaces:**
- Main tabs consume: `useInitialHistoryScrollEligibility(conversationId)`.
- Main and delegated views pass: `initialHistoryScrollEligible` and `historyLoadComplete` to `MessageListView`.

- [ ] **Step 1: Add failing wiring tests**

Append to `conversation-detail-panel-layout.test.ts`:

```typescript
describe("ConversationTabView initial history eligibility", () => {
  it("captures persisted eligibility at mount and passes successful load state", () => {
    expect(source).toMatch(
      /useInitialHistoryScrollEligibility\(\s*conversationId\s*\)/
    )
    expect(source).toContain(
      "initialHistoryScrollEligible={initialHistoryScrollEligible}"
    )
    expect(source).toContain("historyLoadComplete={detail != null}")
  })
})
```

Extend the `MessageListView` sentinel in
`sub-agent-session-dialog.test.tsx` with:

```tsx
data-initial-history-scroll={String(props.initialHistoryScrollEligible)}
data-history-load-complete={String(props.historyLoadComplete)}
```

Add these expectations to the existing read-only MessageListView test:

```tsx
expect(list).toHaveAttribute("data-initial-history-scroll", "true")
expect(list).toHaveAttribute("data-history-load-complete", "false")
```

- [ ] **Step 2: Run both wiring tests and confirm the props are absent**

```powershell
pnpm exec vitest run src/components/conversations/conversation-detail-panel-layout.test.ts src/components/message/sub-agent-session-dialog.test.tsx
```

Expected: both new assertions fail because neither conversation view supplies
the initialization contract yet.

- [ ] **Step 3: Freeze main-tab eligibility at ConversationTabView mount**

Import the hook in `conversation-detail-panel.tsx`:

```tsx
import { useInitialHistoryScrollEligibility } from "@/components/message/initial-history-scroll-controller"
```

Call it unconditionally near the start of `ConversationTabView`:

```tsx
const initialHistoryScrollEligible =
  useInitialHistoryScrollEligibility(conversationId)
```

Pass both values to the existing `MessageListView`:

```tsx
initialHistoryScrollEligible={initialHistoryScrollEligible}
historyLoadComplete={detail != null}
```

Because the hook uses a lazy `useState`, changing `conversationId` when a draft
binds cannot turn a mount-ineligible view into an eligible one. The existing
keep-alive tab component is not remounted by active/inactive CSS changes, so its
latch is also not recreated by tab switching.

- [ ] **Step 4: Opt the uncached delegated-session body into the same lifecycle**

Include `detail` in the existing `useConversationDetail` destructure:

```tsx
const { detail, loading, error, acpLoadError } = useConversationDetail(
  childConversationId,
  { enabled: false }
)
```

Pass these props to its `MessageListView`:

```tsx
initialHistoryScrollEligible
historyLoadComplete={detail != null}
```

`SubAgentSessionBody` is conditionally mounted only while the dialog is open,
so closing and reopening naturally creates a new eligible uncached viewer.

- [ ] **Step 5: Run all focused lifecycle tests and lint**

```powershell
pnpm exec vitest run src/components/message/initial-history-scroll-controller.test.tsx src/components/message/message-list-view.test.tsx src/components/conversations/conversation-detail-panel-layout.test.ts src/components/message/sub-agent-session-dialog.test.tsx
pnpm eslint src/components/message/initial-history-scroll-controller.tsx src/components/message/initial-history-scroll-controller.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/components/conversations/conversation-detail-panel.tsx src/components/conversations/conversation-detail-panel-layout.test.ts src/components/message/sub-agent-session-dialog.tsx src/components/message/sub-agent-session-dialog.test.tsx
```

Expected: every focused suite and lint pass. Draft bind stays ineligible, cached
tab rerenders stay complete, remount reinitializes, and delegated dialog bodies
opt in once per open.

- [ ] **Step 6: Commit view eligibility wiring**

```powershell
git add src/components/conversations/conversation-detail-panel.tsx src/components/conversations/conversation-detail-panel-layout.test.ts src/components/message/sub-agent-session-dialog.tsx src/components/message/sub-agent-session-dialog.test.tsx
git commit -m "feat(chat): scope initial scroll to uncached views"
```

---

### Task 4: Verify Behavior and Regression Boundaries

**Files:**
- No new files.
- Verify all files changed in Tasks 1-3.

**Interfaces:**
- Verifies the controller, message-list latch, keep-alive ownership, delegated viewer, and unchanged live-follow behavior as one independently shippable frontend feature.

- [ ] **Step 1: Confirm the live-footer coordinator remains untouched**

```powershell
git diff -- src/components/message/virtualized-message-thread.tsx src/components/message/message-scroll-context.tsx
```

Expected: this feature adds no diff to either file. Any pre-existing user diff
shown by the command remains byte-for-byte unchanged by this plan.

- [ ] **Step 2: Run the complete frontend checks**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: lint, the full Vitest suite, and static export build pass, including
all existing live-follow escape and message-navigation tests.

- [ ] **Step 3: Perform the real-view measurement smoke check**

Run the app in the normal development runtime, then use one long persisted
conversation and record these exact observations:

```text
1. Close its tab, reopen it from history, and verify the first visible frame is at the bottom with no smooth scrollbar traversal.
2. Scroll upward, switch to another tab and back, and verify the cached position is unchanged.
3. Trigger manual Reload while scrolled upward and verify initialization does not run again.
4. Close and reopen the tab and verify the new uncached mount initializes at the bottom again.
5. During first-load measurement, use wheel, touch/pointer drag, PageUp, Home, and ArrowUp; verify each input takes control without a corrective jump back down.
6. Open, close, and reopen a long delegated sub-agent dialog; verify each newly mounted dialog starts at its bottom.
```

Expected: all six observations match. This manual check is required because
jsdom cannot reproduce Virtua's real ResizeObserver measurements or a webview's
first paint.

- [ ] **Step 4: Inspect the scoped final diff**

```powershell
git diff --check
git status --short
```

Expected: no whitespace errors; only the new controller/tests and the three
planned consumer files are part of this feature's commits. Unrelated working
tree changes remain untouched.
