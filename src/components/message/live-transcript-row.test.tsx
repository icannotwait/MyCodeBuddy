import { act, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type {
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import { adaptMessageTurn } from "@/lib/adapters/ai-elements-adapter"
import type { AcceptedConnectionFrame, EventEnvelope } from "@/lib/types"
import enMessages from "@/i18n/messages/en.json"
import {
  __resetLiveTranscriptStoreForTests,
  getToolJoinedOutput,
  LIVE_RUNNING_OUTPUT_TAIL_CHARS,
  liveTranscriptStore,
  selectRunningOutputTail,
} from "@/stores/live-transcript-store"

// Streamdown / markdown stack is heavy in unit tests.
vi.mock("@/components/ai-elements/message", () => ({
  Message: ({
    children,
    ...rest
  }: {
    children?: React.ReactNode
    from?: string
  }) => (
    <div data-testid="message" {...rest}>
      {children}
    </div>
  ),
  MessageContent: ({ children }: { children?: React.ReactNode }) => (
    <div data-testid="message-content">{children}</div>
  ),
  MessageResponse: ({ children }: { children?: React.ReactNode }) => (
    <div data-testid="message-response">{children}</div>
  ),
  MessageAction: ({ children }: { children?: React.ReactNode }) => (
    <button type="button">{children}</button>
  ),
}))

vi.mock("@/components/ai-elements/reasoning", () => ({
  Reasoning: ({ children }: { children?: React.ReactNode }) => (
    <div data-testid="reasoning">{children}</div>
  ),
  ReasoningTrigger: () => <div data-testid="reasoning-trigger" />,
  ReasoningContent: ({ children }: { children?: React.ReactNode }) => (
    <div data-testid="reasoning-content">{children}</div>
  ),
}))

vi.mock("./content-parts-renderer", () => ({
  ContentPartsRenderer: ({
    parts,
  }: {
    parts: Array<{ type: string; toolName?: string; toolCallId?: string }>
  }) => (
    <div data-testid="tool-parts">
      {parts.map((p, i) => (
        <div
          key={i}
          data-part-type={p.type}
          data-tool-id={p.toolCallId}
          data-testid={p.toolCallId ? `tool-part-${p.toolCallId}` : undefined}
        >
          {p.toolName ?? p.type}
        </div>
      ))}
    </div>
  ),
  ToolCallPart: ({
    part,
  }: {
    part: { toolCallId?: string; toolName?: string }
  }) => (
    <div
      data-testid={part.toolCallId ? `tool-part-${part.toolCallId}` : "tool"}
    >
      {part.toolName}
    </div>
  ),
}))

import { adaptLiveToolPart, LiveTranscriptRow } from "./live-transcript-row"

const CID = 77

function liveMessage(text: string, id = "live-1"): LiveMessage {
  return {
    id,
    role: "assistant",
    content: [{ type: "text", text }],
    startedAt: 1_700_000_000_000,
  }
}

function contentDelta(
  connectionId: string,
  seq: number,
  text: string
): EventEnvelope {
  return {
    connection_id: connectionId,
    seq,
    type: "content_delta",
    text,
  }
}

function frame(
  applyEvents: EventEnvelope[],
  connectionId = "c1",
  highestSeq?: number
): AcceptedConnectionFrame {
  const seqs = applyEvents.map((e) => e.seq)
  return {
    contextKey: "tab-1",
    connectionId,
    deliveryIds: [1],
    applyEvents,
    rawEvents: applyEvents,
    highestSeq: highestSeq ?? (seqs.length > 0 ? Math.max(...seqs) : 0),
  }
}

function tool(
  id: string,
  over: Partial<ToolCallInfo> & { raw_output?: string } = {}
): ToolCallInfo {
  const chunks =
    over.raw_output_chunks ?? (over.raw_output != null ? [over.raw_output] : [])
  return {
    tool_call_id: id,
    title: over.title ?? id,
    kind: over.kind ?? "other",
    status: over.status ?? "in_progress",
    content: over.content ?? null,
    raw_input: over.raw_input ?? null,
    raw_output_chunks: chunks,
    raw_output_total_bytes:
      over.raw_output_total_bytes ?? chunks.join("").length,
    locations: over.locations ?? null,
    meta: over.meta ?? null,
    images: over.images ?? [],
  }
}

function seedLiveTools(conversationId: number, tools: ToolCallInfo[]): void {
  const msg: LiveMessage = {
    id: "live-tools",
    role: "assistant",
    content: tools.map((info) => ({ type: "tool_call" as const, info })),
    startedAt: 1,
  }
  liveTranscriptStore.rebuild(conversationId, "c1", msg, tools.length)
}

function publishToolUpdate(
  conversationId: number,
  toolCallId: string,
  patch: { status?: string; raw_output?: string; raw_output_append?: boolean }
): void {
  const existing = liveTranscriptStore.getTool(conversationId, toolCallId)
  const snap = liveTranscriptStore.getConversation(conversationId)
  if (!existing || !snap) throw new Error("missing tool/snapshot")
  const seq = snap.lastAppliedSeq + 1
  const nextInfo: ToolCallInfo = {
    ...existing,
    status: (patch.status as ToolCallInfo["status"]) ?? existing.status,
    raw_output_chunks:
      patch.raw_output != null
        ? patch.raw_output_append
          ? [...existing.raw_output_chunks, patch.raw_output]
          : [patch.raw_output]
        : existing.raw_output_chunks,
  }
  const canonical: LiveMessage = {
    id: snap.messageId,
    role: "assistant",
    content: [...snap.tools.values()].map((info) => ({
      type: "tool_call" as const,
      info: info.tool_call_id === toolCallId ? nextInfo : info,
    })),
    startedAt: snap.startedAt,
  }
  liveTranscriptStore.publish(
    conversationId,
    frame([
      {
        connection_id: "c1",
        seq,
        type: "tool_call_update",
        tool_call_id: toolCallId,
        title: null,
        status: patch.status ?? null,
        content: null,
        raw_input: null,
        raw_output: patch.raw_output ?? null,
        raw_output_append: patch.raw_output_append,
        locations: null,
        meta: null,
      },
    ]),
    canonical
  )
}

function publishToolAppend(
  conversationId: number,
  toolCallId: string,
  chunk: string
): void {
  publishToolUpdate(conversationId, toolCallId, {
    raw_output: chunk,
    raw_output_append: true,
  })
}

function renderRow(onToolRender?: (id: string) => void, showThinking = true) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <LiveTranscriptRow
        conversationId={CID}
        agentType="codex"
        showThinking={showThinking}
        onToolRender={onToolRender}
      />
    </NextIntlClientProvider>
  )
}

