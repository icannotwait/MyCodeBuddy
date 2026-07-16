import { act, render, screen, cleanup } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"
import { forwardRef, useImperativeHandle, type Ref } from "react"
import type { LiveMessage } from "@/contexts/acp-connections-context"
import type {
  AcceptedConnectionFrame,
  EventEnvelope,
  MessageTurn,
} from "@/lib/types"
import enMessages from "@/i18n/messages/en.json"
import {
  canReloadSessionLoadError,
  singletonSourceTurns,
} from "./message-list-view"
import {
  completeLiveTranscriptTurn,
  resetConversationRuntimeStore,
  selectHistoricalTimelineTurns,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import {
  __resetLiveTranscriptStoreForTests,
  liveTranscriptStore,
} from "@/stores/live-transcript-store"
import {
  __resetStreamingPerformanceConfigForTests,
  initializeStreamingPerformanceConfig,
} from "@/lib/acp/streaming-performance-config"

// virtua / stick-to-bottom / heavy markdown — keep list tests focused.
vi.mock("virtua", () => ({
  Virtualizer: forwardRef(function VirtualizerMock(
    props: { children?: ReactNode },
    ref: Ref<{ scrollToIndex: (i: number) => void }>
  ) {
    useImperativeHandle(ref, () => ({ scrollToIndex: vi.fn() }))
    return (
      <div data-testid="virtua-root">
        {Array.isArray(props.children)
          ? props.children.map((child, index) => (
              <div key={index} data-virtua-item>
                {child}
              </div>
            ))
          : props.children}
      </div>
    )
  }),
}))

const listScrollToBottom = vi.fn()
const listStopScroll = vi.fn()
vi.mock("use-stick-to-bottom", () => ({
  useStickToBottomContext: () => ({
    scrollRef: { current: document.createElement("div") },
    scrollToBottom: listScrollToBottom,
    stopScroll: listStopScroll,
    isAtBottom: true,
  }),
  StickToBottom: Object.assign(
    ({
      children,
      resize,
      ...rest
    }: {
      children?: ReactNode
      role?: string
      resize?: string
    }) => (
      <div
        role={rest.role ?? "log"}
        data-testid="message-thread"
        data-resize={resize ?? ""}
      >
        {children}
      </div>
    ),
    {
      Content: ({
        children,
        className,
      }: {
        children?: ReactNode
        className?: string
      }) => (
        <div className={className} data-testid="thread-content">
          {children}
        </div>
      ),
    }
  ),
}))

vi.mock("@/components/ai-elements/message", () => ({
  Message: ({ children, ...rest }: { children?: ReactNode; from?: string }) => (
    <div data-testid="ai-message" data-from={rest.from}>
      {children}
    </div>
  ),
  MessageContent: ({ children }: { children?: ReactNode }) => (
    <div>{children}</div>
  ),
  MessageResponse: ({ children }: { children?: ReactNode }) => (
    <div data-testid="message-response">{children}</div>
  ),
  MessageAction: ({ children }: { children?: ReactNode }) => (
    <button type="button">{children}</button>
  ),
}))

vi.mock("@/components/ai-elements/reasoning", () => ({
  Reasoning: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  ReasoningTrigger: () => null,
  ReasoningContent: ({ children }: { children?: ReactNode }) => (
    <div>{children}</div>
  ),
}))

vi.mock("./content-parts-renderer", () => ({
  ContentPartsRenderer: ({
    parts,
  }: {
    parts: Array<{ type: string; text?: string }>
  }) => (
    <div data-testid="content-parts">
      {parts.map((p, i) =>
        p.type === "text" ? (
          <span key={i} data-testid="assistant-text">
            {p.text}
          </span>
        ) : (
          <span key={i} data-part={p.type} />
        )
      )}
    </div>
  ),
}))

vi.mock("./live-turn-stats", () => ({
  LiveTurnStats: () => <div data-testid="live-turn-stats" />,
}))

vi.mock("./turn-stats", () => ({
  TurnStats: () => null,
}))

vi.mock("./reply-artifacts", () => ({
  ReplyArtifacts: () => null,
}))

vi.mock("@/components/chat/agent-plan-overlay", () => ({
  AgentPlanOverlay: () => null,
}))

vi.mock("@/components/chat/sub-agent-overlay", () => ({
  SubAgentOverlay: () => null,
}))

vi.mock("@/contexts/session-stats-context", () => ({
  useSessionStats: () => ({ setSessionStats: vi.fn() }),
}))

vi.mock("./conversation-message-nav", () => ({
  ConversationMessageNav: () => null,
}))

const historicalRenderSpy = vi.fn()
const liveRenderSpy = vi.fn()

vi.mock("@/lib/perf/streaming-perf-recorder", () => ({
  streamingPerfRecorder: {
    countRender: (kind: string) => {
      if (kind === "historicalRow" || kind === "historicalThread") {
        historicalRenderSpy(kind)
      }
      if (kind === "liveRow") {
        liveRenderSpy(kind)
      }
    },
    markReactCommit: vi.fn(),
    isActive: () => false,
  },
}))

import { MessageListView } from "./message-list-view"

const CID = 501

function userTurn(id: string, text = id): MessageTurn {
  return {
    id,
    role: "user",
    blocks: [{ type: "text", text }],
    timestamp: "2026-05-28T00:00:00.000Z",
  }
}

function assistantTurn(id: string, text: string): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: [{ type: "text", text }],
    timestamp: "2026-05-28T00:00:01.000Z",
  }
}

