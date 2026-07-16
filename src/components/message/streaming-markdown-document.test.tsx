import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import {
  appendStreamingMarkdown,
  completeStreamingMarkdown,
  createIncrementalStreamBlocks,
  type IncrementalStreamBlocks,
} from "@/lib/markdown/incremental-stream-blocks"
import { StreamingMarkdownDocument } from "./streaming-markdown-document"

vi.mock("@/components/ai-elements/message", () => ({
  MessageResponse: ({
    children,
    mode,
    autolinkLocalPaths,
  }: {
    children?: React.ReactNode
    mode?: string
    autolinkLocalPaths?: boolean
  }) => (
    <div
      data-testid="message-response"
      data-mode={mode}
      data-autolink-local-paths={String(!!autolinkLocalPaths)}
    >
      {children}
    </div>
  ),
}))

vi.mock("@/components/ai-elements/code-block", () => ({
  CodeBlockContainer: ({
    children,
    language,
  }: {
    children?: React.ReactNode
    language: string
  }) => (
    <div data-testid="code-block-container" data-language={language}>
      {children}
    </div>
  ),
}))

function doc(source: string): IncrementalStreamBlocks {
  return appendStreamingMarkdown(
    createIncrementalStreamBlocks("segment-1"),
    source
  )
}

function invalidDoc(source: string): IncrementalStreamBlocks {
  const document = doc(source)
  return { ...document, sealed: [], tail: source, valid: false }
}

describe("StreamingMarkdownDocument", () => {
  it("renders sealed blocks once while only the tail changes", () => {
    const blockRender = vi.fn()
    const { rerender } = render(
      <StreamingMarkdownDocument
        document={doc("first\n\ntail")}
        onBlockRender={blockRender}
      />
    )
    const sealedRenders = blockRender.mock.calls.length
    expect(sealedRenders).toBeGreaterThan(0)
    rerender(
      <StreamingMarkdownDocument
        document={doc("first\n\ntail grows")}
        onBlockRender={blockRender}
      />
    )
    // Memoized sealed blocks must not re-render when only the tail grows.
    expect(blockRender).toHaveBeenCalledTimes(sealedRenders)
    expect(screen.getByTestId("streaming-markdown-tail")).toHaveTextContent(
      "tail grows"
    )
  })

  it("keeps an open code fence plain, selectable, and unhighlighted", () => {
    render(<StreamingMarkdownDocument document={doc("```ts\nconst x = 1")} />)
    const tail = screen.getByTestId("streaming-code-tail")
    expect(tail).toHaveTextContent("const x = 1")
    expect(tail.querySelector("[data-highlighted]")).toBeNull()
    expect(tail).toHaveClass("select-text")
  })

  it("falls back to visible canonical source when partition validation fails", () => {
    render(<StreamingMarkdownDocument document={invalidDoc("**visible")} />)
    // Mock MessageResponse echoes raw source; real Streamdown would strip **.
    expect(screen.getByText("**visible")).toBeInTheDocument()
  })

  it("keeps live sealed blocks opted out by default", () => {
    render(<StreamingMarkdownDocument document={doc("first\n\ntail")} />)
    for (const response of screen.getAllByTestId("message-response")) {
      expect(response).toHaveAttribute("data-autolink-local-paths", "false")
    }
  })

  it("propagates the opt-in to every completed sealed block", () => {
    const completed = completeStreamingMarkdown(
      doc(String.raw`D:\repo\src\app.ts`)
    )
    render(
      <StreamingMarkdownDocument
        document={completed}
        richContentState="complete"
        autolinkLocalPaths
      />
    )
    for (const response of screen.getAllByTestId("message-response")) {
      expect(response).toHaveAttribute("data-autolink-local-paths", "true")
    }
  })

  it("propagates the opt-in through the invalid-document fallback", () => {
    render(
      <StreamingMarkdownDocument
        document={invalidDoc(String.raw`D:\repo\src\app.ts`)}
        richContentState="complete"
        autolinkLocalPaths
      />
    )
    expect(screen.getByTestId("message-response")).toHaveAttribute(
      "data-autolink-local-paths",
      "true"
    )
  })

  it("rerenders a sealed block when the opt-in changes", () => {
    const blockRender = vi.fn()
    const document = doc("first\n\ntail")
    const { rerender } = render(
      <StreamingMarkdownDocument
        document={document}
        onBlockRender={blockRender}
      />
    )
    const before = blockRender.mock.calls.length
    rerender(
      <StreamingMarkdownDocument
        document={document}
        onBlockRender={blockRender}
        autolinkLocalPaths
      />
    )
    expect(blockRender.mock.calls.length).toBeGreaterThan(before)
  })
})