describe("LiveTranscriptRow", () => {
  beforeEach(() => {
    __resetLiveTranscriptStoreForTests()
  })

  afterEach(() => {
    __resetLiveTranscriptStoreForTests()
  })

  it("does not mount a thinking segment when visibility is off", () => {
    const message: LiveMessage = {
      id: "thinking-only",
      role: "assistant",
      content: [{ type: "thinking", text: "hidden live thought" }],
      startedAt: 1,
    }
    liveTranscriptStore.rebuild(CID, "c1", message, 1)
    renderRow(undefined, false)
    expect(screen.queryByTestId("reasoning")).not.toBeInTheDocument()
    expect(screen.queryByTestId("live-transcript-row")).not.toBeInTheDocument()
  })

  it("keeps tools visible when a thinking segment is hidden", () => {
    const message: LiveMessage = {
      id: "thinking-and-tool",
      role: "assistant",
      content: [
        { type: "thinking", text: "hidden live thought" },
        { type: "tool_call", info: tool("visible-tool") },
      ],
      startedAt: 1,
    }
    liveTranscriptStore.rebuild(CID, "c1", message, 2)
    renderRow(undefined, false)
    expect(screen.queryByTestId("reasoning")).not.toBeInTheDocument()
    expect(screen.getByTestId("tool-part-visible-tool")).toBeInTheDocument()
  })

  it("shows a typing indicator when the live snapshot has no segments yet", () => {
    liveTranscriptStore.rebuild(
      CID,
      "c1",
      {
        id: "empty",
        role: "assistant",
        content: [],
        startedAt: 1,
      },
      0
    )
    renderRow()
    // No text content — three pulse dots from the pending indicator.
    expect(screen.queryByTestId("live-transcript-row")).toBeNull()
    expect(screen.getByTestId("message")).toBeInTheDocument()
  })

  it("renders text segments via narrow subscriptions", () => {
    liveTranscriptStore.rebuild(CID, "c1", liveMessage("hello"), 1)
    renderRow()
    expect(screen.getByTestId("live-transcript-row")).toBeInTheDocument()
    expect(screen.getByTestId("message-response")).toHaveTextContent("hello")
  })

  it("updates text without remounting the row when chunks append", () => {
    liveTranscriptStore.rebuild(CID, "c1", liveMessage("a"), 1)
    renderRow()
    expect(screen.getByTestId("message-response")).toHaveTextContent("a")

    act(() => {
      liveTranscriptStore.publish(
        CID,
        frame([contentDelta("c1", 2, "b")]),
        liveMessage("ab")
      )
    })
    expect(screen.getByTestId("message-response")).toHaveTextContent("ab")
    expect(screen.getByTestId("live-transcript-row")).toBeInTheDocument()
  })

  it("updates one tool card without rendering siblings", () => {
    const renders = new Map<string, number>()
    // Three tools form a live group — expand so cards mount, then isolate.
    seedLiveTools(CID, [tool("a"), tool("b"), tool("c")])
    renderRow((id) => renders.set(id, (renders.get(id) ?? 0) + 1))
    fireEvent.click(screen.getByRole("button"))
    expect(screen.getByTestId("tool-part-b")).toBeInTheDocument()
    renders.clear()
    act(() => {
      publishToolUpdate(CID, "b", { status: "completed" })
    })
    expect(renders).toEqual(new Map([["b", 1]]))
  })

  it("preserves ordered append chunks and visible tail cap", () => {
    seedLiveTools(CID, [tool("a", { raw_output: "head\n" })])
    act(() => {
      publishToolAppend(CID, "a", "one\n")
      publishToolAppend(CID, "a", "two\n")
    })
    const record = liveTranscriptStore.getTool(CID, "a")!
    expect(getToolJoinedOutput(record)).toBe("head\none\ntwo\n")
    // "head\none\ntwo\n".slice(-8) === "one\ntwo\n"
    expect(selectRunningOutputTail(record, 8)).toBe("one\ntwo\n")
  })

  it("collapses multi-tool groups to a summary until expanded", () => {
    seedLiveTools(CID, [
      tool("a", {
        title: "Read",
        kind: "read",
        raw_input: JSON.stringify({ file_path: "a.ts" }),
      }),
      tool("b", {
        title: "Read",
        kind: "read",
        raw_input: JSON.stringify({ file_path: "b.ts" }),
      }),
      tool("c", {
        title: "Read",
        kind: "read",
        raw_input: JSON.stringify({ file_path: "c.ts" }),
      }),
    ])
    const renders = new Map<string, number>()
    renderRow((id) => renders.set(id, (renders.get(id) ?? 0) + 1))

    // Collapsed: only the group trigger (no per-tool cards).
    expect(screen.getAllByRole("button")).toHaveLength(1)
    expect(renders.size).toBe(0)
    expect(screen.queryByTestId("tool-part-a")).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole("button"))
    // Expanded: LiveToolCard mounts for each group member.
    expect(screen.getByTestId("tool-part-a")).toBeInTheDocument()
    expect(screen.getByTestId("tool-part-b")).toBeInTheDocument()
    expect(screen.getByTestId("tool-part-c")).toBeInTheDocument()
    expect(renders.size).toBe(3)

    // Collapse again → children unmount.
    fireEvent.click(screen.getByRole("button"))
    expect(screen.queryByTestId("tool-part-a")).not.toBeInTheDocument()
  })

  it("caps running command adapt output via selectRunningOutputTail", () => {
    const huge = "x".repeat(30_000)
    const info = tool("bash-1", {
      title: "bash",
      kind: "execute",
      raw_input: JSON.stringify({ command: "yes" }),
      raw_output: huge,
      status: "in_progress",
    })
    const adapted = adaptLiveToolPart(info)
    expect(adapted.state).toBe("input-available")
    expect(adapted.output?.length).toBeLessThanOrEqual(
      LIVE_RUNNING_OUTPUT_TAIL_CHARS
    )
    expect(adapted.output).toBe(selectRunningOutputTail(info))
  })

  it("deep-equals completed adaptLiveToolPart with historical adapt path", () => {
    const input = JSON.stringify({
      file_path: "src/a.ts",
      old_string: "const a = 1",
      new_string: "const a = 2",
    })
    const output = "ok"
    const info = tool("tc-edit-1", {
      title: "Edit",
      kind: "edit",
      status: "completed",
      raw_input: input,
      raw_output: output,
    })
    const live = adaptLiveToolPart(info)

    const historical = adaptMessageTurn(
      {
        id: "hist-1",
        role: "assistant",
        timestamp: "2026-07-16T00:00:00.000Z",
        blocks: [
          {
            type: "tool_use",
            tool_use_id: "tc-edit-1",
            tool_name: live.toolName,
            input_preview: input,
            meta: null,
          },
          {
            type: "tool_result",
            tool_use_id: "tc-edit-1",
            output_preview: output,
            is_error: false,
          },
        ],
      },
      {
        attachedResources: "Attached resources",
        toolCallFailed: "failed",
      },
      false
    )
    const group = historical.content.find((p) => p.type === "tool-group")
    expect(group?.type).toBe("tool-group")
    if (group?.type !== "tool-group") throw new Error("expected tool-group")
    const histTool = group.items[0]
    // Core fields match the historical adapted tool-call (displayTitle is live-only).
    expect({
      type: live.type,
      toolCallId: live.toolCallId,
      toolName: live.toolName,
      input: live.input,
      state: live.state,
      output: live.output,
      errorText: live.errorText,
      meta: live.meta,
    }).toEqual({
      type: histTool.type,
      toolCallId: histTool.toolCallId,
      toolName: histTool.toolName,
      input: histTool.input,
      state: histTool.state,
      output: histTool.output,
      errorText: histTool.errorText,
      meta: histTool.meta ?? null,
    })
  })
})
