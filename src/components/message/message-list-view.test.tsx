import {
  act,
  fireEvent,
  render,
  screen,
  cleanup,
  waitFor,
} from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"
import { forwardRef, StrictMode, useImperativeHandle, type Ref } from "react"
import type { LiveMessage } from "@/contexts/acp-connections-context"
import type {
  AcceptedConnectionFrame,
  DbConversationSummary,
  EventEnvelope,
  MessageTurn,
} from "@/lib/types"
import enMessages from "@/i18n/messages/en.json"
import zhCNMessages from "@/i18n/messages/zh-CN.json"
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

const {
  virtualizerScrollToIndex,
  virtualizerKeysSpy,
  listChildConversationsMock,
  recordFrontendTurnTrace,
} = vi.hoisted(() => ({
  virtualizerScrollToIndex: vi.fn(),
  virtualizerKeysSpy: vi.fn(),
  listChildConversationsMock: vi.fn(async () => [] as const),
  recordFrontendTurnTrace: vi.fn(),
}))

vi.mock("@/lib/acp/frontend-turn-trace", () => ({
  recordFrontendTurnTrace: (...args: unknown[]) =>
    recordFrontendTurnTrace(...args),
}))

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>()
  return {
    ...actual,
    listChildConversations: (
      ...args: Parameters<typeof actual.listChildConversations>
    ) => listChildConversationsMock(...args),
  }
})

vi.mock("@/lib/platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/platform")>()
  return {
    ...actual,
    subscribe: vi.fn(async () => () => {}),
    onTransportReconnect: vi.fn(() => () => {}),
  }
})