function liveMessage(text: string, id = "lm-1"): LiveMessage {
  return {
    id,
    role: "assistant",
    content: [{ type: "text", text }],
    startedAt: 1_700_000_000_000,
  }
}

function contentDelta(seq: number, text: string): EventEnvelope {
  return {
    connection_id: "c1",
    seq,
    type: "content_delta",
    text,
  }
}

function frame(applyEvents: EventEnvelope[]): AcceptedConnectionFrame {
  const seqs = applyEvents.map((e) => e.seq)
  return {
    contextKey: "tab-1",
    connectionId: "c1",
    deliveryIds: [1],
    applyEvents,
    rawEvents: applyEvents,
    highestSeq: seqs.length > 0 ? Math.max(...seqs) : 0,
  }
}

function seedHistory() {
  useConversationRuntimeStore.setState({
    byConversationId: new Map([
      [
        CID,
        {
          conversationId: CID,
          detail: {
            summary: {
              id: CID,
              folder_id: 1,
              agent_type: "codex",
              title: "t",
              title_locked: false,
              status: "in_progress",
              kind: "regular",
              model: null,
              git_branch: null,
              external_id: "sid-1",
              message_count: 2,
              child_count: 0,
              created_at: "2026-05-28T00:00:00.000Z",
              updated_at: "2026-05-28T00:00:00.000Z",
              pinned_at: null,
            },
            turns: [
              userTurn("u1", "hello"),
              assistantTurn("a1", "prior reply"),
            ],
            session_stats: null,
          },
          detailLoading: false,
          detailError: null,
          acpLoadError: null,
          localTurns: [],
          optimisticTurns: [],
          backgroundTurns: [],
          liveMessage: null,
          liveOwnsActiveTurn: false,
          delegationKickoffText: null,
          sessionStats: null,
          syncState: "idle",
          externalId: "sid-1",
          dbConversationId: CID,
          activeTurnToken: null,
          pendingCleanup: false,
        },
      ],
    ]),
    conversationIdByExternalId: new Map([["sid-1", CID]]),
  })
}

function publishLiveText(text: string, seq: number) {
  const msg = liveMessage(text)
  liveTranscriptStore.publish(CID, frame([contentDelta(seq, text)]), msg)
  // Canonical live also lands for completion handoff.
  useConversationRuntimeStore.getState().actions.setLiveMessage(CID, msg, true)
}

function enableIncremental() {
  __resetStreamingPerformanceConfigForTests()
  initializeStreamingPerformanceConfig({
    mode: "batched",
    perf_replay_available: true,
    failure_event: "acp://delivery-failed",
    flags: {
      desktop_acp_event_batching: true,
      incremental_live_transcript: true,
      deferred_streaming_rich_content: false,
    },
  })
}

function renderMessageList() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageListView
        conversationId={CID}
        agentType="codex"
        connStatus="prompting"
        isActive
        showMessageNav={false}
      />
    </NextIntlClientProvider>
  )
}

function assistantTexts(): string[] {
  return screen
    .queryAllByTestId("assistant-text")
    .map((el) => el.textContent ?? "")
    .concat(
      screen
        .queryAllByTestId("message-response")
        .map((el) => el.textContent ?? "")
    )
}

