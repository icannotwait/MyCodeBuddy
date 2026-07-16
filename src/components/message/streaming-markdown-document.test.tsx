import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import {
  appendStreamingMarkdown,
  createIncrementalStreamBlocks,
  type IncrementalStreamBlocks,
} from "@/lib/markdown/incremental-stream-blocks"
import { StreamingMarkdownDocument } from "./streaming-markdown-document"

vi.mock("@/components/ai-elements/message", () => ({
  MessageResponse: ({
    children,
    mode,
  }: {
    children?: React.ReactNode
    mode?: string
  }) => (
    <div data-testid="message-response" data-mode={mode}>
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
})