// virtua / stick-to-bottom / heavy markdown — keep list tests focused.
vi.mock("virtua", () => ({
  Virtualizer: forwardRef(function VirtualizerMock(
    props: { children?: ReactNode },
    ref: Ref<{ scrollToIndex: (i: number) => void }>
  ) {
    const children = Array.isArray(props.children) ? props.children : []
    virtualizerKeysSpy(
      children.map(
        (child) => (child as { key?: string | null } | null)?.key ?? null
      )
    )
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

vi.mock("@/components/ai-elements/grok-session-image-context", () => ({
  GrokConversationProvider: ({
    children,
    conversationId,
  }: {
    children: ReactNode
    conversationId: number | null
  }) => (
    <div
      data-testid="grok-conversation-provider"
      data-grok-conversation-id={conversationId ?? "none"}
    >
      {children}
    </div>
  ),
  GrokSessionImageScope: ({
    children,
    phase,
  }: {
    children: ReactNode
    phase: "live" | "complete" | null
  }) => <div data-grok-phase={phase ?? undefined}>{children}</div>,
  useGrokConversationId: () => null,
  useGrokSessionImageScope: () => null,
}))

vi.mock("./content-parts-renderer", () => ({
  ContentPartsRenderer: ({
    parts,
    parentConversationId,
    autolinkLocalPathParts,
    grokSessionImagePhase,
    grokSessionImageTextParts,
  }: {
    parts: Array<{
      type: string
      text?: string
      key?: string
      toolCallId?: string
      sources?: Array<{
        meta?: Record<string, unknown> | null
      }>
      visibleTaskIds?: string[]
    }>
    parentConversationId?: number | null
    autolinkLocalPathParts?: ReadonlySet<{
      type: string
      text?: string
    }>
    grokSessionImagePhase?: "live" | "complete" | null
    grokSessionImageTextParts?: ReadonlySet<{
      type: string
      text?: string
    }>
  }) => (
    <div
      data-testid="content-parts"
      data-grok-phase={grokSessionImagePhase ?? undefined}
      data-parent-conversation-id={parentConversationId ?? undefined}
    >
      {parts.map((part, index) =>
        part.type === "text" ? (
          <span
            key={index}
            data-testid="assistant-text"
            data-autolink-local-paths={String(
              autolinkLocalPathParts?.has(part) ?? false
            )}
            data-grok-session-image-eligible={String(
              grokSessionImageTextParts?.has(part) ?? false
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
            data-parent-conversation-id={parentConversationId ?? undefined}
            data-latest-status={String(
              (
                part.sources?.[part.sources.length - 1]?.meta?.[
                  "codeg.delegation"
                ] as Record<string, unknown> | undefined
              )?.status ?? "unknown"
            )}
          />
        ) : part.type === "delegation-status-group" ? (
          <span
            key={index}
            data-testid="delegation-status-residual"
            data-visible-task-ids={part.visibleTaskIds?.join(",") ?? "all"}
          />
        ) : (
          <span
            key={index}
            data-testid={
              part.type === "tool-call" && part.toolCallId
                ? `tool-part-${part.toolCallId}`
                : undefined
            }
            data-part={part.type}
          />
        )
      )}
    </div>
  ),
}))

vi.mock("./live-turn-stats", () => ({
  LiveTurnStats: (props: {
    statusMode?: string
    message?: { content?: unknown[] }
  }) => (
    <div
      data-testid="live-turn-stats"
      data-status-mode={props.statusMode ?? "auto"}
      data-tool-count={
        props.message?.content?.filter(
          (b: { type?: string }) => b.type === "tool_call"
        ).length ?? 0
      }
    />
  ),
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
    delegations?: Array<{
      parentToolUseId: string
      parentConversationId?: number | null
    }>
    conversationId?: number | null
    defaultExpanded?: boolean
    overlayKey?: string | null
    isActive?: boolean
    workspaceRootPath?: string | null
  }) => {
    subAgentOverlayPropsSpy(props)
    return <div data-testid="sub-agent-overlay-capture" />
  },
}))

vi.mock("./conversation-message-nav", () => ({
  ConversationMessageNav: () => null,
}))

// The create-task action pulls workbench-route + tab-store contexts that this
// unit test doesn't mount; stub it to a no-op handler.
vi.mock("./use-create-task-from-message", () => ({
  useCreateTaskFromMessage: () => () => {},
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

beforeEach(() => {
  listChildConversationsMock.mockReset()
  listChildConversationsMock.mockResolvedValue([])
  recordFrontendTurnTrace.mockReset()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

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
    workUnitKey?: string
    childConversationId?: number
    generation?: number
    terminal?: boolean
  } = {}
): MessageTurn {
  const toolName = options.toolName ?? "delegate_to_agent"
  const targetTaskId = options.targetTaskId
  const workUnitKey = options.workUnitKey ?? "unit-a"
  const childConversationId = options.childConversationId ?? 3001
  const generation = options.generation ?? 1
  const terminal = options.terminal === true
  const startedAt = "2026-07-27T00:00:00.000Z"
  const finishedAt = terminal ? "2026-07-27T00:30:00.000Z" : null
  return {
    id,
    role: "assistant",
    blocks: [
      {
        type: "tool_use",
        tool_use_id: toolCallId,
        tool_name: toolName,
        input_preview: JSON.stringify({
          ...(toolName === "delegate_to_agent" ? { agent_type: "codex" } : {}),
          task: "implement",
          work_unit_key: workUnitKey,
          ...(targetTaskId ? { task_id: targetTaskId } : {}),
        }),
        meta: {
          "codeg.delegation": {
            status: terminal ? "completed" : "running",
            task_id: taskId,
            child_conversation_id: childConversationId,
            generation,
            started_at: startedAt,
            finished_at: finishedAt,
            runtime_stats: {
              started_at: startedAt,
              finished_at: finishedAt,
              tool_call_count: generation,
              edit_tool_call_count: 0,
              touched_files: [],
              touched_files_truncated: false,
              line_counts_complete: false,
            },
          },
        },
      },
      {
        type: "tool_result",
        tool_use_id: toolCallId,
        output_preview: JSON.stringify({
          content: [{ type: "text", text: `Delegated ${taskId}` }],
          structuredContent: {
            status: terminal ? "completed" : "running",
            task_id: taskId,
            child_conversation_id: childConversationId,
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
  delegations?: Array<{
    parentToolUseId: string
    parentConversationId?: number | null
  }>
  conversationId?: number | null
  defaultExpanded?: boolean
  overlayKey?: string | null
  isActive?: boolean
  workspaceRootPath?: string | null
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
  ],
  options?: {
    runtimeId?: number
    dbConversationId?: number | null
    persistedAgentType?: DbConversationSummary["agent_type"]
  }
) {
  const runtimeId = options?.runtimeId ?? CID
  const dbConversationId =
    options?.dbConversationId === undefined
      ? runtimeId
      : options.dbConversationId
  useConversationRuntimeStore.setState({
    byConversationId: new Map([
      [
        runtimeId,
        {
          conversationId: runtimeId,
          detail: {
            summary: {
              id: dbConversationId ?? runtimeId,
              folder_id: 1,
              agent_type: options?.persistedAgentType ?? "codex",
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
          detailHistoryLoadingOlder: false,
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
          dbConversationId,
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
    conversationIdByExternalId: new Map([["sid-1", runtimeId]]),
  })
}

function setLocalTurns(turns: MessageTurn[]): void {
  const state = useConversationRuntimeStore.getState()
  const byConversationId = new Map(state.byConversationId)
  const session = byConversationId.get(CID)
  if (!session) throw new Error("conversation fixture is not seeded")
  byConversationId.set(CID, { ...session, localTurns: turns })
  useConversationRuntimeStore.setState({ byConversationId })
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

function messageListUi(options?: {
  waitingForSubagentsArmedAtMs?: number | null
  connStatus?: "connected" | "prompting" | "connecting" | "disconnected"
  isActive?: boolean
  workspaceRootPath?: string | null
  conversationId?: number
  agentType?: "codex" | "grok"
}) {
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageListView
        conversationId={options?.conversationId ?? CID}
        agentType={options?.agentType ?? "codex"}
        connStatus={options?.connStatus ?? "prompting"}
        isActive={options?.isActive ?? true}
        workspaceRootPath={options?.workspaceRootPath ?? null}
        showMessageNav={false}
        waitingForSubagentsArmedAtMs={
          options?.waitingForSubagentsArmedAtMs ?? null
        }
      />
    </NextIntlClientProvider>
  )
}

function renderMessageList(options?: {
  waitingForSubagentsArmedAtMs?: number | null
  connStatus?: "connected" | "prompting" | "connecting" | "disconnected"
  isActive?: boolean
  workspaceRootPath?: string | null
  conversationId?: number
  agentType?: "codex" | "grok"
}) {
  return render(messageListUi(options))
}

describe("MessageListView Grok durable identity and phases", () => {
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

  it("provides the positive durable binding for a virtual Grok conversation", () => {
    seedHistory(undefined, {
      runtimeId: -7,
      dbConversationId: 42,
      persistedAgentType: "grok",
    })

    renderMessageList({ conversationId: -7, agentType: "grok" })

    expect(screen.getByTestId("grok-conversation-provider")).toHaveAttribute(
      "data-grok-conversation-id",
      "42"
    )
  })

  it("rejects caller Grok identity when the persisted summary is non-Grok", () => {
    seedHistory(undefined, {
      runtimeId: -7,
      dbConversationId: 42,
      persistedAgentType: "codex",
    })

    renderMessageList({ conversationId: -7, agentType: "grok" })

    expect(screen.getByTestId("grok-conversation-provider")).toHaveAttribute(
      "data-grok-conversation-id",
      "none"
    )
  })

  it("does not provide a virtual Grok id without a durable binding", () => {
    seedHistory(undefined, {
      runtimeId: -7,
      dbConversationId: null,
      persistedAgentType: "grok",
    })

    renderMessageList({ conversationId: -7, agentType: "grok" })

    expect(screen.getByTestId("grok-conversation-provider")).toHaveAttribute(
      "data-grok-conversation-id",
      "none"
    )
  })

  it("rejects caller non-Grok identity when the persisted summary is Grok", () => {
    seedHistory(undefined, {
      runtimeId: 42,
      dbConversationId: 42,
      persistedAgentType: "grok",
    })

    renderMessageList({ conversationId: 42, agentType: "codex" })

    expect(screen.getByTestId("grok-conversation-provider")).toHaveAttribute(
      "data-grok-conversation-id",
      "none"
    )
  })

  it("reacts when a mounted virtual Grok conversation gains a durable binding", async () => {
    seedHistory(undefined, {
      runtimeId: -7,
      dbConversationId: null,
      persistedAgentType: "grok",
    })
    renderMessageList({ conversationId: -7, agentType: "grok" })
    expect(screen.getByTestId("grok-conversation-provider")).toHaveAttribute(
      "data-grok-conversation-id",
      "none"
    )

    act(() => {
      useConversationRuntimeStore.setState((state) => {
        const session = state.byConversationId.get(-7)
        if (!session) throw new Error("virtual conversation fixture is missing")
        const byConversationId = new Map(state.byConversationId)
        byConversationId.set(-7, { ...session, dbConversationId: 42 })
        return { byConversationId }
      })
    })

    await waitFor(() => {
      expect(screen.getByTestId("grok-conversation-provider")).toHaveAttribute(
        "data-grok-conversation-id",
        "42"
      )
    })
  })

  it("marks compatibility live history as live and persisted history as complete", () => {
    seedHistory([userTurn("u1", "hello"), assistantTurn("a1", "prior reply")], {
      persistedAgentType: "grok",
    })
    act(() => {
      useConversationRuntimeStore
        .getState()
        .actions.setLiveMessage(CID, liveMessage("compat live reply"), true)
    })

    renderMessageList({ agentType: "grok" })

    expect(
      screen.getByText("prior reply").closest("[data-grok-phase]")
    ).toHaveAttribute("data-grok-phase", "complete")
    expect(screen.getByText("prior reply")).toHaveAttribute(
      "data-grok-session-image-eligible",
      "true"
    )
    expect(
      screen.getByText("compat live reply").closest("[data-grok-phase]")
    ).toHaveAttribute("data-grok-phase", "live")
    expect(screen.getByText("compat live reply")).toHaveAttribute(
      "data-grok-session-image-eligible",
      "true"
    )
  })
})

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
  const origin = groupOverrides.autonomous_origin
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
      grokSessionImageTextParts: new Set(),
      ...groupOverrides,
    },
    phase: "persisted",
    showStats: false,
    isRoleTransition: false,
    previousUserIndex: null,
    sourceTurns: [
      {
        id,
        role: "assistant",
        blocks: [],
        timestamp: "",
        ...(origin != null ? { autonomous_origin: origin } : {}),
      },
    ],
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

describe("MessageListView history prepend keys", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    virtualizerKeysSpy.mockClear()
    seedHistory([
      userTurn("u1", "first"),
      assistantTurn("a1", "first reply"),
      userTurn("u2", "second"),
      assistantTurn("a2", "second reply"),
    ])
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("keeps existing virtual row keys stable when older turns are prepended", () => {
    renderMessageList()
    const before = virtualizerKeysSpy.mock.calls[
      virtualizerKeysSpy.mock.calls.length - 1
    ]?.[0] as string[]

    act(() => {
      useConversationRuntimeStore.setState((state) => {
        const current = state.byConversationId.get(CID)!
        const next = new Map(state.byConversationId)
        next.set(CID, {
          ...current,
          detail: {
            ...current.detail!,
            turns: [
              userTurn("u0", "older"),
              assistantTurn("a0", "older reply"),
              ...current.detail!.turns,
            ],
          },
        })
        return { byConversationId: next }
      })
    })

    const after = virtualizerKeysSpy.mock.calls[
      virtualizerKeysSpy.mock.calls.length - 1
    ]?.[0] as string[]
    expect(after.slice(-before.length)).toEqual(before)
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

    renderMessageList({ agentType: "grok" })

    expect(screen.getByText(assistantText)).toHaveAttribute(
      "data-autolink-local-paths",
      "true"
    )
    expect(screen.getByText(assistantText)).toHaveAttribute(
      "data-grok-session-image-eligible",
      "true"
    )
    expect(screen.getByText(toolText)).toHaveAttribute(
      "data-autolink-local-paths",
      "false"
    )
    expect(screen.getByText(toolText)).toHaveAttribute(
      "data-grok-session-image-eligible",
      "false"
    )
  })

  it("keeps standalone source-tool Markdown outside Grok image scope", () => {
    const toolText = "![tool](images/tool.png)"
    seedHistory([userTurn("u1", "hello"), toolTurn("t1", toolText)], {
      persistedAgentType: "grok",
    })

    renderMessageList({ agentType: "grok" })

    expect(screen.getByText(toolText)).toHaveAttribute(
      "data-grok-session-image-eligible",
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

  it("uses the bound db parent id for virtual historical continuation cards", () => {
    const runtimeId = -9
    seedHistory(
      [
        userTurn("u1", "continue"),
        workUnitRunTurn("a1", "continue-1", "run-2", {
          toolName: "continue_delegation",
          targetTaskId: "run-1",
        }),
      ],
      { runtimeId, dbConversationId: CID }
    )

    renderMessageList({ conversationId: runtimeId })

    expect(screen.getByTestId("delegation-work-unit")).toHaveAttribute(
      "data-parent-conversation-id",
      String(CID)
    )
    expect(
      (lastOverlayProps().delegations ?? []).find(
        (source) => source.parentToolUseId === "continue-1"
      )
    ).toMatchObject({ parentConversationId: CID })
  })

  it("renders one historical card per turn and a complete mixed status call", () => {
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

    const cards = screen.getAllByTestId("delegation-work-unit")
    expect(cards).toHaveLength(2)
    expect(cards[0]).toHaveAttribute("data-work-unit-key", "wu:unit-a:run-1")
    expect(cards[0]).toHaveAttribute("data-source-count", "1")
    expect(cards[1]).toHaveAttribute("data-work-unit-key", "wu:unit-a:run-2")
    expect(cards[1]).toHaveAttribute("data-source-count", "1")
    expect(screen.getByText("checkpoint explanation")).toBeInTheDocument()
    expect(screen.getByText("still working")).toBeInTheDocument()
    expect(screen.getByTestId("delegation-status-residual")).toHaveAttribute(
      "data-visible-task-ids",
      "all"
    )
    expect(
      (lastOverlayProps().delegations ?? []).map(
        (delegation) => delegation.parentToolUseId
      )
    ).toEqual(["tool-1", "tool-2"])
  })

  it("renders replayed snapshots of one exact run as a single card", () => {
    seedHistory([
      userTurn("u1", "start"),
      workUnitRunTurn("a1", "same-tool", "run-1"),
      workUnitRunTurn("a2", "same-tool", "run-1", {
        generation: 2,
        terminal: true,
      }),
    ])

    renderMessageList()

    const card = screen.getByTestId("delegation-work-unit")
    expect(card).toHaveAttribute("data-work-unit-key", "wu:unit-a:run-1")
    expect(card).toHaveAttribute("data-source-count", "2")
    expect(card).toHaveAttribute("data-latest-status", "completed")
  })

  it("applies whole-call folding to promoted localTurns", () => {
    seedHistory([
      userTurn("u1", "start"),
      workUnitRunTurn("a1", "delegate-1", "run-1"),
    ])
    setLocalTurns([workUnitStatusTurn("promoted-mixed", ["run-1", "unknown"])])
    const view = renderMessageList()
    expect(screen.getByTestId("delegation-status-residual")).toHaveAttribute(
      "data-visible-task-ids",
      "all"
    )

    view.unmount()
    setLocalTurns([workUnitStatusTurn("promoted-known", ["run-1"])])
    renderMessageList()
    expect(
      screen.queryByTestId("delegation-status-residual")
    ).not.toBeInTheDocument()
    expect(screen.getByTestId("delegation-work-unit")).toBeInTheDocument()
  })

  it("projects a multi-turn persisted session to one card per run", () => {
    const checkpoints = [
      "checkpoint 01",
      "checkpoint 02",
      "checkpoint 03",
      "checkpoint 04",
      "checkpoint 05",
      "checkpoint 06",
      "checkpoint 07",
      "checkpoint 08",
      "checkpoint 09",
      "checkpoint 10",
    ]
    const turns: MessageTurn[] = [
      userTurn("u1", "start parallel work"),
      workUnitRunTurn("a-unit-a-1", "tool-a-1", "run-a-1", {
        workUnitKey: "unit-a",
        childConversationId: 3001,
        generation: 1,
      }),
      workUnitRunTurn("a-unit-b-1", "tool-b-1", "run-b-1", {
        workUnitKey: "unit-b",
        childConversationId: 4002,
        generation: 1,
      }),
    ]
    let currentTaskId = "run-a-1"
    for (let index = 0; index < checkpoints.length; index++) {
      turns.push(
        workUnitStatusTurn(
          `status-${index + 1}`,
          index === checkpoints.length - 1
            ? [currentTaskId, "orphan-run"]
            : [currentTaskId]
        ),
        assistantTurn(`checkpoint-${index + 1}`, checkpoints[index])
      )
      if (index === 2 || index === 5) {
        const generation = index === 2 ? 2 : 3
        const nextTaskId = `run-a-${generation}`
        turns.push(
          workUnitRunTurn(
            `a-unit-a-${generation}`,
            `tool-a-${generation}`,
            nextTaskId,
            {
              toolName: "continue_delegation",
              targetTaskId: currentTaskId,
              workUnitKey: "unit-a",
              childConversationId: 3001,
              generation,
            }
          )
        )
        currentTaskId = nextTaskId
      }
    }
    turns.push(
      workUnitRunTurn("a-unit-a-4", "tool-a-4", "run-a-4", {
        toolName: "continue_delegation",
        targetTaskId: currentTaskId,
        workUnitKey: "unit-a",
        childConversationId: 3001,
        generation: 4,
        terminal: true,
      })
    )
    const original = JSON.parse(JSON.stringify(turns)) as MessageTurn[]

    seedHistory(turns)
    renderMessageList()

    const cards = screen.getAllByTestId("delegation-work-unit")
    // unit-a: gen1 + gen2 + gen3 + terminal gen4; unit-b: gen1
    expect(cards).toHaveLength(5)
    const unitAKeys = cards
      .map((card) => card.getAttribute("data-work-unit-key") ?? "")
      .filter((key) => key.startsWith("wu:unit-a:"))
    const unitBKeys = cards
      .map((card) => card.getAttribute("data-work-unit-key") ?? "")
      .filter((key) => key.startsWith("wu:unit-b:"))
    expect(unitAKeys).toEqual([
      "wu:unit-a:run-a-1",
      "wu:unit-a:run-a-2",
      "wu:unit-a:run-a-3",
      "wu:unit-a:run-a-4",
    ])
    expect(unitBKeys).toEqual(["wu:unit-b:run-b-1"])
    for (const card of cards) {
      expect(card).toHaveAttribute("data-source-count", "1")
    }
    const terminalA = cards.find(
      (card) => card.getAttribute("data-work-unit-key") === "wu:unit-a:run-a-4"
    )
    expect(terminalA).toHaveAttribute("data-latest-status", "completed")
    expect(screen.getAllByTestId("delegation-status-residual")).toHaveLength(1)
    expect(screen.getByTestId("delegation-status-residual")).toHaveAttribute(
      "data-visible-task-ids",
      "all"
    )
    for (const checkpoint of checkpoints) {
      expect(screen.getByText(checkpoint)).toBeInTheDocument()
    }
    expect(turns).toEqual(original)
  })
})

describe("MessageListView interruption marker", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    seedHistory([assistantTurn("a1", " **Conversation interrupted** \n")])
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("hides the exact historical marker on parent and child sessions", () => {
    renderMessageList()
    expect(
      screen.queryByText(/Conversation interrupted/)
    ).not.toBeInTheDocument()
  })
})

describe("MessageListView Codex compaction advisory", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    seedHistory([
      assistantTurn(
        "a1",
        "Warning: Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted.\n\n"
      ),
    ])
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("hides the historical compaction advisory on parent and child sessions", () => {
    renderMessageList()
    expect(screen.queryByText(/Heads up:/)).not.toBeInTheDocument()
  })
})

describe("MessageListView waiting-for-subagents bottom banner", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    enableIncremental()
    seedHistory([
      {
        id: "u1",
        role: "user",
        blocks: [{ type: "text", text: "delegate work" }],
        timestamp: "2026-05-28T00:00:00.000Z",
      },
      assistantTurn("a1", "delegated; waiting"),
    ])
  })

  afterEach(() => {
    cleanup()
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
  })

  it("shows LiveTurnStats with waiting status when continuation owns admission", () => {
    renderMessageList({
      waitingForSubagentsArmedAtMs: Date.parse("2026-05-28T00:00:00.000Z"),
      connStatus: "connected",
    })
    const banner = screen.getByTestId("live-turn-stats")
    expect(banner).toBeInTheDocument()
    expect(banner).toHaveAttribute("data-status-mode", "waiting_for_subagents")
  })

  it("hides the banner when not waiting and not streaming", () => {
    renderMessageList({
      waitingForSubagentsArmedAtMs: null,
      connStatus: "connected",
    })
    expect(screen.queryByTestId("live-turn-stats")).not.toBeInTheDocument()
  })

  it("records the first streaming banner commit and next paint once", () => {
    const frames = new Map<number, FrameRequestCallback>()
    let nextFrameId = 0
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      nextFrameId += 1
      frames.set(nextFrameId, callback)
      return nextFrameId
    })
    vi.stubGlobal("cancelAnimationFrame", (frameId: number) => {
      frames.delete(frameId)
    })
    const live: LiveMessage = {
      id: "live-banner-1",
      role: "assistant",
      content: [],
      startedAt: 1,
    }
    liveTranscriptStore.rebuild(CID, "conn-1", live, 1)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, live, true)

    const { rerender } = render(
      <StrictMode>{messageListUi({ connStatus: "prompting" })}</StrictMode>
    )

    expect(recordFrontendTurnTrace).toHaveBeenCalledWith({
      phase: "banner_commit",
      conversationId: CID,
      liveMessageId: "live-banner-1",
      hasLiveTranscript: true,
    })
    expect(recordFrontendTurnTrace).not.toHaveBeenCalledWith(
      expect.objectContaining({ phase: "banner_paint" })
    )
    act(() => {
      for (const callback of frames.values()) callback(16)
    })
    expect(recordFrontendTurnTrace).toHaveBeenCalledWith({
      phase: "banner_paint",
      conversationId: CID,
      liveMessageId: "live-banner-1",
      hasLiveTranscript: true,
    })

    rerender(
      <StrictMode>{messageListUi({ connStatus: "prompting" })}</StrictMode>
    )
    expect(
      recordFrontendTurnTrace.mock.calls.filter(
        ([trace]) =>
          (trace as { liveMessageId?: string }).liveMessageId ===
          "live-banner-1"
      )
    ).toHaveLength(2)
  })

  it("latches pre-suspend live tools into the waiting banner", () => {
    const live: LiveMessage = {
      id: "lm-1",
      role: "assistant",
      content: [
        { type: "text", text: "delegating" },
        {
          type: "tool_call",
          info: {
            tool_call_id: "tc-1",
            title: "delegate_to_agent",
            kind: "other",
            status: "completed",
            content: null,
            raw_input: "{}",
            raw_output_chunks: [],
            raw_output_total_bytes: 0,
            locations: null,
            meta: null,
            images: [],
          },
        },
      ],
      startedAt: 1_700_000_000_000,
    }
    liveTranscriptStore.rebuild(CID, "c1", live, 1)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, live, true)

    const { rerender } = renderMessageList({
      waitingForSubagentsArmedAtMs: null,
      connStatus: "prompting",
    })
    expect(screen.getByTestId("live-turn-stats")).toHaveAttribute(
      "data-status-mode",
      "auto"
    )

    // Clear live stream (suspend) but keep waiting banner.
    liveTranscriptStore.remove(CID)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, null, true)
    rerender(
      messageListUi({
        waitingForSubagentsArmedAtMs: Date.parse("2026-05-28T00:01:00.000Z"),
        connStatus: "connected",
      })
    )

    const banner = screen.getByTestId("live-turn-stats")
    expect(banner).toHaveAttribute("data-status-mode", "waiting_for_subagents")
    expect(banner).toHaveAttribute("data-tool-count", "1")
  })
})

function durableChild(
  overrides: Partial<DbConversationSummary> & Pick<DbConversationSummary, "id">
): DbConversationSummary {
  return {
    folder_id: 1,
    title: `child-${overrides.id}`,
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "codex",
    status: "pending_review",
    awaiting_reply_token: null,
    kind: "delegate",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-08-19T11:30:08.000Z",
    updated_at: "2026-08-19T11:41:46.000Z",
    pinned_at: null,
    parent_id: CID,
    parent_tool_use_id: `exec-${overrides.id}`,
    delegation_call_id: `task-${overrides.id}`,
    delegation_task_status: "completed",
    ...overrides,
  }
}

describe("MessageListView sub-agent overlay composition", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    __resetLiveTranscriptStoreForTests()
    __resetStreamingPerformanceConfigForTests()
    subAgentOverlayPropsSpy.mockClear()
    listChildConversationsMock.mockReset()
    listChildConversationsMock.mockResolvedValue([])
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

  it("keeps live lookup virtual while using the bound db parent id for continuation cards", () => {
    const runtimeId = -9
    const live: LiveMessage = {
      id: "live-continuation",
      role: "assistant",
      content: [
        {
          type: "tool_call",
          info: {
            tool_call_id: "live-continue-1",
            title: "continue_delegation",
            kind: "other",
            status: "in_progress",
            content: null,
            raw_input: JSON.stringify({
              task_id: "run-1",
              task: "continue",
              work_unit_key: "unit-a",
              correlation_id: "correlation-1",
            }),
            raw_output_chunks: [],
            raw_output_total_bytes: 0,
            locations: null,
            meta: null,
            images: [],
          },
        },
      ],
      startedAt: 1_700_000_000_000,
    }
    seedHistory([], { runtimeId, dbConversationId: CID })
    act(() => {
      liveTranscriptStore.rebuild(runtimeId, "c1", live, 1)
    })

    renderMessageList({ conversationId: runtimeId })

    expect(screen.getByTestId("tool-part-live-continue-1")).toBeInTheDocument()
    expect(
      screen
        .getByTestId("tool-part-live-continue-1")
        .closest('[data-testid="content-parts"]')
    ).toHaveAttribute("data-parent-conversation-id", String(CID))
    expect(
      (lastOverlayProps().delegations ?? []).find(
        (source) => source.parentToolUseId === "live-continue-1"
      )
    ).toMatchObject({ parentConversationId: CID })
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

  it("forwards inactive state through the incremental overlay path", () => {
    renderMessageList({ isActive: false })
    expect(lastOverlayProps().isActive).toBe(false)
  })

  it("forwards inactive state through the legacy overlay path", () => {
    __resetStreamingPerformanceConfigForTests()
    renderMessageList({ isActive: false })
    expect(lastOverlayProps().isActive).toBe(false)
  })

  it("fails if the incremental overlay drops the explicit workspace root", () => {
    renderMessageList({ workspaceRootPath: "D:\\Repo\\Task7" })
    expect(lastOverlayProps().workspaceRootPath).toBe("D:\\Repo\\Task7")
  })

  it("fails if the direct overlay drops the explicit workspace root", () => {
    __resetStreamingPerformanceConfigForTests()
    renderMessageList({ workspaceRootPath: "D:\\Repo\\Task7" })
    expect(lastOverlayProps().workspaceRootPath).toBe("D:\\Repo\\Task7")
  })

  it.each([
    { mode: "incremental", incremental: true },
    { mode: "legacy", incremental: false },
  ])(
    "updates the $mode workflow overlay to the bound db id",
    async ({ incremental }) => {
      if (!incremental) __resetStreamingPerformanceConfigForTests()
      const runtimeId = -9
      seedHistory(undefined, { runtimeId, dbConversationId: null })

      renderMessageList({ conversationId: runtimeId })
      expect(lastOverlayProps().conversationId).toBe(runtimeId)

      act(() => {
        useConversationRuntimeStore
          .getState()
          .actions.setDbConversationId(runtimeId, CID)
      })

      await waitFor(() => {
        expect(listChildConversationsMock).toHaveBeenCalledWith(CID)
        expect(lastOverlayProps().conversationId).toBe(CID)
      })
    }
  )

  it("fills overlay from durable children when the transcript has no delegate cards", async () => {
    listChildConversationsMock.mockResolvedValue([
      durableChild({ id: 3868, parent_tool_use_id: "exec-newer" }),
      durableChild({
        id: 3867,
        parent_tool_use_id: "exec-older",
        created_at: "2026-08-19T11:30:00.000Z",
        delegation_started_at: "2026-08-19T11:30:00.000Z",
      }),
    ])
    seedHistory([
      userTurn("u1", "after compaction"),
      assistantTurn("a1", "summary only"),
    ])

    renderMessageList()

    await waitFor(() => {
      expect(
        (lastOverlayProps().delegations ?? []).map(
          (delegation) => delegation.parentToolUseId
        )
      ).toEqual(["exec-older", "exec-newer"])
    })
    expect(listChildConversationsMock).toHaveBeenCalledWith(CID)
  })

  it("fills the legacy overlay from durable children when the transcript is empty", async () => {
    __resetStreamingPerformanceConfigForTests()
    listChildConversationsMock.mockResolvedValue([
      durableChild({ id: 3867, parent_tool_use_id: "exec-older" }),
    ])
    seedHistory([])

    renderMessageList()

    await waitFor(() => {
      expect(
        (lastOverlayProps().delegations ?? []).map(
          (delegation) => delegation.parentToolUseId
        )
      ).toEqual(["exec-older"])
    })
  })

  it("queries durable children by the bound db id when the runtime key is virtual", async () => {
    const runtimeId = -9
    listChildConversationsMock.mockResolvedValue([
      durableChild({
        id: 3867,
        parent_id: CID,
        parent_tool_use_id: "exec-older",
      }),
    ])
    seedHistory(
      [userTurn("u1", "after compaction"), assistantTurn("a1", "summary only")],
      { runtimeId, dbConversationId: CID }
    )

    renderMessageList({ conversationId: runtimeId })

    await waitFor(() => {
      expect(listChildConversationsMock).toHaveBeenCalledWith(CID)
      expect(
        (lastOverlayProps().delegations ?? []).map(
          (delegation) => delegation.parentToolUseId
        )
      ).toEqual(["exec-older"])
    })
  })
})

describe("mergeConsecutiveAssistantTurns autonomous origin boundary", () => {
  it("does not merge foreground into autonomous or autonomous into foreground", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", { autonomous_origin: undefined }),
      assistantItem("b", { autonomous_origin: "background_task" }),
      assistantItem("c", { autonomous_origin: undefined }),
    ])
    expect(merged).toHaveLength(3)
  })

  it("does not merge distinct autonomous episode ids", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("grok-autonomous:1:assistant:0", {
        autonomous_origin: "background_task",
      }),
      assistantItem("grok-autonomous:2:assistant:0", {
        autonomous_origin: "background_task",
      }),
    ])
    expect(merged).toHaveLength(2)
  })
})

describe("MessageListView autonomous continuation marker", () => {
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

  it("shows the zh-CN marker above autonomous content and never for historical turns", () => {
    seedHistory([
      userTurn("u1", "hello"),
      assistantTurn("a1", "prior reply"),
      {
        ...assistantTurn("grok-autonomous:x:assistant:0", "continued work"),
        autonomous_origin: "background_task",
      },
    ])

    render(
      <NextIntlClientProvider locale="zh-CN" messages={zhCNMessages}>
        <MessageListView
          conversationId={CID}
          agentType="codex"
          connStatus="connected"
          isActive
          showMessageNav={false}
        />
      </NextIntlClientProvider>
    )

    const markers = screen.getAllByTestId("background-continuation-marker")
    expect(markers).toHaveLength(1)
    expect(markers[0]).toHaveTextContent("后台续写")
    expect(screen.getByText("continued work")).toBeInTheDocument()
    expect(screen.getByText("prior reply")).toBeInTheDocument()
    expect(screen.queryByText("<system-reminder>")).toBeNull()
    expect(screen.queryByText(/Background task/)).toBeNull()
    expect(
      extractTextFromParts([{ type: "text", text: "continued work" }])
    ).not.toContain("后台续写")
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

  it("preserves one reasoning effort on a merged assistant response", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", { reasoning_effort: "high" }),
      assistantItem("b"),
    ])

    const item = merged[0] as TurnItem
    expect(item.group.reasoning_effort).toBe("high")
    expect(item.group.reasoning_efforts).toBeUndefined()
  })

  it("deduplicates merged reasoning efforts in encounter order and ignores blanks", () => {
    const merged = mergeConsecutiveAssistantTurns([
      assistantItem("a", { reasoning_effort: " low " }),
      assistantItem("b", { reasoning_effort: "high" }),
      assistantItem("c", { reasoning_effort: "low" }),
      assistantItem("d", { reasoning_effort: "   " }),
    ])

    const item = merged[0] as TurnItem
    expect(item.group.reasoning_effort).toBe("low")
    expect(item.group.reasoning_efforts).toEqual(["low", "high"])
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
    grokSessionImageTextParts: new Set(),
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
