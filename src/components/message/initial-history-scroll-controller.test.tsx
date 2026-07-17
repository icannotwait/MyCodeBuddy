import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
} from "@testing-library/react"
import { StrictMode } from "react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  scrollToBottom: vi.fn(),
  stopScroll: vi.fn(),
}))

const stickRefs = vi.hoisted(() => ({
  scrollRef: { current: null as HTMLDivElement | null },
  contentRef: { current: null as HTMLDivElement | null },
}))

let scrollElement: HTMLDivElement
let contentElement: HTMLDivElement
let scrollHeight = 0
let contentHeight = 0

vi.mock("use-stick-to-bottom", () => ({
  useStickToBottomContext: () => ({
    scrollRef: stickRefs.scrollRef,
    contentRef: stickRefs.contentRef,
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

function flushStableFinish(): void {
  // baseline sample, stable #1, stable #2 → final scroll + finish
  flushNextFrame()
  flushNextFrame()
  flushNextFrame()
}

/** jsdom lacks PointerEvent; MouseEvent carries button/ctrlKey for our handlers. */
function dispatchPointerDown(
  target: EventTarget,
  init: { button?: number; ctrlKey?: boolean; eventTarget?: EventTarget } = {}
): void {
  const event = new MouseEvent("pointerdown", {
    bubbles: true,
    cancelable: true,
    button: init.button ?? 0,
    ctrlKey: init.ctrlKey ?? false,
  })
  if (init.eventTarget != null) {
    Object.defineProperty(event, "target", {
      configurable: true,
      get: () => init.eventTarget,
    })
  }
  target.dispatchEvent(event)
}

beforeEach(() => {
  scrollElement = document.createElement("div")
  contentElement = document.createElement("div")
  stickRefs.scrollRef.current = scrollElement
  stickRefs.contentRef.current = contentElement
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

  it("finishes empty successful history only once under StrictMode", () => {
    const onFinish = vi.fn()
    render(
      <StrictMode>
        <InitialHistoryScrollController
          pending
          historyReady
          hasHistoryRows={false}
          onFinish={onFinish}
        />
      </StrictMode>
    )
    expect(mocks.scrollToBottom).not.toHaveBeenCalled()
    expect(onFinish).toHaveBeenCalledTimes(1)
  })

  it("still finishes after a failed load then empty successful history", () => {
    const onFinish = vi.fn()
    const view = render(
      <InitialHistoryScrollController
        pending
        historyReady={false}
        hasHistoryRows={false}
        onFinish={onFinish}
      />
    )
    expect(onFinish).not.toHaveBeenCalled()

    view.rerender(
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

  it("retries start via RAF when viewport attaches after ready without rerender", () => {
    const onFinish = vi.fn()
    stickRefs.scrollRef.current = null

    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )

    expect(mocks.scrollToBottom).not.toHaveBeenCalled()
    expect(onFinish).not.toHaveBeenCalled()
    expect(frames.size).toBe(1)

    // Attach viewport without React re-render (ref.current change alone).
    stickRefs.scrollRef.current = scrollElement

    flushNextFrame()
    expect(mocks.scrollToBottom).toHaveBeenCalledTimes(1)
    expect(mocks.scrollToBottom).toHaveBeenLastCalledWith({
      animation: "instant",
    })
    expect(frames.size).toBe(1)

    flushStableFinish()
    expect(mocks.scrollToBottom).toHaveBeenCalledTimes(2)
    expect(onFinish).toHaveBeenCalledTimes(1)
    expect(frames.size).toBe(0)
  })

  it("cancels pre-start RAF retry on unmount without finishing", () => {
    const onFinish = vi.fn()
    stickRefs.scrollRef.current = null

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
    expect(mocks.scrollToBottom).not.toHaveBeenCalled()
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

  it("ignores non-primary pointer and leaves RAF pending", () => {
    const onFinish = vi.fn()
    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )
    expect(frames.size).toBe(1)

    dispatchPointerDown(scrollElement, { button: 1 })

    expect(mocks.stopScroll).not.toHaveBeenCalled()
    expect(onFinish).not.toHaveBeenCalled()
    expect(frames.size).toBe(1)
  })

  it("ignores ctrl+primary pointer and leaves RAF pending", () => {
    const onFinish = vi.fn()
    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )
    expect(frames.size).toBe(1)

    dispatchPointerDown(scrollElement, { button: 0, ctrlKey: true })

    expect(mocks.stopScroll).not.toHaveBeenCalled()
    expect(onFinish).not.toHaveBeenCalled()
    expect(frames.size).toBe(1)
  })

  it("ignores pointer on interactive control matching SCROLL_FOLLOW_INTERACTIVE_SELECTOR", () => {
    const onFinish = vi.fn()
    const button = document.createElement("button")
    button.type = "button"
    button.textContent = "action"
    scrollElement.appendChild(button)

    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )
    expect(frames.size).toBe(1)

    // Dispatch on the button so event.target.closest(selector) is exercised.
    dispatchPointerDown(button, { button: 0 })

    expect(mocks.stopScroll).not.toHaveBeenCalled()
    expect(onFinish).not.toHaveBeenCalled()
    expect(frames.size).toBe(1)
  })

  it("cancels on pointerdown when target is a non-Element Text node", () => {
    const onFinish = vi.fn()
    const textNode = document.createTextNode("transcript text")
    scrollElement.appendChild(textNode)

    render(
      <InitialHistoryScrollController
        pending
        historyReady
        hasHistoryRows
        onFinish={onFinish}
      />
    )
    expect(mocks.scrollToBottom).toHaveBeenCalledTimes(1)
    expect(frames.size).toBe(1)

    dispatchPointerDown(scrollElement, {
      button: 0,
      eventTarget: textNode,
    })

    expect(mocks.stopScroll).toHaveBeenCalledTimes(1)
    expect(onFinish).toHaveBeenCalledTimes(1)
    expect(frames.size).toBe(0)
  })

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

    // Former viewport events must no-op after unmount cleanup.
    fireEvent.wheel(scrollElement)
    fireEvent.pointerDown(scrollElement, { button: 0 })
    fireEvent.keyDown(scrollElement, { key: "PageUp" })
    expect(mocks.stopScroll).not.toHaveBeenCalled()
    expect(onFinish).not.toHaveBeenCalled()
  })
})
