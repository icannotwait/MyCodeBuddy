import {
  act,
  fireEvent,
  render,
  renderHook,
  screen,
  within,
} from "@testing-library/react"
import {
  forwardRef,
  type ReactNode,
  type Ref,
  useImperativeHandle,
} from "react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  createLiveFooterScrollCoordinator,
  MessageScrollProvider,
  useMessageScroll,
  type MessageScrollContextValue,
} from "./message-scroll-context"
import { MessageThreadScrollButton } from "@/components/ai-elements/message-thread"

// virtua needs layout; render children directly and tag them as virtua items
// so tests can assert the live footer is outside the item array.
const scrollToIndexMock = vi.fn()
vi.mock("virtua", () => ({
  Virtualizer: forwardRef(function VirtualizerMock(
    props: { children?: ReactNode },
    ref: Ref<{ scrollToIndex: (i: number) => void }>
  ) {
    useImperativeHandle(ref, () => ({
      scrollToIndex: (...args: unknown[]) => scrollToIndexMock(...args),
    }))
    return (
      <div data-testid="virtua-root">
        {Array.isArray(props.children)
          ? props.children.map((child, index) => (
              <div key={index} data-virtua-item>
                {child}
              </div>
            ))
          : props.children}
      </div>
    )
  }),
}))

const scrollToBottom = vi.fn()
const stopScroll = vi.fn()
const scrollEl = document.createElement("div")
// Focusable viewport (mirrors production tabIndex=0 install).
scrollEl.tabIndex = 0
Object.defineProperty(scrollEl, "scrollHeight", {
  configurable: true,
  get: () => 1000,
})
Object.defineProperty(scrollEl, "clientHeight", {
  configurable: true,
  get: () => 400,
})
let scrollTopValue = 600
Object.defineProperty(scrollEl, "scrollTop", {
  configurable: true,
  get: () => scrollTopValue,
  set: (v: number) => {
    scrollTopValue = v
  },
})
/** Mutable stick-to-bottom state for reopen / down-button tests. */
let mockIsAtBottom = true

vi.mock("use-stick-to-bottom", () => ({
  useStickToBottomContext: () => ({
    scrollRef: { current: scrollEl },
    scrollToBottom,
    stopScroll,
    get isAtBottom() {
      return mockIsAtBottom
    },
  }),
  StickToBottom: Object.assign(
    ({ children }: { children?: ReactNode }) => <div>{children}</div>,
    {
      Content: ({
        children,
        className,
        ...rest
      }: {
        children?: ReactNode
        className?: string
      }) => (
        <div className={className} data-testid="thread-content" {...rest}>
          {children}
        </div>
      ),
    }
  ),
}))

import { VirtualizedMessageThread } from "./virtualized-message-thread"

function virtualizerItems(): HTMLElement[] {
  return Array.from(
    document.querySelectorAll<HTMLElement>("[data-virtua-item]")
  )
}

const pendingFrames: FrameRequestCallback[] = []
let resizeObserverCb: ResizeObserverCallback | null = null

function runPendingFrames() {
  const pending = pendingFrames.splice(0, pendingFrames.length)
  for (const cb of pending) cb(performance.now())
}

/** jsdom lacks PointerEvent; synthesize pointerdown with a real `button`. */
function dispatchPointerDown(
  target: EventTarget,
  opts: { button?: number; ctrlKey?: boolean } = {}
) {
  const event = new Event("pointerdown", { bubbles: true, cancelable: true })
  Object.defineProperty(event, "button", {
    value: opts.button ?? 0,
    enumerable: true,
  })
  Object.defineProperty(event, "ctrlKey", {
    value: opts.ctrlKey ?? false,
    enumerable: true,
  })
  target.dispatchEvent(event)
}

function createStickToBottomHarness(opts: { atBottom: boolean }) {
  const stop = vi.fn()
  const scrollToBottomFn = vi.fn()
  let scrollTop = opts.atBottom ? 600 : 100
  const anchorScrollTop = scrollTop
  const frames: FrameRequestCallback[] = []
  const coordinator = createLiveFooterScrollCoordinator({
    scrollToBottom: scrollToBottomFn as never,
    stopScroll: stop,
    initiallyFollowing: opts.atBottom,
    scheduleFrame: (cb) => {
      frames.push(cb)
      return frames.length
    },
    cancelFrame: () => {
      frames.length = 0
    },
  })
  return {
    scrollToBottom: scrollToBottomFn,
    stopScroll: stop,
    coordinator,
    anchorScrollTop,
    scrollTop: () => scrollTop,
    setScrollTop: (v: number) => {
      scrollTop = v
    },
    runAnimationFrames: () => {
      const pending = frames.splice(0, frames.length)
      for (const cb of pending) cb(performance.now())
    },
  }
}

