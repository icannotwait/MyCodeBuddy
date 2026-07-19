import { act, fireEvent, render, screen, cleanup } from "@testing-library/react"
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
  Message: ({
    children,
    from,
    ...rest
  }: {
    children?: ReactNode
    from?: string
    [key: string]: unknown
  }) => (
    <div data-testid="ai-message" data-from={from} {...rest}>
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
    autolinkLocalPathParts,
  }: {
    parts: Array<{ type: string; text?: string }>
    autolinkLocalPathParts?: ReadonlySet<{
      type: string
      text?: string
    }>
  }) => (
    <div data-testid="content-parts">
      {parts.map((part, index) =>
        part.type === "text" ? (
          <span
            key={index}
            data-testid="assistant-text"
            data-autolink-local-paths={String(
              autolinkLocalPathParts?.has(part) ?? false
            )}
          >
            {part.text}
          </span>
        ) : (
          <span key={index} data-part={part.type} />
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

const { subAgentOverlayPropsSpy } = vi.hoisted(() => ({
  subAgentOverlayPropsSpy: vi.fn(),
}))

vi.mock("@/components/chat/sub-agent-overlay", () => ({
  SubAgentOverlay: (props: {
    activities?: Array<{ task_id?: string; origin?: string }>
    delegations?: Array<{ parentToolUseId: string }>
    defaultExpanded?: boolean
    overlayKey?: string | null
  }) => {
    subAgentOverlayPropsSpy(props)
    return <div data-testid="sub-agent-overlay-capture" />
  },
}))

vi.mock("@/contexts/session-stats-context", () => ({
  useSessionStats: () => ({ setSessionStats: vi.fn() }),
}))

vi.mock("./conversation-message-nav", () => ({
  ConversationMessageNav: () => null,
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAgentThinkingVisibility: () => false,
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

const initialScrollControllerSpy = vi.fn()
vi.mock("./initial-history-scroll-controller", () => ({
  InitialHistoryScrollController: (props: {
    pending: boolean
    historyReady: boolean
    hasHistoryRows: boolean
    onFinish: () => void
  }) => {
    initialScrollControllerSpy(props)
    return props.pending ? (
      <button
        type="button"
        data-testid="finish-initial-history-scroll"
        onClick={props.onFinish}
      />
    ) : null
  },
}))

import { extractTextFromParts, MessageListView } from "./message-list-view"
import type { AdaptedToolCallPart } from "@/lib/adapters/ai-elements-adapter"
import type { DelegationActivityView } from "@/lib/types"

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

function toolTurn(id: string, text: string): MessageTurn {
  return {
    id,
    role: "tool",
    blocks: [{ type: "text", text }],
    timestamp: "2026-05-28T00:00:02.000Z",
  }
}

/** Historical assistant turn that materializes a Codex native spawn activity. */
function nativeSpawnAssistantTurn(
  id: string,
  toolCallId: string,
  taskId: string,
  timestamp = "2026-05-28T00:00:01.000Z"
): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: [
      {
        type: "tool_use",
        tool_use_id: toolCallId,
        tool_name: "spawn_agent",
        input_preview: JSON.stringify({
          agent_type: "worker",
          message: `work-${taskId}`,
        }),
      },
      {
        type: "tool_result",
        tool_use_id: toolCallId,
        output_preview: JSON.stringify({ agent_id: taskId }),
        is_error: false,
      },
    ],
    timestamp,
  }
}

/** Historical Codeg delegate_to_agent tool call on an assistant turn. */
function codegDelegateAssistantTurn(
  id: string,
  toolCallId: string,
  timestamp = "2026-05-28T00:00:01.000Z"
): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: [
      {
        type: "tool_use",
        tool_use_id: toolCallId,
        tool_name: "delegate_to_agent",
        input_preview: JSON.stringify({
          agent_type: "codex",
          task: `task-${toolCallId}`,
        }),
      },
      {
        type: "tool_result",
        tool_use_id: toolCallId,
        output_preview: JSON.stringify({
          task_id: `broker-${toolCallId}`,
          status: "running",
        }),
        is_error: false,
      },
    ],
    timestamp,
  }
}

function nativeActivityView(
  taskId: string,
  overrides: Partial<DelegationActivityView> = {}
): DelegationActivityView {
  return {
    origin: "native",
    authoritative: false,
    platform: "codex",
    task_id: taskId,
    operation: "spawn",
    observed_status: "completed",
    started_at: "2026-05-28T00:00:01.000Z",
    updated_at: "2026-05-28T00:00:02.000Z",
    ...overrides,
  }
}

function setStoreActivities(activities: DelegationActivityView[]) {
  useConversationRuntimeStore.setState((s) => {
    const session = s.byConversationId.get(CID)
    if (!session) return s
    const next = new Map(s.byConversationId)
    next.set(CID, { ...session, delegationActivities: activities })
    return { byConversationId: next }
  })
}

function lastOverlayProps(): {
  activities?: Array<{ task_id?: string; origin?: string }>
  delegations?: Array<{ parentToolUseId: string }>
  defaultExpanded?: boolean
  overlayKey?: string | null
} {
  const calls = subAgentOverlayPropsSpy.mock.calls
  expect(calls.length).toBeGreaterThan(0)
  return calls[calls.length - 1][0]
}

function activityTaskIds(props: ReturnType<typeof lastOverlayProps>): string[] {
  return (props.activities ?? [])
    .map((a) => a.task_id)
    .filter((id): id is string => typeof id === "string" && id.length > 0)
}

function liveMessage(text: string, id = "lm-1"): LiveMessage {
  return {
    id,
    role: "assistant",
    content: [{ type: "text", text }],
    startedAt: 1_700_000_000_000,
  }
}

function liveNativeSpawnMessage(
  taskId: string,
  toolCallId = "live-spawn-1"
): LiveMessage {
  const output = JSON.stringify({ agent_id: taskId })
  return {
    id: "lm-native-spawn",
    role: "assistant",
    content: [
      {
        type: "tool_call",
        info: {
          tool_call_id: toolCallId,
          title: "spawn_agent",
          kind: "other",
          status: "completed",
          content: null,
          raw_input: JSON.stringify({
            agent_type: "worker",
            message: `live-${taskId}`,
          }),
          raw_output_chunks: [output],
          raw_output_total_bytes: output.length,
          locations: null,
          meta: null,
          images: [],
        },
      },
    ],
    startedAt: Date.parse("2026-07-16T10:00:00Z"),
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

function seedHistory(
  turns: MessageTurn[] = [
    userTurn("u1", "hello"),
    assistantTurn("a1", "prior reply"),
  ]
) {
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
              auto_title_finalized: false,
              status: "in_progress",
              awaiting_reply_token: null,
              kind: "regular",
              model: null,
              git_branch: null,
              external_id: "sid-1",
              message_count: turns.length,
              child_count: 0,
              created_at: "2026-05-28T00:00:00.000Z",
              updated_at: "2026-05-28T00:00:00.000Z",
              pinned_at: null,
            },
            turns,
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
          delegationActivities: [],
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

describe("extractTextFromParts", () => {
  it("copies reasoning even when its view is hidden", () => {
    expect(
      extractTextFromParts([
        { type: "reasoning", content: "hidden thought", isStreaming: false },
        { type: "text", text: "final answer" },
      ])
    ).toBe("hidden thought\nfinal answer")
  })

  it("copies reasoning recursively through goal runs", () => {
    const start: AdaptedToolCallPart = {
      type: "tool-call",
      toolCallId: "goal-1",
      toolName: "update_goal",
      input: null,
      state: "input-available",
    }
    expect(
      extractTextFromParts([
        {
          type: "goal-run",
          start,
          end: null,
          items: [
            {
              type: "reasoning",
              content: "nested hidden thought",
              isStreaming: false,
            },
          ],
          isRunning: false,
        },
      ])
    ).toBe("nested hidden thought")
  })
})

describe("MessageListView initial history scroll latch", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    initialScrollControllerSpy.mockClear()
    listScrollToBottom.mockClear()
    seedHistory()
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  const ui = (isActive: boolean, detailLoading: boolean) => (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageListView
        conversationId={CID}
        agentType="codex"
        connStatus="connected"
        isActive={isActive}
        detailLoading={detailLoading}
        initialHistoryScrollEligible
        historyLoadComplete
        showMessageNav={false}
      />
    </NextIntlClientProvider>
  )

  it("uses instant resize once and does not reset for cache switches or reloads", () => {
    const view = render(ui(true, false))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "instant"
    )
    expect(
      screen.getByTestId("finish-initial-history-scroll")
    ).toBeInTheDocument()

    fireEvent.click(screen.getByTestId("finish-initial-history-scroll"))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "smooth"
    )
    expect(
      screen.queryByTestId("finish-initial-history-scroll")
    ).not.toBeInTheDocument()

    view.rerender(ui(false, false))
    view.rerender(ui(true, true))
    view.rerender(ui(true, false))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "smooth"
    )
    expect(
      screen.queryByTestId("finish-initial-history-scroll")
    ).not.toBeInTheDocument()

    view.unmount()
    render(ui(true, false))
    expect(screen.getByTestId("message-thread")).toHaveAttribute(
      "data-resize",
      "instant"
    )
    expect(
      screen.getByTestId("finish-initial-history-scroll")
    ).toBeInTheDocument()
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

  it("keeps the compatibility streaming row opted out until completion", () => {
    __resetStreamingPerformanceConfigForTests()
    act(() => {
      useConversationRuntimeStore
        .getState()
        .actions.setLiveMessage(CID, liveMessage("compat live reply"), true)
    })

    renderMessageList()

    expect(screen.getByText("compat live reply")).toHaveAttribute(
      "data-autolink-local-paths",
      "false"
    )

    act(() => {
      completeLiveTranscriptTurn(CID, liveMessage("compat live reply"))
    })
    expect(screen.getByText("compat live reply")).toHaveAttribute(
      "data-autolink-local-paths",
      "true"
    )
  })

  it("keeps source tool text ineligible after assistant-display merging", () => {
    const assistantText = String.raw`D:\assistant\src\app.ts`
    const toolText = String.raw`D:\tool-output\src\app.ts`
    seedHistory([
      userTurn("u1", "hello"),
      assistantTurn("a1", assistantText),
      toolTurn("t1", toolText),
    ])

    renderMessageList()

    expect(screen.getByText(assistantText)).toHaveAttribute(
      "data-autolink-local-paths",
      "true"
    )
    expect(screen.getByText(toolText)).toHaveAttribute(
      "data-autolink-local-paths",
      "false"
    )
  })

  it("keeps live activity visible for a hidden thinking-only footer", () => {
    const message: LiveMessage = {
      id: "thinking-only",
      role: "assistant",
      content: [{ type: "thinking", text: "hidden live thought" }],
      startedAt: 1,
    }
    liveTranscriptStore.rebuild(CID, "c1", message, 1)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, message, true)

    renderMessageList()

    expect(screen.queryByTestId("live-transcript-row")).not.toBeInTheDocument()
    expect(screen.getByTestId("live-turn-stats")).toBeInTheDocument()
  })
})

describe("MessageListView sub-agent overlay composition", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    subAgentOverlayPropsSpy.mockClear()
    enableIncremental()
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("merges earlier historical native activities with non-empty latest-turn store materialization", () => {
    // Store deliberately materializes only the latest assistant turn; the
    // earlier native spawn must still reach overlay props via full-session
    // derivation + dedupe (not a store non-empty short circuit).
    seedHistory([
      userTurn("u1", "first"),
      nativeSpawnAssistantTurn(
        "a1",
        "call-old",
        "task-older",
        "2026-05-28T00:00:01.000Z"
      ),
      userTurn("u2", "second"),
      nativeSpawnAssistantTurn(
        "a2",
        "call-new",
        "task-newer",
        "2026-05-28T00:00:03.000Z"
      ),
    ])
    setStoreActivities([
      nativeActivityView("task-newer", {
        started_at: "2026-05-28T00:00:03.000Z",
        updated_at: "2026-05-28T00:00:04.000Z",
      }),
    ])

    renderMessageList()

    const props = lastOverlayProps()
    const taskIds = activityTaskIds(props)
    expect(taskIds).toEqual(
      expect.arrayContaining(["task-older", "task-newer"])
    )
    expect(taskIds.filter((id) => id === "task-older")).toHaveLength(1)
    expect(taskIds.filter((id) => id === "task-newer")).toHaveLength(1)
  })

  it("projects live native activities while a pre-existing store activity is present", () => {
    seedHistory([
      userTurn("u1", "prior"),
      nativeSpawnAssistantTurn("a1", "call-store", "task-store"),
    ])
    // Pre-existing store materialization (e.g. prior COMPLETE_TURN). Live
    // transcript holds a *new* native spawn; do not call setLiveMessage so the
    // store is not rewritten to last-live-only before composition runs.
    setStoreActivities([nativeActivityView("task-store")])

    const live = liveNativeSpawnMessage("task-live", "live-spawn-1")
    act(() => {
      liveTranscriptStore.rebuild(CID, "c1", live, 1)
    })

    renderMessageList()

    const props = lastOverlayProps()
    const taskIds = activityTaskIds(props)
    expect(taskIds).toEqual(expect.arrayContaining(["task-store", "task-live"]))
    expect(taskIds.filter((id) => id === "task-store")).toHaveLength(1)
    expect(taskIds.filter((id) => id === "task-live")).toHaveLength(1)
  })

  it("passes full-session Codeg delegations with conversation-scoped key and defaultExpanded", () => {
    seedHistory([
      userTurn("u1", "first"),
      codegDelegateAssistantTurn("a1", "pt-older", "2026-05-28T00:00:01.000Z"),
      userTurn("u2", "second"),
      codegDelegateAssistantTurn("a2", "pt-newer", "2026-05-28T00:00:03.000Z"),
    ])

    renderMessageList()

    const props = lastOverlayProps()
    const parentIds = (props.delegations ?? []).map((d) => d.parentToolUseId)
    expect(parentIds).toEqual(["pt-older", "pt-newer"])
    expect(props.defaultExpanded).toBe(true)
    expect(props.overlayKey).toBe(`subagents-${CID}`)
  })
})
