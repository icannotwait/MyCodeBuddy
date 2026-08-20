import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { toast } from "sonner"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { copyTextToClipboard } from "@/lib/utils"
import type {
  AdaptedContentPart,
  AdaptedGoalRunPart,
  AdaptedToolCallPart,
} from "@/lib/adapters/ai-elements-adapter"
import enMessages from "@/i18n/messages/en.json"
import * as tryParseJsonMod from "@/lib/try-parse-json"
import * as unifiedDiff from "@/lib/unified-diff-generator"
import {
  appendStreamingMarkdown,
  cacheCompletedStreamingPartition,
  clearCompletedStreamingPartitions,
  completeStreamingMarkdown,
  createIncrementalStreamBlocks,
} from "@/lib/markdown/incremental-stream-blocks"

type AdaptedTextPart = Extract<AdaptedContentPart, { type: "text" }>

vi.mock("@/components/ai-elements/message", () => ({
  MessageResponse: ({
    children,
    autolinkLocalPaths,
  }: {
    children?: React.ReactNode
    autolinkLocalPaths?: boolean
  }) => (
    <div
      data-testid="markdown-response"
      data-autolink-local-paths={String(!!autolinkLocalPaths)}
    >
      {children}
    </div>
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

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}))

vi.mock("@/lib/utils", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/utils")>()
  return { ...actual, copyTextToClipboard: vi.fn().mockResolvedValue(true) }
})

