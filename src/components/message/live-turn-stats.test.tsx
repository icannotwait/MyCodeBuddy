import { act, cleanup, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  extractLiveEditStats,
  LIVE_TURN_REQUEST_USAGE_VISIBLE,
  LiveTurnStats,
} from "./live-turn-stats"
import type {
  LiveContentBlock,
  LiveMessage,
} from "@/contexts/acp-connections-context"
import arMessages from "@/i18n/messages/ar.json"
import deMessages from "@/i18n/messages/de.json"
import enMessages from "@/i18n/messages/en.json"
import esMessages from "@/i18n/messages/es.json"
import frMessages from "@/i18n/messages/fr.json"
import jaMessages from "@/i18n/messages/ja.json"
import koMessages from "@/i18n/messages/ko.json"
import ptMessages from "@/i18n/messages/pt.json"
import zhCNMessages from "@/i18n/messages/zh-CN.json"
import zhTWMessages from "@/i18n/messages/zh-TW.json"
import { publishRequestUsage } from "@/lib/request-usage-live"
import { EMPTY_REQUEST_USAGE } from "@/lib/request-usage-speed"

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

function renderUsage(conversationId: number) {
  const message = msg([textBlock("streaming")])
  message.startedAt = Date.now() - 10_000
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <LiveTurnStats
        message={message}
        agentType="codex"
        conversationId={conversationId}
        isStreaming
      />
    </NextIntlClientProvider>
  )
}

function expectCollapsedUsageSlot(
  testId: "output-speed-slot" | "generation-share-slot"
) {
  const el = screen.getByTestId(testId)
  expect(el).not.toHaveClass("invisible")
  expect(el).not.toHaveClass(
    testId === "output-speed-slot"
      ? "@[30rem]/turnstats:inline-flex"
      : "@[36rem]/turnstats:inline-flex"
  )
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
  })
})

describe.skipIf(LIVE_TURN_REQUEST_USAGE_VISIBLE)(
  "LiveTurnStats request usage visibility",
  () => {
    afterEach(() => {
      cleanup()
    })

    it("omits tok/s and generation-share from the streaming banner", () => {
      const message = msg([textBlock("streaming")])
      message.startedAt = Date.now() - 10_000
      publishRequestUsage(5_030, {
        outputTokens: 100,
        generationMs: 13_000,
        tps: 76.7,
        sampleCount: 1,
        estimatedSampleCount: 1,
      })
      render(
        <NextIntlClientProvider locale="en" messages={enMessages}>
          <LiveTurnStats
            message={message}
            agentType="codex"
            conversationId={5_030}
            isStreaming
          />
        </NextIntlClientProvider>
      )

      expect(screen.queryByTestId("output-speed-slot")).not.toBeInTheDocument()
      expect(
        screen.queryByTestId("generation-share-slot")
      ).not.toBeInTheDocument()
      expect(screen.queryByText(/tok\/s/)).not.toBeInTheDocument()
    })
  }
)