describe("LiveFooterScrollCoordinator", () => {
  it("uses one instant correction for one live publication while following", () => {
    const scroll = createStickToBottomHarness({ atBottom: true })
    act(() => {
      scroll.coordinator.scheduleFollow(1)
    })
    act(() => {
      scroll.runAnimationFrames()
    })
    expect(scroll.scrollToBottom).toHaveBeenCalledTimes(1)
    expect(scroll.scrollToBottom).toHaveBeenCalledWith({
      animation: "instant",
      preserveScrollPosition: true,
    })
  })

  it("stops following when cancelForUserInput is invoked", () => {
    const scroll = createStickToBottomHarness({ atBottom: true })
    act(() => {
      scroll.coordinator.cancelForUserInput()
    })
    act(() => {
      scroll.coordinator.scheduleFollow(2)
    })
    act(() => {
      scroll.runAnimationFrames()
    })
    expect(scroll.stopScroll).toHaveBeenCalled()
    expect(scroll.scrollToBottom).not.toHaveBeenCalled()
    expect(scroll.scrollTop()).toBe(scroll.anchorScrollTop)
  })

  it("coalesces text and sealed-block height changes into one correction", () => {
    const scroll = createStickToBottomHarness({ atBottom: true })
    act(() => {
      scroll.coordinator.scheduleFollow(10)
      scroll.coordinator.scheduleFollow(11)
    })
    act(() => {
      scroll.runAnimationFrames()
    })
    expect(scroll.scrollToBottom).toHaveBeenCalledTimes(1)
  })

  it("re-enables follow after markAtBottom", () => {
    const scroll = createStickToBottomHarness({ atBottom: true })
    act(() => {
      scroll.coordinator.cancelForUserInput()
      scroll.coordinator.markAtBottom()
      scroll.coordinator.scheduleFollow(3)
    })
    act(() => {
      scroll.runAnimationFrames()
    })
    expect(scroll.scrollToBottom).toHaveBeenCalledTimes(1)
  })
})

