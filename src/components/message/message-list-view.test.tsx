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
  mergeConsecutiveAssistantTurns,
  singletonSourceTurns,
  type MergedAssistantRunCache,
  type ResolvedMessageGroup,
  type ThreadRenderItem,
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

const { virtualizerScrollToIndex } = vi.hoisted(() => ({
  virtualizerScrollToIndex: vi.fn(),
}))

// virtua / stick-to-bottom / heavy markdown — keep list tests focused.
vi.mock("virtua", () => ({
  Virtualizer: forwardRef(function VirtualizerMock(
    props: { children?: ReactNode },
    ref: Ref<{ scrollToIndex: (i: number) => void }>
  ) {
    useImperativeHandle(ref, () => ({
      scrollToIndex: virtualizerScrollToIndex,
    }))
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
    parts: Array<{
      type: string
      text?: string
      key?: string
      sources?: unknown[]
      visibleTaskIds?: string[]
    }>
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
        ) : part.type === "delegation-work-unit" ? (
          <span
            key={index}
            data-testid="delegation-work-unit"
            data-work-unit-key={part.key}
            data-source-count={part.sources?.length ?? 0}
          />
        ) : part.type === "delegation-status-group" ? (
          <span
            key={index}
            data-testid="delegation-status-residual"
            data-visible-task-ids={part.visibleTaskIds?.join(",") ?? "all"}
          />
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

function workUnitRunTurn(
  id: string,
  toolCallId: string,
  taskId: string,
  options: {
    toolName?: "delegate_to_agent" | "continue_delegation"
    targetTaskId?: string
  } = {}
): MessageTurn {
  const targetTaskId = options.targetTaskId
  return {
    id,
    role: "assistant",
    blocks: [
      {
        type: "tool_use",
        tool_use_id: toolCallId,
        tool_name: options.toolName ?? "delegate_to_agent",
        input_preview: JSON.stringify({
          agent_type: "codex",
          task: "implement",
          work_unit_key: "unit-a",
          ...(targetTaskId ? { task_id: targetTaskId } : {}),
        }),
      },
      {
        type: "tool_result",
        tool_use_id: toolCallId,
        output_preview: JSON.stringify({
          content: [{ type: "text", text: `Delegated ${taskId}` }],
          structuredContent: {
            status: "running",
            task_id: taskId,
            child_conversation_id: 3001,
            ...(targetTaskId ? { continued_from_task_id: targetTaskId } : {}),
          },
        }),
        is_error: false,
      },
    ],
    timestamp: "2026-05-28T00:00:01.000Z",
  }
}

function workUnitStatusTurn(id: string, taskIds: string[]): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: [
      {
        type: "tool_use",
        tool_use_id: `poll-${id}`,
        tool_name: "get_delegation_status",
        input_preview: JSON.stringify({ task_ids: taskIds }),
      },
      {
        type: "tool_result",
        tool_use_id: `poll-${id}`,
        output_preview: JSON.stringify({
          content: [{ type: "text", text: "status batch" }],
          structuredContent: {
            tasks: taskIds.map((taskId) => ({
              task_id: taskId,
              status: "running",
              message: "Running.",
            })),
          },
        }),
        is_error: false,
      },
    ],
    timestamp: "2026-05-28T00:00:02.000Z",
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
          pendingBackgroundSettlements: [],
          liveMessage: null,
          liveOwnsActiveTurn: false,
          lastTurnOwned: false,
          delegationKickoffText: null,
          sessionStats: null,
          syncState: "idle",
          externalId: "sid-1",
          dbConversationId: CID,
          activeTurnToken: null,
          pendingCleanup: false,
          delegationActivities: [],
          historyAssistantBaseline: null,
          delegateSyncError: null,
          pendingCancel: null,
          softFence: false,
          ownerPreserve: false,
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

type ThreadItem = Parameters<typeof mergeConsecutiveAssistantTurns>[0][number]
type TurnItem = Extract<ThreadItem, { kind: "turn" }>

function turn(id: string): MessageTurn {
  return { id, role: "assistant", blocks: [], timestamp: "" }
}

function assistantItem(
  id: string,
  groupOverrides: Partial<TurnItem["group"]> = {}
): ThreadItem {
  return {
    key: `persisted-${id}`,
    kind: "turn",
    group: {
      id,
      role: "assistant",
      parts: [{ type: "text", text: `reply ${id}` }],
      resources: [],
      images: [],
      autolinkableTextParts: new Set(),
      ...groupOverrides,
    },
    phase: "persisted",
    showStats: false,
    isRoleTransition: false,
    previousUserIndex: null,
    sourceTurns: [],
  }
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

describe("MessageListView turn-anchor focus", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    virtualizerScrollToIndex.mockClear()
    seedHistory()
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("focuses the exact persisted turn when the dialog supplies an anchor", () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <MessageListView
          conversationId={CID}
          agentType="codex"
          connStatus="connected"
          isActive={false}
          showMessageNav={false}
          initialHistoryScrollEligible
          focusTurnAnchor="a1"
        />
      </NextIntlClientProvider>
    )

    expect(virtualizerScrollToIndex).toHaveBeenCalledWith(1, {
      align: "start",
      smooth: true,
    })
    const lastInitialScrollProps =
      initialScrollControllerSpy.mock.calls.at(-1)?.[0]
    expect(lastInitialScrollProps).toMatchObject({ pending: false })
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

describe("MessageListView delegation work-unit projection", () => {
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

  it("renders one historical card and one residual status row across continuations", () => {
    seedHistory([
      userTurn("u1", "start"),
      workUnitRunTurn("a1", "tool-1", "run-1"),
      workUnitStatusTurn("a2", ["run-1"]),
      assistantTurn("a3", "checkpoint explanation"),
      workUnitRunTurn("a4", "tool-2", "run-2", {
        toolName: "continue_delegation",
        targetTaskId: "run-1",
      }),
      assistantTurn("a5", "still working"),
      workUnitStatusTurn("a6", ["run-2", "unknown-run"]),
    ])

    renderMessageList()

    expect(screen.getAllByTestId("delegation-work-unit")).toHaveLength(1)
    expect(screen.getByTestId("delegation-work-unit")).toHaveAttribute(
      "data-work-unit-key",
      "wu:unit-a"
    )
    expect(screen.getByTestId("delegation-work-unit")).toHaveAttribute(
      "data-source-count",
      "2"
    )
    expect(screen.getByText("checkpoint explanation")).toBeInTheDocument()
    expect(screen.getByText("still working")).toBeInTheDocument()
    expect(screen.getByTestId("delegation-status-residual")).toHaveAttribute(
      "data-visible-task-ids",
      "unknown-run"
    )
    expect(
      (lastOverlayProps().delegations ?? []).map(
        (delegation) => delegation.parentToolUseId
      )
    ).toEqual(["tool-1", "tool-2"])
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

describe("mergeConsecutiveAssistantTurns completion metadata", () => {
  it("surfaces completion time patched onto a non-last sub-turn", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", {
        duration_ms: 15_975,
        completed_at: "2026-07-19T05:25:22.851Z",
      }),
      assistantItem("b"),
    ])
    expect(merged).toHaveLength(1)
    const item = merged[0] as TurnItem
    expect(item.group.completed_at).toBe("2026-07-19T05:25:22.851Z")
    expect(item.group.duration_ms).toBe(15_975)
  })

  it("keeps the latest completion across merged sub-turns", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", { completed_at: "2026-07-19T05:25:10.000Z" }),
      assistantItem("b", { completed_at: "2026-07-19T05:25:22.851Z" }),
    ])
    const item = merged[0] as TurnItem
    expect(item.group.completed_at).toBe("2026-07-19T05:25:22.851Z")
  })

  it("does not fold a compaction divider into the preceding assistant reply", () => {
    // The compaction event sits BETWEEN two assistant replies (the reply before
    // `/compact` and the next). Two bare assistant turns would merge into one;
    // the dedicated "compaction" item must break that run so the divider renders
    // standalone in the correct between-turns position (and the first reply keeps
    // its own footer).
    const compaction: ThreadItem = {
      key: "persisted-compact",
      kind: "compaction",
      meta: { contextCompaction: true, tokensBefore: 51777, tokensAfter: 4616 },
    }
    // Sanity: without the divider, the two assistant turns DO merge to one.
    expect(
      mergeConsecutiveAssistantTurns([assistantItem("a"), assistantItem("b")])
    ).toHaveLength(1)
    // With the divider between them, the run is broken → 3 standalone items.
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a"),
      compaction,
      assistantItem("b"),
    ])
    expect(merged.map((it) => it.kind)).toEqual(["turn", "compaction", "turn"])
  })
})

function makeGroup(
  role: "user" | "assistant",
  id: string
): ResolvedMessageGroup {
  return {
    id,
    role,
    parts: [],
    resources: [],
    images: [],
    autolinkableTextParts: new Set(),
  }
}

function makeItem(
  group: ResolvedMessageGroup,
  index: number,
  phase: "persisted" | "optimistic" | "streaming" = "persisted"
): ThreadRenderItem {
  return {
    key: `${phase}-${group.id}-${index}`,
    kind: "turn",
    group,
    phase,
    showStats: false,
    isRoleTransition: false,
    previousUserIndex: null,
    sourceTurns: singletonSourceTurns(turn(group.id)),
  }
}

function makeUserItem(id: string, index: number): ThreadRenderItem {
  const item = makeItem(makeGroup("user", id), index)
  if (item.kind === "turn") {
    item.group.parts = [{ type: "text", text: "hi" }]
  }
  return item
}

describe("mergeConsecutiveAssistantTurns merged-run cache", () => {
  it("reuses the merged item when membership is unchanged", () => {
    const cache: MergedAssistantRunCache = new WeakMap()
    const g1 = makeGroup("assistant", "a1")
    const g2 = makeGroup("assistant", "a2")

    const out1 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), makeItem(g2, 1)],
      cache
    )
    const out2 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), makeItem(g2, 1)],
      cache
    )

    expect(out1).toHaveLength(1)
    expect(out2[0]).toBe(out1[0])
    expect(out2[0].key).toBe("merged-persisted-a1-0")
  })

  it("rebuilds a changed run without touching a neighboring run", () => {
    const cache: MergedAssistantRunCache = new WeakMap()
    const g1 = makeGroup("assistant", "a1")
    const g2 = makeGroup("assistant", "a2")
    const g3 = makeGroup("assistant", "a3")
    const g4 = makeGroup("assistant", "a4")
    const out1 = mergeConsecutiveAssistantTurns(
      [
        makeItem(g1, 0),
        makeItem(g2, 1),
        makeUserItem("u1", 2),
        makeItem(g3, 3),
        makeItem(g4, 4),
      ],
      cache
    )
    const out2 = mergeConsecutiveAssistantTurns(
      [
        makeItem(g1, 0),
        makeItem(makeGroup("assistant", "a2"), 1),
        makeUserItem("u1", 2),
        makeItem(g3, 3),
        makeItem(g4, 4),
      ],
      cache
    )

    expect(out2[0]).not.toBe(out1[0])
    expect(out2[2]).toBe(out1[2])
  })

  it("misses when a run gains a member, then caches the new membership", () => {
    const cache: MergedAssistantRunCache = new WeakMap()
    const g1 = makeGroup("assistant", "a1")
    const g2 = makeGroup("assistant", "a2")
    const g3 = makeGroup("assistant", "a3")
    const out1 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), makeItem(g2, 1)],
      cache
    )
    const out2 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), makeItem(g2, 1), makeItem(g3, 2)],
      cache
    )
    const out3 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), makeItem(g2, 1), makeItem(g3, 2)],
      cache
    )

    expect(out2[0]).not.toBe(out1[0])
    expect(out3[0]).toBe(out2[0])
  })

  it("keeps cache hits across interleaved empty turns", () => {
    const cache: MergedAssistantRunCache = new WeakMap()
    const g1 = makeGroup("assistant", "a1")
    const g2 = makeGroup("assistant", "a2")
    const emptyUser = () => makeItem(makeGroup("user", "empty"), 1)
    const out1 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), emptyUser(), makeItem(g2, 2)],
      cache
    )
    const out2 = mergeConsecutiveAssistantTurns(
      [makeItem(g1, 0), emptyUser(), makeItem(g2, 2)],
      cache
    )

    expect(out1).toHaveLength(1)
    expect(out2[0]).toBe(out1[0])
  })

  it("passes a single turn through and still merges without a cache", () => {
    const item = makeItem(makeGroup("assistant", "solo"), 0)
    expect(mergeConsecutiveAssistantTurns([item])[0]).toBe(item)

    const g1 = makeGroup("assistant", "a1")
    const g2 = makeGroup("assistant", "a2")
    const out1 = mergeConsecutiveAssistantTurns([
      makeItem(g1, 0),
      makeItem(g2, 1),
    ])
    const out2 = mergeConsecutiveAssistantTurns([
      makeItem(g1, 0),
      makeItem(g2, 1),
    ])
    expect(out2[0]).not.toBe(out1[0])
    expect(out2[0]).toEqual(out1[0])
  })
})

