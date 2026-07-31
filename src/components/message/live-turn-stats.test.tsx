import { render, screen, cleanup } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, describe, expect, it } from "vitest"

import { extractLiveEditStats, LiveTurnStats } from "./live-turn-stats"
import type {
  LiveContentBlock,
  LiveMessage,
} from "@/contexts/acp-connections-context"
import enMessages from "@/i18n/messages/en.json"

// --- fixtures --------------------------------------------------------------

let toolIdCounter = 0

// A completed tool_call block with a deliberately NON-classifying title/kind
// ("tool"), so the tool is classified purely by `raw_input` shape. This means a
// regression in input-shape detection can't be masked by a title/kind fallback.
function toolBlock(rawInput: string): LiveContentBlock {
  toolIdCounter += 1
  return {
    type: "tool_call",
    info: {
      tool_call_id: `tc-${toolIdCounter}`,
      title: "tool",
      kind: "tool",
      status: "completed",
      content: null,
      raw_input: rawInput,
      raw_output_chunks: [],
      raw_output_total_bytes: 0,
      locations: null,
      meta: null,
      images: [],
    },
  }
}

function textBlock(text: string): LiveContentBlock {
  return { type: "text", text }
}

function msg(content: LiveContentBlock[]): LiveMessage {
  return { id: "m1", role: "assistant", content, startedAt: 0 }
}

// `{content, file_path}` → classified as "write"; additions = line count.
const writeInput = (content: string, filePath: string) =>
  JSON.stringify({ content, file_path: filePath })

// A minimal codex-style patch → classified as "apply_patch".
const applyPatch = (body: string) => `*** Begin Patch\n${body}\n*** End Patch`

// --- tests -----------------------------------------------------------------

describe("extractLiveEditStats", () => {
  it("counts a write tool's added lines and file", () => {
    const stats = extractLiveEditStats(
      msg([toolBlock(writeInput("a\nb\nc", "x.ts"))])
    )
    expect(stats).toEqual({ files: 1, additions: 3, deletions: 0 })
  })

  it("counts an apply_patch tool's added lines and file", () => {
    const stats = extractLiveEditStats(
      msg([toolBlock(applyPatch("*** Add File: new.ts\n+alpha\n+beta"))])
    )
    expect(stats).toEqual({ files: 1, additions: 2, deletions: 0 })
  })

  it("dedupes files and sums line counts across blocks", () => {
    const stats = extractLiveEditStats(
      msg([
        toolBlock(writeInput("a", "same.ts")),
        toolBlock(writeInput("b\nc", "same.ts")),
      ])
    )
    expect(stats).toEqual({ files: 1, additions: 3, deletions: 0 })
  })

  it("ignores non-edit blocks", () => {
    const stats = extractLiveEditStats(
      msg([textBlock("hello"), toolBlock('{"command":"ls"}')])
    )
    expect(stats).toEqual({ files: 0, additions: 0, deletions: 0 })
  })

  it("returns a stable result when called repeatedly (cache hit)", () => {
    const message = msg([toolBlock(writeInput("a\nb", "x.ts"))])
    const first = extractLiveEditStats(message)
    const second = extractLiveEditStats(message)
    expect(second).toEqual(first)
    expect(first).toEqual({ files: 1, additions: 2, deletions: 0 })
  })

  it("reuses a cached block's contribution when it reappears in a new message", () => {
    // The reducer preserves an unchanged block's reference across streaming
    // tokens, so the same block object shows up in successive messages. The
    // per-block cache must still aggregate it correctly alongside new blocks.
    const shared = toolBlock(writeInput("a\nb\nc", "x.ts"))
    const before = extractLiveEditStats(msg([shared]))
    expect(before).toEqual({ files: 1, additions: 3, deletions: 0 })

    const added = toolBlock(writeInput("p\nq", "z.ts"))
    const after = extractLiveEditStats(msg([shared, added]))
    expect(after).toEqual({ files: 2, additions: 5, deletions: 0 })
  })
})

describe("LiveTurnStats status label", () => {
  afterEach(() => {
    cleanup()
  })

  it("replaces streaming with waiting-for-subagents while keeping tool metrics", () => {
    const message = msg([
      textBlock("hi"),
      toolBlock(writeInput("a\nb", "x.ts")),
      toolBlock('{"command":"ls"}'),
    ])
    // startedAt far enough in the past that elapsed is non-zero once mounted
    message.startedAt = Date.now() - 5_000

    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <LiveTurnStats
          message={message}
          agentType="codex"
          isStreaming
          statusMode="waiting_for_subagents"
        />
      </NextIntlClientProvider>
    )

    expect(screen.getByTestId("live-turn-stats")).toHaveAttribute(
      "data-status-mode",
      "waiting_for_subagents"
    )
    expect(screen.getByTestId("live-turn-stats-status")).toHaveTextContent(
      enMessages.Folder.chat.liveTurnStats.waitingForSubagents
    )
    expect(
      screen.queryByText(enMessages.Folder.chat.liveTurnStats.streaming)
    ).not.toBeInTheDocument()
    // tool uses still render (2 tool_call blocks)
    expect(screen.getByText(/2 tool/)).toBeInTheDocument()
  })
})