describe("VirtualizedMessageThread footer slot", () => {
  beforeEach(() => {
    scrollToBottom.mockClear()
    stopScroll.mockClear()
    scrollToIndexMock.mockClear()
    scrollTopValue = 600
    mockIsAtBottom = true
    pendingFrames.length = 0
    resizeObserverCb = null

    vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
      pendingFrames.push(cb)
      return pendingFrames.length
    })
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {
      pendingFrames.length = 0
    })

    class MockResizeObserver {
      constructor(cb: ResizeObserverCallback) {
        resizeObserverCb = cb
      }
      observe() {}
      unobserve() {}
      disconnect() {
        resizeObserverCb = null
      }
    }
    vi.stubGlobal("ResizeObserver", MockResizeObserver)
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    scrollEl.replaceChildren()
    scrollEl.remove()
  })

  it("keeps the footer outside the Virtua item array", () => {
    render(
      <VirtualizedMessageThread
        items={["history"]}
        getItemKey={(item) => item}
        renderItem={(item) => <div data-testid="history">{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
      />
    )
    expect(virtualizerItems()).toHaveLength(1)
    expect(screen.getByTestId("live-footer")).toBeInTheDocument()
    expect(screen.getByTestId("live-footer")).not.toHaveAttribute(
      "data-virtua-item"
    )
    expect(
      within(document.querySelector("[data-virtua-item]")!).queryByTestId(
        "live-footer"
      )
    ).toBeNull()
    expect(
      document.querySelector("[data-message-live-footer]")
    ).toBeInTheDocument()
  })

  it("renders the footer when history is empty without using the empty state", () => {
    render(
      <VirtualizedMessageThread
        items={[]}
        getItemKey={() => "x"}
        renderItem={() => null}
        emptyState={<div data-testid="empty">empty</div>}
        footer={<div data-testid="live-footer">live</div>}
      />
    )
    expect(screen.queryByTestId("empty")).toBeNull()
    expect(screen.getByTestId("live-footer")).toBeInTheDocument()
    expect(virtualizerItems()).toHaveLength(0)
  })

  it("scrollToIndex only targets historical item indices", () => {
    const scrollApiRef: {
      current: null | MessageScrollContextValue
    } = { current: null }
    render(
      <VirtualizedMessageThread
        items={["a", "b"]}
        getItemKey={(item) => item}
        renderItem={(item) => <div>{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
        scrollApiRef={scrollApiRef as never}
      />
    )
    expect(scrollApiRef.current).not.toBeNull()
    expect(typeof scrollApiRef.current?.scrollToIndex).toBe("function")
    // Footer is not a virtua item — only two history rows exist.
    expect(virtualizerItems()).toHaveLength(2)
    act(() => {
      scrollApiRef.current?.scrollToIndex(0)
      scrollApiRef.current?.scrollToIndex(1)
    })
    expect(scrollToIndexMock).toHaveBeenCalledWith(0, undefined)
    expect(scrollToIndexMock).toHaveBeenCalledWith(1, undefined)
  })

  it("keeps the footer under role=log ancestry for selection/copy", () => {
    render(
      // MessageThread normally supplies role="log"; simulate that ancestry.
      <div role="log">
        <VirtualizedMessageThread
          items={["history"]}
          getItemKey={(item) => item}
          renderItem={(item) => <div>{item}</div>}
          footer={
            <div data-testid="live-footer" data-selectable>
              copy me
            </div>
          }
        />
      </div>
    )
    const footer = screen.getByTestId("live-footer")
    expect(footer.closest('[role="log"]')).not.toBeNull()
    expect(footer).toHaveAttribute("data-selectable")
  })

  it("500 footer height changes do not change Virtua items or keys", () => {
    const history = Array.from({ length: 3 }, (_, i) => `turn-${i}`)
    const { rerender } = render(
      <VirtualizedMessageThread
        items={history}
        getItemKey={(item) => item}
        renderItem={(item) => <div data-testid={item}>{item}</div>}
        footer={<div data-testid="live-footer">v0</div>}
      />
    )
    const keysBefore = virtualizerItems().map((el) =>
      el.querySelector("[data-testid]")?.getAttribute("data-testid")
    )
    expect(virtualizerItems()).toHaveLength(3)

    for (let i = 1; i <= 500; i += 1) {
      rerender(
        <VirtualizedMessageThread
          items={history}
          getItemKey={(item) => item}
          renderItem={(item) => <div data-testid={item}>{item}</div>}
          footer={<div data-testid="live-footer">v{i}</div>}
        />
      )
    }

    expect(virtualizerItems()).toHaveLength(3)
    const keysAfter = virtualizerItems().map((el) =>
      el.querySelector("[data-testid]")?.getAttribute("data-testid")
    )
    expect(keysAfter).toEqual(keysBefore)
  })

  it("scrollToIndex(0) and last historical index still target same turns with footer", () => {
    const scrollApiRef: {
      current: null | MessageScrollContextValue
    } = { current: null }
    const items = ["first", "mid", "last"]
    render(
      <VirtualizedMessageThread
        items={items}
        getItemKey={(item) => item}
        renderItem={(item) => <div data-testid={`row-${item}`}>{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
        scrollApiRef={scrollApiRef as never}
      />
    )
    act(() => {
      scrollApiRef.current?.scrollToIndex(0)
      scrollApiRef.current?.scrollToIndex(items.length - 1)
    })
    expect(scrollToIndexMock).toHaveBeenNthCalledWith(1, 0, undefined)
    expect(scrollToIndexMock).toHaveBeenNthCalledWith(2, 2, undefined)
    expect(screen.getByTestId("row-first")).toBeInTheDocument()
    expect(screen.getByTestId("row-last")).toBeInTheDocument()
    expect(virtualizerItems()).toHaveLength(3)
  })

  describe("viewport escape DOM events", () => {
    // Escape listeners attach to scrollRef (scrollEl). Keep interactive
    // targets as children of scrollEl so pointerdown bubbles with the
    // correct event.target for closest(interactive) checks.
    function mountWithFooter() {
      const scrollApiRef: { current: null | MessageScrollContextValue } = {
        current: null,
      }
      if (!scrollEl.isConnected) {
        document.body.appendChild(scrollEl)
      }
      scrollEl.replaceChildren()
      render(
        <VirtualizedMessageThread
          items={["a"]}
          getItemKey={(item) => item}
          renderItem={(item) => <div data-testid="history-row">{item}</div>}
          footer={<div data-testid="live-footer">live body</div>}
          scrollApiRef={scrollApiRef as never}
        />
      )
      return scrollApiRef
    }

    it.each([
      ["wheel", () => fireEvent.wheel(scrollEl)],
      ["touchstart", () => fireEvent.touchStart(scrollEl)],
      [
        "pointerdown non-interactive",
        () => {
          const plain = document.createElement("div")
          plain.dataset.testid = "plain-body"
          scrollEl.appendChild(plain)
          dispatchPointerDown(plain)
        },
      ],
      [
        "PageUp",
        () => fireEvent.keyDown(scrollEl, { key: "PageUp", bubbles: true }),
      ],
      [
        "Home",
        () => fireEvent.keyDown(scrollEl, { key: "Home", bubbles: true }),
      ],
      [
        "ArrowUp",
        () => fireEvent.keyDown(scrollEl, { key: "ArrowUp", bubbles: true }),
      ],
    ] as const)(
      "cancels follow on %s and post-escape publish does not scrollToBottom",
      (_label, fireEscape) => {
        const scrollApiRef = mountWithFooter()
        const footerScroll = scrollApiRef.current?.footerScroll
        expect(footerScroll?.isFollowing()).toBe(true)

        act(() => {
          fireEscape()
        })
        expect(stopScroll).toHaveBeenCalled()
        expect(footerScroll?.isFollowing()).toBe(false)

        scrollToBottom.mockClear()
        act(() => {
          footerScroll?.scheduleFollow(99)
          runPendingFrames()
        })
        expect(scrollToBottom).not.toHaveBeenCalled()
        expect(scrollTopValue).toBe(600)
      }
    )

    it("pointerdown on interactive button does not cancel follow", () => {
      const scrollApiRef = mountWithFooter()
      const footerScroll = scrollApiRef.current?.footerScroll
      const button = document.createElement("button")
      button.type = "button"
      button.dataset.testid = "tool-expander"
      scrollEl.appendChild(button)

      act(() => {
        dispatchPointerDown(button)
      })
      expect(stopScroll).not.toHaveBeenCalled()
      expect(footerScroll?.isFollowing()).toBe(true)

      act(() => {
        footerScroll?.scheduleFollow(7)
        runPendingFrames()
      })
      expect(scrollToBottom).toHaveBeenCalledWith({
        animation: "instant",
        preserveScrollPosition: true,
      })
    })

    it("pointerdown on tool expander role=button does not cancel follow", () => {
      const scrollApiRef = mountWithFooter()
      const footerScroll = scrollApiRef.current?.footerScroll
      const expander = document.createElement("div")
      expander.setAttribute("role", "button")
      expander.tabIndex = 0
      expander.dataset.testid = "role-button-expander"
      scrollEl.appendChild(expander)

      act(() => {
        dispatchPointerDown(expander)
      })
      expect(stopScroll).not.toHaveBeenCalled()
      expect(footerScroll?.isFollowing()).toBe(true)
    })
  })

  it("down button click re-arms follow via markAtBottom + scrollToBottom", () => {
    const scrollApiRef: { current: null | MessageScrollContextValue } = {
      current: null,
    }
    mockIsAtBottom = false
    scrollTopValue = 100

    render(
      <>
        <VirtualizedMessageThread
          items={["a"]}
          getItemKey={(item) => item}
          renderItem={(item) => <div>{item}</div>}
          footer={<div data-testid="live-footer">live</div>}
          scrollApiRef={scrollApiRef as never}
        />
        <MessageThreadScrollButton
          onBeforeScrollToBottom={() => {
            scrollApiRef.current?.footerScroll?.markAtBottom()
          }}
        />
      </>
    )

    const footerScroll = scrollApiRef.current?.footerScroll
    expect(footerScroll).toBeDefined()
    // Coordinator was created with isAtBottom false → not following.
    expect(footerScroll?.isFollowing()).toBe(false)

    const downBtn = document.querySelector(
      "[data-message-scroll-to-bottom]"
    ) as HTMLButtonElement
    expect(downBtn).toBeTruthy()

    act(() => {
      fireEvent.click(downBtn)
    })

    expect(footerScroll?.isFollowing()).toBe(true)
    expect(scrollToBottom).toHaveBeenCalled()

    scrollToBottom.mockClear()
    act(() => {
      footerScroll?.scheduleFollow(12)
      runPendingFrames()
    })
    expect(scrollToBottom).toHaveBeenCalledTimes(1)
  })

  it("expand at bottom schedules one ResizeObserver correction", () => {
    const scrollApiRef: { current: null | MessageScrollContextValue } = {
      current: null,
    }
    render(
      <VirtualizedMessageThread
        items={["a"]}
        getItemKey={(item) => item}
        renderItem={(item) => <div>{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
        scrollApiRef={scrollApiRef as never}
      />
    )
    expect(resizeObserverCb).toBeTypeOf("function")
    expect(scrollApiRef.current?.footerScroll?.isFollowing()).toBe(true)

    act(() => {
      resizeObserverCb?.(
        [] as unknown as ResizeObserverEntry[],
        {} as ResizeObserver
      )
      runPendingFrames()
    })
    expect(scrollToBottom).toHaveBeenCalledTimes(1)
    expect(scrollToBottom).toHaveBeenCalledWith({
      animation: "instant",
      preserveScrollPosition: true,
    })
  })

  it("expand while scrolled away preserves scrollTop (no follow correction)", () => {
    const scrollApiRef: { current: null | MessageScrollContextValue } = {
      current: null,
    }
    scrollTopValue = 100
    mockIsAtBottom = false

    render(
      <VirtualizedMessageThread
        items={["a"]}
        getItemKey={(item) => item}
        renderItem={(item) => <div>{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
        scrollApiRef={scrollApiRef as never}
      />
    )
    const footerScroll = scrollApiRef.current?.footerScroll
    expect(footerScroll?.isFollowing()).toBe(false)
    const anchor = scrollTopValue

    act(() => {
      resizeObserverCb?.(
        [] as unknown as ResizeObserverEntry[],
        {} as ResizeObserver
      )
      runPendingFrames()
    })
    expect(scrollToBottom).not.toHaveBeenCalled()
    expect(scrollTopValue).toBe(anchor)
  })

  it("mid-stream reopen with isAtBottom false does not auto-follow publications", () => {
    const scrollApiRef: { current: null | MessageScrollContextValue } = {
      current: null,
    }
    scrollTopValue = 10
    mockIsAtBottom = false

    render(
      <VirtualizedMessageThread
        items={["a"]}
        getItemKey={(item) => item}
        renderItem={(item) => <div>{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
        scrollApiRef={scrollApiRef as never}
      />
    )
    const footerScroll = scrollApiRef.current?.footerScroll
    expect(footerScroll?.isFollowing()).toBe(false)

    act(() => {
      // Footer-owned publication path (same as LiveTranscriptRow).
      footerScroll?.scheduleFollow(5)
      runPendingFrames()
    })
    expect(scrollToBottom).not.toHaveBeenCalled()
  })

  it("text growth + expand at bottom coalesce to one correction under one RAF", () => {
    const scrollApiRef: { current: null | MessageScrollContextValue } = {
      current: null,
    }
    render(
      <VirtualizedMessageThread
        items={["a"]}
        getItemKey={(item) => item}
        renderItem={(item) => <div>{item}</div>}
        footer={<div data-testid="live-footer">live</div>}
        scrollApiRef={scrollApiRef as never}
      />
    )
    const footerScroll = scrollApiRef.current?.footerScroll

    act(() => {
      // Publication (footer owner) + expand height (shell ResizeObserver).
      footerScroll?.scheduleFollow(20)
      resizeObserverCb?.(
        [] as unknown as ResizeObserverEntry[],
        {} as ResizeObserver
      )
      runPendingFrames()
    })
    expect(scrollToBottom).toHaveBeenCalledTimes(1)
  })
})

describe("MessageScrollProvider footerScroll access", () => {
  it("exposes footerScroll to descendants", () => {
    const coordinator = createLiveFooterScrollCoordinator({
      scrollToBottom: vi.fn() as never,
      stopScroll: vi.fn(),
    })
    const { result } = renderHook(() => useMessageScroll(), {
      wrapper: ({ children }) => (
        <MessageScrollProvider
          value={{ scrollToIndex: vi.fn(), footerScroll: coordinator }}
        >
          {children}
        </MessageScrollProvider>
      ),
    })
    expect(result.current?.footerScroll).toBe(coordinator)
  })
})
