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
