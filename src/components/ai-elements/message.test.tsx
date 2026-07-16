import { act, render, screen } from "@testing-library/react"
import type { ReactNode } from "react"
import { afterEach, describe, expect, it, vi } from "vitest"
import {
  appendStreamingMarkdown,
  createIncrementalStreamBlocks,
} from "@/lib/markdown/incremental-stream-blocks"
import { StreamingMarkdownDocument } from "@/components/message/streaming-markdown-document"
import {
  __getStreamdownPluginDebugStateForTest,
  __resetStreamdownPluginsForTest,
} from "./streamdown-plugins"

vi.mock("streamdown", () => ({
  Streamdown: ({
    children,
    className,
  }: {
    children: ReactNode
    className?: string
  }) => (
    <div className={className} data-testid="streamdown-root">
      {children}
    </div>
  ),
  defaultRemarkPlugins: {},
  defaultRehypePlugins: {},
}))

vi.mock("@streamdown/cjk", () => ({ cjk: {} }))
vi.mock("@streamdown/math", () => ({
  createMathPlugin: () => ({}),
}))
vi.mock("@streamdown/mermaid", () => ({ mermaid: {} }))
vi.mock("@streamdown/code", () => ({
  code: {
    highlight: vi.fn(),
    supportsLanguage: vi.fn(() => true),
  },
}))

vi.mock("@/components/ai-elements/link-safety", () => ({
  useStreamdownLinkSafety: () => ({ enabled: false }),
}))

import { MessageResponse } from "./message"

afterEach(() => {
  __resetStreamdownPluginsForTest()
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

function installIntersectionObserver(initiallyVisible: boolean) {
  let enterNearViewport: (() => void) | null = null
  class FakeIntersectionObserver implements IntersectionObserver {
    readonly root: Element | Document | null = null
    readonly rootMargin = "600px 0px"
    readonly thresholds: ReadonlyArray<number> = [0]
    constructor(next: IntersectionObserverCallback) {
      enterNearViewport = () => {
        next([{ isIntersecting: true } as IntersectionObserverEntry], this)
      }
    }
    observe = vi.fn(() => {
      if (initiallyVisible) {
        enterNearViewport?.()
      }
    })
    disconnect = vi.fn()
    unobserve = vi.fn()
    takeRecords = () => []
  }
  vi.stubGlobal("IntersectionObserver", FakeIntersectionObserver)
  return {
    enter: () => {
      act(() => {
        enterNearViewport?.()
      })
    },
  }
}

describe("MessageResponse", () => {
  it("applies marker styles so ordered Markdown lists render as lists", () => {
    render(<MessageResponse>{"1. First\n2. Second"}</MessageResponse>)

    expect(screen.getByTestId("streamdown-root")).toHaveClass(
      "[&_ol]:list-decimal",
      "[&_ol]:pl-3"
    )
  })

  it("does not request Mermaid for a sealed streaming block", async () => {
    render(
      <MessageResponse richContentState="sealed-streaming">
        {"```mermaid\ngraph TD; A-->B\n```"}
      </MessageResponse>
    )
    await act(async () => {})
    expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(0)
    expect(screen.getByText(/graph TD/)).toBeVisible()
  })

  it("renders sealed math but never parses a lightweight tail", async () => {
    render(
      <MessageResponse richContentState="sealed-streaming">
        {"$x$"}
      </MessageResponse>
    )
    await vi.waitFor(() =>
      expect(__getStreamdownPluginDebugStateForTest().requests.math).toBe(1)
    )
    // Identity splitter — avoid pulling Streamdown's real parseMarkdownIntoBlocks
    // through the module mock used for MessageResponse.
    const tail = appendStreamingMarkdown(
      createIncrementalStreamBlocks("tail-1", (markdown) =>
        markdown ? [markdown] : []
      ),
      "$unfinished"
    )
    render(<StreamingMarkdownDocument document={tail} />)
    expect(__getStreamdownPluginDebugStateForTest().requests.math).toBe(1)
  })

  it("loads completed Mermaid only near the viewport", async () => {
    const observer = installIntersectionObserver(false)
    render(
      <MessageResponse richContentState="complete">
        {"```mermaid\ngraph TD; A-->B\n```"}
      </MessageResponse>
    )
    expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(0)
    observer.enter()
    await vi.waitFor(() =>
      expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(1)
    )
  })

  it("cancels idle Mermaid enablement on unmount when IntersectionObserver is absent", () => {
    vi.useFakeTimers()
    vi.stubGlobal("IntersectionObserver", undefined)
    vi.stubGlobal("requestIdleCallback", undefined)

    const { unmount } = render(
      <MessageResponse richContentState="complete">
        {"```mermaid\ngraph TD; A-->B\n```"}
      </MessageResponse>
    )
    expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(0)
    unmount()
    act(() => {
      vi.advanceTimersByTime(1_000)
    })
    expect(__getStreamdownPluginDebugStateForTest().requests.mermaid).toBe(0)
  })
})