describe("mergeConsecutiveAssistantTurns outcome presentation", () => {
  const interruptedOutcome = {
    status: "interrupted" as const,
    stop_reason: "cancelled" as const,
    source: "user_stop" as const,
    provider_turn_id: "turn-abc",
    completed_at: "2026-07-25T12:00:00.000Z",
    duration_ms: 1500,
  }

  it("propagates the last non-null outcome onto the merged response group", () => {
    const withContent = makeGroup("assistant", "a1")
    withContent.parts = [{ type: "text", text: "partial" }]
    const outcomeOnly = makeGroup("assistant", "a2")
    outcomeOnly.outcome = interruptedOutcome

    const merged = mergeConsecutiveAssistantTurns([
      makeItem(withContent, 0),
      makeItem(outcomeOnly, 1),
    ])

    expect(merged).toHaveLength(1)
    const item = merged[0] as TurnItem
    expect(item.group.outcome).toEqual(interruptedOutcome)
    expect(item.group.parts).toEqual([{ type: "text", text: "partial" }])
  })

  it("keeps an outcome-only assistant as a grouping participant (not transparent)", () => {
    const content = makeGroup("assistant", "a1")
    content.parts = [{ type: "text", text: "hello" }]
    const outcomeOnly = makeGroup("assistant", "a-outcome")
    outcomeOnly.outcome = interruptedOutcome
    // Empty user between assistants is transparent today; outcome-only must
    // still survive as a member so its outcome can land on the merged group.
    const emptyUser = makeGroup("user", "empty-u")
    const trailing = makeGroup("assistant", "a3")
    trailing.parts = [{ type: "text", text: "more" }]

    const merged = mergeConsecutiveAssistantTurns([
      makeItem(content, 0),
      makeItem(emptyUser, 1),
      makeItem(outcomeOnly, 2),
      makeItem(trailing, 3),
    ])

    expect(merged).toHaveLength(1)
    const item = merged[0] as TurnItem
    expect(item.group.outcome).toEqual(interruptedOutcome)
    expect(
      item.group.parts.map((p) => (p.type === "text" ? p.text : ""))
    ).toEqual(["hello", "more"])
  })
})