vi.mock("./delegated-sub-thread", () => ({
  DelegatedSubThread: ({
    parentToolUseId,
    workUnitKey,
    workUnitSources,
  }: {
    parentToolUseId: string
    workUnitKey?: string | null
    workUnitSources?: unknown[]
  }) => (
    <div
      data-testid="delegated-sub-thread"
      data-tool-use-id={parentToolUseId}
      data-work-unit-key={workUnitKey ?? undefined}
      data-source-count={workUnitSources?.length ?? 0}
    />
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

beforeEach(() => {
  clearCompletedStreamingPartitions()
})

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

describe("ContentPartsRenderer delegation dispatch", () => {
  it("renders continue_delegation through the per-run delegation card", () => {
    wrap(
      <ToolCallPart
        part={{
          type: "tool-call",
          toolCallId: "continue-1",
          toolName: "mcp__codeg-mcp__continue_delegation",
          input: JSON.stringify({ task_id: "run-1", task: "Review the fix" }),
          state: "output-available",
          output: JSON.stringify({ task_id: "run-2" }),
        }}
      />
    )

    expect(screen.getByTestId("delegated-sub-thread")).toHaveAttribute(
      "data-tool-use-id",
      "continue-1"
    )
  })

  it("renders one canonical work-unit card from its latest source", () => {
    const source = (toolCallId: string): AdaptedToolCallPart => ({
      type: "tool-call",
      toolCallId,
      toolName: "delegate_to_agent",
      input: JSON.stringify({ task: toolCallId }),
      state: "output-available",
      output: JSON.stringify({ task_id: `run-${toolCallId}` }),
    })
    wrap(
      <ContentPartsRenderer
        role="assistant"
        parentConversationId={2075}
        parts={[
          {
            type: "delegation-work-unit",
            key: "wu:unit-a",
            sources: [source("tool-1"), source("tool-2")],
            explicitUserCancel: false,
          },
        ]}
      />
    )

    expect(screen.getAllByTestId("delegated-sub-thread")).toHaveLength(1)
    expect(screen.getByTestId("delegated-sub-thread")).toHaveAttribute(
      "data-tool-use-id",
      "tool-2"
    )
    expect(screen.getByTestId("delegated-sub-thread")).toHaveAttribute(
      "data-work-unit-key",
      "wu:unit-a"
    )
    expect(screen.getByTestId("delegated-sub-thread")).toHaveAttribute(
      "data-source-count",
      "2"
    )
  })
})

describe("ContentPartsRenderer local-path autolink scope", () => {
  it("requires assistant role and membership in the eligible set", () => {
    const part: AdaptedTextPart = {
      type: "text",
      text: String.raw`D:\repo\src\app.ts`,
    }
    const eligible = new Set<AdaptedTextPart>([part])
    const { rerender } = wrap(
      <ContentPartsRenderer parts={[part]} role="assistant" />
    )
    expect(screen.getByTestId("markdown-response")).toHaveAttribute(
      "data-autolink-local-paths",
      "false"
    )

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ContentPartsRenderer
          parts={[part]}
          role="assistant"
          autolinkLocalPathParts={eligible}
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByTestId("markdown-response")).toHaveAttribute(
      "data-autolink-local-paths",
      "true"
    )

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ContentPartsRenderer
          parts={[part]}
          role="system"
          autolinkLocalPathParts={eligible}
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByTestId("markdown-response")).toHaveAttribute(
      "data-autolink-local-paths",
      "false"
    )
  })

  it("keeps user text on the plain-text renderer", () => {
    const part: AdaptedTextPart = {
      type: "text",
      text: String.raw`D:\repo\src\app.ts`,
    }
    wrap(
      <ContentPartsRenderer
        parts={[part]}
        role="user"
        autolinkLocalPathParts={new Set([part])}
      />
    )
    expect(screen.queryByTestId("markdown-response")).toBeNull()
  })

  it("does not inherit the opt-in inside a structured goal run", () => {
    const start: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "goal-1",
      toolName: "create_goal",
      input: JSON.stringify({ objective: "test" }),
      state: "output-error",
      errorText: "failed",
    }
    const nested: AdaptedTextPart = {
      type: "text",
      text: String.raw`D:\nested\src\app.ts`,
    }
    const goal: AdaptedGoalRunPart = {
      type: "goal-run",
      start,
      end: null,
      items: [nested],
      isRunning: false,
    }
    wrap(
      <ContentPartsRenderer
        parts={[goal]}
        role="assistant"
        autolinkLocalPathParts={new Set([nested])}
      />
    )
    expect(screen.getByTestId("markdown-response")).toHaveAttribute(
      "data-autolink-local-paths",
      "false"
    )
  })

  it("enables the opt-in after a completed partition handoff", () => {
    const text = String.raw`D:\repo\src\app.ts`
    const part: AdaptedTextPart = { type: "text", text }
    let document = createIncrementalStreamBlocks("completed-assistant")
    document = appendStreamingMarkdown(document, text)
    document = completeStreamingMarkdown(document)
    expect(cacheCompletedStreamingPartition(text, document)).toBe(true)

    wrap(
      <ContentPartsRenderer
        parts={[part]}
        role="assistant"
        autolinkLocalPathParts={new Set([part])}
      />
    )
    expect(screen.getByTestId("markdown-response")).toHaveAttribute(
      "data-autolink-local-paths",
      "true"
    )
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

function largeWriteTool(lineCount: number): {
  part: AdaptedToolCallPart
  content: string
} {
  const content = Array.from(
    { length: lineCount },
    (_, i) => `line ${i + 1}`
  ).join("\n")
  return {
    content,
    part: {
      type: "tool-call",
      toolCallId: "write-large",
      toolName: "write",
      input: JSON.stringify({
        content,
      }),
      state: "output-available",
      output: "ok",
    },
  }
}

describe("ContentPartsRenderer large file copy controls", () => {
  beforeEach(() => {
    vi.mocked(copyTextToClipboard).mockReset()
    vi.mocked(copyTextToClipboard).mockResolvedValue(false)
    vi.mocked(toast.error).mockReset()
  })

  it("uses translated hidden-line and copy text, and reports a localized copy failure", async () => {
    const { part, content } = largeWriteTool(401)
    wrap(<ToolCallPart part={part} />)
    fireEvent.click(screen.getByRole("button"))

    expect(screen.getByText("1 more lines")).toBeInTheDocument()
    const copyAll = screen.getByRole("button", { name: "Copy all" })
    fireEvent.click(copyAll)

    await waitFor(() => {
      expect(copyTextToClipboard).toHaveBeenCalledWith(content)
      expect(toast.error).toHaveBeenCalledWith(
        "Could not copy the full file content"
      )
    })
  })
})