describe.skipIf(!LIVE_TURN_REQUEST_USAGE_VISIBLE)(
  "LiveTurnStats request usage transition",
  () => {
    beforeEach(() => {
      vi.useFakeTimers()
      vi.setSystemTime(new Date("2026-08-21T00:00:00.000Z"))
      const startedAt = Date.now()
      vi.spyOn(performance, "now").mockImplementation(
        () => Date.now() - startedAt
      )
    })

    afterEach(() => {
      cleanup()
      vi.unstubAllGlobals()
      vi.restoreAllMocks()
      vi.useRealTimers()
    })

    it.each([
      ["empty", 5_001, EMPTY_REQUEST_USAGE],
      [
        "non-finite",
        5_009,
        {
          outputTokens: 10,
          generationMs: Number.NaN,
          tps: Number.NaN,
          sampleCount: 1,
          estimatedSampleCount: 0,
        },
      ],
      [
        "non-positive",
        5_010,
        {
          outputTokens: 10,
          generationMs: 0,
          tps: -1,
          sampleCount: 1,
          estimatedSampleCount: 0,
        },
      ],
    ] as const)("hides %s metrics", (_name, conversationId, snapshot) => {
      publishRequestUsage(conversationId, snapshot)
      renderUsage(conversationId)
      act(() => vi.advanceTimersByTime(5_016))

      expect(screen.queryByText(/tok\/s$/)).not.toBeInTheDocument()
      expectCollapsedUsageSlot("output-speed-slot")
      expectCollapsedUsageSlot("generation-share-slot")
    })

    it("does not reserve centered-row space for empty usage metrics", () => {
      // `invisible` + fixed widths on unused tok/s and generation slots sat to
      // the right of "streaming | 26s" inside a justify-center row, so the
      // visible status looked left-of-center for the whole turn.
      publishRequestUsage(5_020, EMPTY_REQUEST_USAGE)
      renderUsage(5_020)

      expectCollapsedUsageSlot("output-speed-slot")
      expectCollapsedUsageSlot("generation-share-slot")
    })

    it("sizes visible usage slots to their content, not a leftover rem box", () => {
      // w-[7.5rem] / w-[8.5rem] left a gap after "tok/s ≈" and "13s (37%) ≈"
      // because the copy is shorter than those boxes and the slots are
      // left-aligned.
      publishRequestUsage(5_021, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 76.7,
        sampleCount: 1,
        estimatedSampleCount: 1,
      })
      renderUsage(5_021)
      act(() => vi.advanceTimersByTime(5_016))

      expect(screen.getByTestId("output-speed-slot")).not.toHaveClass(
        "w-[7.5rem]"
      )
      expect(screen.getByTestId("generation-share-slot")).not.toHaveClass(
        "w-[8.5rem]"
      )
      expect(screen.getByText(/tok\/s$/)).toBeInTheDocument()
      expect(screen.getByTestId("generation-share-slot")).toHaveTextContent(
        /\(\d+%\)/
      )
    })

    it("hides a settled sub-second generation duration", () => {
      publishRequestUsage(5_013, {
        outputTokens: 10,
        generationMs: 999,
        tps: 10,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_013)
      act(() => vi.advanceTimersByTime(5_016))

      expectCollapsedUsageSlot("generation-share-slot")
    })

    it("hides the generation slot during the formatted-zero transition", () => {
      publishRequestUsage(5_014, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_014)
      act(() => vi.advanceTimersByTime(33))

      expect(screen.getByTestId("generation-share-slot")).toHaveClass(
        "invisible"
      )
    })

    it("hides formatted-zero speed without hiding valid generation share", () => {
      publishRequestUsage(5_011, {
        outputTokens: 1,
        generationMs: 1_000,
        tps: 0.04,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_011)
      act(() => vi.advanceTimersByTime(5_016))

      expect(screen.queryByText("0.0 tok/s")).not.toBeInTheDocument()
      expectCollapsedUsageSlot("output-speed-slot")
      expect(screen.getByTestId("generation-share-slot")).not.toHaveClass(
        "invisible"
      )
      expect(screen.getByTestId("generation-share-slot")).toHaveTextContent(
        /\(7%\)/
      )
    })

    it("ticks every 33ms and reaches the exact target on the 5016ms tick", () => {
      publishRequestUsage(5_002, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_002)

      expect(screen.queryByText("0.0 tok/s")).not.toBeInTheDocument()
      act(() => vi.advanceTimersByTime(32))
      expect(screen.getByTestId("output-speed-slot")).toHaveClass("invisible")
      act(() => vi.advanceTimersByTime(1))
      expect(screen.getByTestId("output-speed-slot")).not.toHaveClass(
        "invisible"
      )
      expect(screen.getByText(/tok\/s$/)).not.toHaveTextContent("100.0 tok/s")

      act(() => vi.advanceTimersByTime(4_950))
      expect(vi.getTimerCount()).toBe(2)
      act(() => vi.advanceTimersByTime(33))
      expect(screen.getByText(/tok\/s$/)).toHaveTextContent("100.0 tok/s")
      expect(vi.getTimerCount()).toBe(1)
    })

    it("replaces a target from the current interpolated value", () => {
      publishRequestUsage(5_003, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_003)
      act(() => vi.advanceTimersByTime(990))
      const before = Number.parseFloat(
        screen.getByText(/tok\/s$/).textContent ?? "0"
      )

      act(() => {
        publishRequestUsage(5_003, {
          outputTokens: 400,
          generationMs: 2_000,
          tps: 200,
          sampleCount: 2,
          estimatedSampleCount: 0,
        })
      })
      const atReplacement = Number.parseFloat(
        screen.getByText(/tok\/s$/).textContent ?? "0"
      )
      expect(atReplacement).toBeCloseTo(before, 1)

      act(() => vi.advanceTimersByTime(33))
      const after = Number.parseFloat(
        screen.getByText(/tok\/s$/).textContent ?? "0"
      )
      expect(after).toBeGreaterThan(atReplacement)
      expect(after).toBeLessThan(200)
    })

    it("hides immediately on reset and clears target work on unmount", () => {
      publishRequestUsage(5_004, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      const view = renderUsage(5_004)
      act(() => vi.advanceTimersByTime(330))
      expect(vi.getTimerCount()).toBe(2)

      act(() => publishRequestUsage(5_004, EMPTY_REQUEST_USAGE))
      expectCollapsedUsageSlot("output-speed-slot")
      expect(vi.getTimerCount()).toBe(1)

      view.unmount()
      expect(vi.getTimerCount()).toBe(0)
    })

    it("resets the transition when conversation identity changes", () => {
      publishRequestUsage(5_005, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      const message = msg([textBlock("streaming")])
      message.startedAt = Date.now() - 10_000
      const view = render(
        <NextIntlClientProvider locale="en" messages={enMessages}>
          <LiveTurnStats
            message={message}
            agentType="codex"
            conversationId={5_005}
          />
        </NextIntlClientProvider>
      )
      act(() => vi.advanceTimersByTime(330))
      publishRequestUsage(5_006, EMPTY_REQUEST_USAGE)
      view.rerender(
        <NextIntlClientProvider locale="en" messages={enMessages}>
          <LiveTurnStats
            message={message}
            agentType="codex"
            conversationId={5_006}
          />
        </NextIntlClientProvider>
      )

      expectCollapsedUsageSlot("output-speed-slot")
      expect(vi.getTimerCount()).toBe(1)
    })

    it("keeps the five-second transition when reduced motion is requested", () => {
      vi.stubGlobal(
        "matchMedia",
        vi.fn().mockReturnValue({
          matches: true,
          media: "(prefers-reduced-motion: reduce)",
          onchange: null,
          addEventListener: vi.fn(),
          removeEventListener: vi.fn(),
          addListener: vi.fn(),
          removeListener: vi.fn(),
          dispatchEvent: vi.fn(),
        })
      )
      publishRequestUsage(5_012, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_012)

      act(() => vi.advanceTimersByTime(33))
      expect(screen.getByText(/tok\/s$/)).not.toHaveTextContent("100.0 tok/s")
    })

    it("shows one accessible estimate marker for each approximate metric", async () => {
      publishRequestUsage(5_007, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 1,
      })
      renderUsage(5_007)
      act(() => vi.advanceTimersByTime(5_016))

      const markers = screen.getAllByRole("button", {
        name: enMessages.Folder.chat.liveTurnStats.estimatedAria,
      })
      expect(markers).toHaveLength(2)
      expect(markers[0]).toHaveTextContent("≈")
      act(() => markers[0].focus())
      expect(markers[0]).toHaveFocus()
      act(() => markers[0].blur())
      act(() => {
        fireEvent.click(markers[0])
        vi.advanceTimersByTime(0)
      })
      expect(screen.getByRole("tooltip")).toHaveTextContent(
        enMessages.Folder.chat.liveTurnStats.estimatedTooltip
      )
    })

    it("shows no approximation marker for exact-only usage", () => {
      publishRequestUsage(5_008, {
        outputTokens: 100,
        generationMs: 1_000,
        tps: 100,
        sampleCount: 1,
        estimatedSampleCount: 0,
      })
      renderUsage(5_008)
      act(() => vi.advanceTimersByTime(33))

      expect(
        screen.queryByRole("button", {
          name: enMessages.Folder.chat.liveTurnStats.estimatedAria,
        })
      ).not.toBeInTheDocument()
    })
  }
)

describe("LiveTurnStats request usage copy", () => {
  it.each([
    ["ar", arMessages],
    ["de", deMessages],
    ["en", enMessages],
    ["es", esMessages],
    ["fr", frMessages],
    ["ja", jaMessages],
    ["ko", koMessages],
    ["pt", ptMessages],
    ["zh-CN", zhCNMessages],
    ["zh-TW", zhTWMessages],
  ])("provides nonempty approximation copy for %s", (_locale, messages) => {
    expect(messages.Folder.chat.liveTurnStats.estimatedAria.trim()).not.toBe("")
    expect(messages.Folder.chat.liveTurnStats.estimatedTooltip.trim()).not.toBe(
      ""
    )
  })
})