describe("MessageListView response-interrupted footer", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("renders a compact footer once for an interrupted outcome and excludes it from copy", () => {
    seedHistory([
      userTurn("u1", "hello"),
      {
        ...assistantTurn("a1", "partial reply"),
        outcome: {
          status: "interrupted",
          stop_reason: "cancelled",
          source: "user_stop",
          provider_turn_id: "turn-abc",
        },
      },
    ])

    renderMessageList()

    const footers = screen.getAllByTestId("response-interrupted-footer")
    expect(footers).toHaveLength(1)
    expect(footers[0]).toHaveTextContent("Response interrupted")
    // Body text remains; footer copy is not part of extractable assistant text.
    expect(screen.getByText("partial reply")).toBeInTheDocument()
    expect(
      extractTextFromParts([{ type: "text", text: "partial reply" }])
    ).toBe("partial reply")
    expect(
      extractTextFromParts([{ type: "text", text: "partial reply" }])
    ).not.toContain("Response interrupted")
  })

  it("does not create an empty message bubble for an outcome-only assistant turn", () => {
    seedHistory([
      userTurn("u1", "hello"),
      {
        id: "a-outcome-only",
        role: "assistant",
        blocks: [],
        timestamp: "2026-05-28T00:00:01.000Z",
        outcome: {
          status: "interrupted",
          stop_reason: "cancelled",
          source: "user_stop",
          provider_turn_id: "turn-empty",
        },
      },
    ])

    renderMessageList()

    expect(
      screen.getByTestId("response-interrupted-footer")
    ).toBeInTheDocument()
    // Outcome-only: no empty assistant bubble (user row may still use shared text ids).
    expect(document.querySelectorAll('[data-from="assistant"]')).toHaveLength(0)
    expect(screen.queryByTestId("message-response")).toBeNull()
  })

  it("invalidates list rendering when only outcome is attached later (FE case 19)", () => {
    seedHistory([userTurn("u1", "hello"), assistantTurn("a1", "partial reply")])

    const view = renderMessageList()
    expect(screen.queryByTestId("response-interrupted-footer")).toBeNull()

    act(() => {
      seedHistory([
        userTurn("u1", "hello"),
        {
          ...assistantTurn("a1", "partial reply"),
          outcome: {
            status: "interrupted",
            stop_reason: "cancelled",
            source: "user_stop",
            provider_turn_id: "turn-late",
            duration_ms: 900,
          },
        },
      ])
    })
    view.rerender(
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

    expect(
      screen.getByTestId("response-interrupted-footer")
    ).toBeInTheDocument()
    expect(screen.getByText("partial reply")).toBeInTheDocument()
  })
})
