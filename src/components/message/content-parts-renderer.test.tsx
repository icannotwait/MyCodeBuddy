import { act, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import type {
  AdaptedContentPart,
  AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import enMessages from "@/i18n/messages/en.json"
import * as tryParseJsonMod from "@/lib/try-parse-json"
import * as unifiedDiff from "@/lib/unified-diff-generator"

vi.mock("@/components/ai-elements/message", () => ({
  MessageResponse: ({ children }: { children?: React.ReactNode }) => (
    <div data-testid="markdown-response">{children}</div>
  ),
  normalizeMathDelimiters: (children: React.ReactNode) => children,
}))

vi.mock("@/components/ai-elements/terminal", () => ({
  Terminal: ({
    output,
    isStreaming,
  }: {
    output: string
    isStreaming?: boolean
  }) => (
    <pre data-testid="terminal-output" data-streaming={String(!!isStreaming)}>
      {output}
    </pre>
  ),
}))

vi.mock("@/components/diff/unified-diff-preview", () => ({
  UnifiedDiffPreview: ({ diffText }: { diffText: string }) => (
    <div data-testid="unified-diff">{diffText}</div>
  ),
}))

import { ContentPartsRenderer, ToolCallPart } from "./content-parts-renderer"

function wrap(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

function completedEditTool(): AdaptedToolCallPart {
  return {
    type: "tool-call",
    toolCallId: "edit-1",
    toolName: "edit",
    input: JSON.stringify({
      file_path: "src/a.ts",
      old_string: "const a = 1",
      new_string: "const a = 2",
    }),
    state: "output-available",
    output: "ok",
  }
}

function runningCommandWithOutput(output: string): AdaptedToolCallPart {
  return {
    type: "tool-call",
    toolCallId: "bash-1",
    toolName: "bash",
    input: JSON.stringify({ command: "yes" }),
    state: "input-available",
    output,
  }
}

function groupOf50Tools(): Extract<
  import("@/lib/adapters/ai-elements-adapter").AdaptedContentPart,
  { type: "tool-group" }
> {
  const items: AdaptedToolCallPart[] = Array.from({ length: 50 }, (_, i) => ({
    type: "tool-call",
    toolCallId: `t-${i}`,
    toolName: "read",
    input: JSON.stringify({ file_path: `f${i}.ts` }),
    state: "output-available",
    output: "done",
  }))
  return { type: "tool-group", items, isStreaming: false }
}

describe("ContentPartsRenderer lazy tools", () => {
  beforeEach(() => {
    vi.restoreAllMocks()
  })

  it("does not construct collapsed group children", () => {
    wrap(<ContentPartsRenderer parts={[groupOf50Tools()]} />)
    // Only the group trigger is a button while collapsed.
    expect(screen.getAllByRole("button")).toHaveLength(1)
    fireEvent.click(screen.getByRole("button"))
    // Group trigger + 50 tool headers.
    expect(screen.getAllByRole("button")).toHaveLength(51)
  })

  it("defers structured input and diff parsing until expansion", () => {
    const generateUnifiedDiffSpy = vi.spyOn(unifiedDiff, "generateUnifiedDiff")
    // Real module spy (not a throwaway local object) — StructuredToolInput /
    // EditToolInput import tryParseJson from @/lib/try-parse-json.
    const parseStructuredInputSpy = vi.spyOn(tryParseJsonMod, "tryParseJson")

    wrap(<ToolCallPart part={completedEditTool()} />)
    // Diff / body structured work must not run while collapsed.
    expect(generateUnifiedDiffSpy).not.toHaveBeenCalled()
    expect(screen.queryByTestId("unified-diff")).not.toBeInTheDocument()
    // Header may still parse lightly for +/- title stats (optional residual).
    const parseCallsWhileCollapsed = parseStructuredInputSpy.mock.calls.length

    fireEvent.click(screen.getByRole("button"))
    expect(generateUnifiedDiffSpy).toHaveBeenCalled()
    // Body StructuredToolInput performs additional tryParseJson on expand.
    expect(parseStructuredInputSpy.mock.calls.length).toBeGreaterThan(
      parseCallsWhileCollapsed
    )
    expect(screen.getByTestId("unified-diff")).toBeInTheDocument()
    parseStructuredInputSpy.mockRestore()
  })

  it("keeps running command output plain and bounded", () => {
    wrap(<ToolCallPart part={runningCommandWithOutput("x".repeat(30_000))} />)
    const log = screen.getByRole("log")
    expect(log.textContent?.length).toBeLessThanOrEqual(24_000 + 64)
    expect(screen.queryByTestId("markdown-response")).not.toBeInTheDocument()
  })

  it("does not parse edit body while collapsed after completion", () => {
    const generateUnifiedDiffSpy = vi.spyOn(unifiedDiff, "generateUnifiedDiff")
    wrap(<ToolCallPart part={completedEditTool()} />)
    expect(generateUnifiedDiffSpy).not.toHaveBeenCalled()
    // Expand once → parse once
    fireEvent.click(screen.getByRole("button"))
    const callsAfterOpen = generateUnifiedDiffSpy.mock.calls.length
    expect(callsAfterOpen).toBeGreaterThanOrEqual(1)
    // Collapse
    fireEvent.click(screen.getByRole("button"))
    expect(generateUnifiedDiffSpy.mock.calls.length).toBe(callsAfterOpen)
    expect(screen.queryByTestId("unified-diff")).not.toBeInTheDocument()
    // Re-expand → body remounts and may parse again (once per mount)
    fireEvent.click(screen.getByRole("button"))
    expect(generateUnifiedDiffSpy.mock.calls.length).toBeGreaterThan(
      callsAfterOpen
    )
  })

  it("unmounts body on mid-stream collapse and resumes appends when re-expanded", () => {
    // Non-command running tool: starts collapsed (manual expand). Avoid file
    // tools (read/edit hide duplicate result) so ToolOutput stays visible.
    const part: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "search-1",
      toolName: "grep",
      input: JSON.stringify({ pattern: "foo" }),
      state: "input-available",
      output: "line-1",
    }
    const { rerender } = wrap(<ToolCallPart part={part} />)
    // Collapsed: body (result output) unmounted.
    expect(screen.queryByText("line-1")).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole("button"))
    expect(screen.getByText("line-1")).toBeInTheDocument()

    // Collapse mid-stream → body unmounts.
    fireEvent.click(screen.getByRole("button"))
    expect(screen.queryByText("line-1")).not.toBeInTheDocument()

    // Further appends while collapsed stay unmounted.
    const appended: AdaptedToolCallPart = {
      ...part,
      output: "line-1\nline-2",
    }
    act(() => {
      rerender(
        <NextIntlClientProvider locale="en" messages={enMessages}>
          <ToolCallPart part={appended} />
        </NextIntlClientProvider>
      )
    })
    expect(screen.queryByText(/line-2/)).not.toBeInTheDocument()

    // Re-expand → body shows full currently-capped output including appends.
    fireEvent.click(screen.getByRole("button"))
    expect(screen.getByText(/line-2/)).toBeInTheDocument()

    // Further appends while expanded update the mounted body.
    const more: AdaptedToolCallPart = {
      ...appended,
      output: "line-1\nline-2\nline-3",
    }
    act(() => {
      rerender(
        <NextIntlClientProvider locale="en" messages={enMessages}>
          <ToolCallPart part={more} />
        </NextIntlClientProvider>
      )
    })
    expect(screen.getByText(/line-3/)).toBeInTheDocument()
  })
})

describe("ContentPartsRenderer thinking visibility", () => {
  it("omits reasoning when showThinking is false", () => {
    const reasoning: AdaptedContentPart = {
      type: "reasoning",
      content: "private chain",
      isStreaming: false,
    }
    wrap(<ContentPartsRenderer parts={[reasoning]} showThinking={false} />)
    expect(screen.queryByText("private chain")).not.toBeInTheDocument()
  })

  it("omits reasoning nested in a goal run", () => {
    const start: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "goal-1",
      toolName: "update_goal",
      input: null,
      state: "input-available",
    }
    const goalRun: AdaptedContentPart = {
      type: "goal-run",
      start,
      end: null,
      items: [
        {
          type: "reasoning",
          content: "nested private chain",
          isStreaming: false,
        },
        { type: "text", text: "visible result" },
      ],
      isRunning: false,
    }
    wrap(<ContentPartsRenderer parts={[goalRun]} showThinking={false} />)
    fireEvent.click(screen.getByRole("button"))
    expect(screen.queryByText("nested private chain")).not.toBeInTheDocument()
    expect(screen.getByText("visible result")).toBeInTheDocument()
  })
})