describe("singletonSourceTurns", () => {
  it("returns the same array reference for the same turn", () => {
    const t = assistantTurn("t1", "x")
    const first = singletonSourceTurns(t)
    const second = singletonSourceTurns(t)
    expect(first).toBe(second)
    expect(first).toEqual([t])
  })

  it("returns distinct arrays for distinct turns", () => {
    const a = singletonSourceTurns(assistantTurn("a", "a"))
    const b = singletonSourceTurns(assistantTurn("b", "b"))
    expect(a).not.toBe(b)
  })
})

describe("canReloadSessionLoadError", () => {
  it("requires a fresh conversation for legacy Codex CLI sessions", () => {
    expect(canReloadSessionLoadError("legacy_cli_session")).toBe(false)
    expect(canReloadSessionLoadError("resource_not_found")).toBe(true)
    expect(canReloadSessionLoadError(null)).toBe(true)
  })
})

describe("MessageListView live footer isolation", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    historicalRenderSpy.mockClear()
    liveRenderSpy.mockClear()
    enableIncremental()
    seedHistory()
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("renders no additional historical row during 500 live publications", () => {
    liveTranscriptStore.rebuild(CID, "c1", liveMessage("chunk-0"), 1)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, liveMessage("chunk-0"), true)

    renderMessageList()
    const historyAfterMount = historicalRenderSpy.mock.calls.filter(
      (c) => c[0] === "historicalRow"
    ).length
    const threadAfterMount = historicalRenderSpy.mock.calls.filter(
      (c) => c[0] === "historicalThread"
    ).length
    expect(historyAfterMount).toBeGreaterThan(0)

    act(() => {
      for (let index = 1; index < 500; index += 1) {
        publishLiveText(`chunk-${index}`, index + 1)
      }
    })

    const historyAfterLive = historicalRenderSpy.mock.calls.filter(
      (c) => c[0] === "historicalRow"
    ).length
    const threadAfterLive = historicalRenderSpy.mock.calls.filter(
      (c) => c[0] === "historicalThread"
    ).length

    // P2 gate: historical thread + rows stay cold during active live output.
    expect(historyAfterLive).toBe(historyAfterMount)
    expect(threadAfterLive).toBe(threadAfterMount)
    expect(liveRenderSpy.mock.calls.length).toBeGreaterThan(1)
    expect(document.querySelector("[data-message-live-footer]")).not.toBeNull()
    expect(document.querySelectorAll("[data-virtua-item]").length).toBe(2)
  })

  it("hands off without an empty or duplicate assistant row", () => {
    const finalText = "final answer"
    liveTranscriptStore.rebuild(CID, "c1", liveMessage(finalText), 1)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, liveMessage(finalText), true)

    renderMessageList()
    expect(assistantTexts()).toContain(finalText)

    act(() => {
      completeLiveTranscriptTurn(CID)
    })

    // Canonical promotion lands the same text once in history; live footer gone.
    expect(liveTranscriptStore.getConversation(CID)).toBeNull()
    const texts = assistantTexts()
    const finals = texts.filter((t) => t === finalText)
    expect(finals.length).toBe(1)
    expect(document.querySelector("[data-message-live-footer]")).toBeNull()
  })

  it("keeps historical selector stable while live content updates", () => {
    // Identity start invalidates history once; subsequent same-id content
    // updates must keep the historical array reference.
    act(() => {
      useConversationRuntimeStore
        .getState()
        .actions.setLiveMessage(CID, liveMessage("stream-a"), true)
      liveTranscriptStore.rebuild(CID, "c1", liveMessage("stream-a"), 1)
    })
    const before = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    act(() => {
      useConversationRuntimeStore
        .getState()
        .actions.setLiveMessage(CID, liveMessage("stream-ab"), true)
      liveTranscriptStore.publish(
        CID,
        frame([contentDelta(2, "b")]),
        liveMessage("stream-ab")
      )
    })
    const after = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(after).toBe(before)
  })

  it("footer remains under role=log for selection/copy ancestry", () => {
    liveTranscriptStore.rebuild(CID, "c1", liveMessage("copy me"), 1)
    renderMessageList()
    const footer = document.querySelector("[data-message-live-footer]")
    expect(footer).not.toBeNull()
    expect(footer?.closest('[role="log"]')).not.toBeNull()
  })

  it("uses instant resize while a live transcript is present", () => {
    liveTranscriptStore.rebuild(CID, "c1", liveMessage("stream"), 1)
    renderMessageList()
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "instant"
    )
  })

  it("uses smooth resize when no live transcript is present", () => {
    renderMessageList()
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "smooth"
    )
  })
})
