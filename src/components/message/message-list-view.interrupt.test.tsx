/**
 * Task 6: historical render suppress of Conversation interrupted on delegated
 * children; Response interrupted footer preserved; user-role not suppressed.
 */
import { act, cleanup, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"
import { forwardRef, useImperativeHandle, type Ref } from "react"
import type { MessageTurn } from "@/lib/types"
import enMessages from "@/i18n/messages/en.json"
import {
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import {
  __resetLiveTranscriptStoreForTests,
  liveTranscriptStore,
} from "@/stores/live-transcript-store"
import { __resetStreamingPerformanceConfigForTests } from "@/lib/acp/streaming-performance-config"

const { virtualizerScrollToIndex } = vi.hoisted(() => ({
  virtualizerScrollToIndex: vi.fn(),
}))

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
  }: {
    parts: Array<{ type: string; text?: string }>
  }) => (
    <div data-testid="content-parts">
      {parts.map((part, index) =>
        part.type === "text" ? (
          <span key={index} data-testid="assistant-text">
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

vi.mock("@/components/chat/sub-agent-overlay", () => ({
  SubAgentOverlay: () => <div data-testid="sub-agent-overlay-capture" />,
}))

vi.mock("./conversation-message-nav", () => ({
  ConversationMessageNav: () => null,
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAgentThinkingVisibility: () => false,
}))

vi.mock("@/lib/perf/streaming-perf-recorder", () => ({
  streamingPerfRecorder: {
    countRender: vi.fn(),
    markReactCommit: vi.fn(),
    isActive: () => false,
  },
}))

vi.mock("./initial-history-scroll-controller", () => ({
  InitialHistoryScrollController: () => null,
}))

import { MessageListView } from "./message-list-view"

const CID = 601
const PARENT_ID = 99
const MARKER = "*Conversation interrupted*"

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

function seedHistory(
  turns: MessageTurn[],
  options: { parentId?: number | null } = {}
) {
  const parentId = options.parentId
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
              external_id: "sid-interrupt",
              message_count: turns.length,
              child_count: 0,
              created_at: "2026-05-28T00:00:00.000Z",
              updated_at: "2026-05-28T00:00:00.000Z",
              pinned_at: null,
              ...(parentId !== undefined ? { parent_id: parentId } : {}),
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
          externalId: "sid-interrupt",
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
    conversationIdByExternalId: new Map([["sid-interrupt", CID]]),
  })
}

function renderMessageList() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageListView
        conversationId={CID}
        agentType="codex"
        connStatus="connected"
        isActive
        showMessageNav={false}
      />
    </NextIntlClientProvider>
  )
}

describe("MessageListView Conversation interrupted suppress", () => {
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

  it("hides historical assistant Conversation interrupted marker when delegated", () => {
    seedHistory([userTurn("u1", "hello"), assistantTurn("a1", MARKER)], {
      parentId: PARENT_ID,
    })

    renderMessageList()

    expect(screen.queryByText(MARKER)).toBeNull()
    // User prompt still renders; only the assistant marker is suppressed.
    expect(screen.getByText("hello")).toBeInTheDocument()
  })

  it("still shows Conversation interrupted assistant text on standalone sessions", () => {
    seedHistory([userTurn("u1", "hello"), assistantTurn("a1", MARKER)], {
      parentId: null,
    })

    renderMessageList()

    expect(screen.getByText(MARKER)).toBeInTheDocument()
  })

  it("does not suppress user-role identical Conversation interrupted text", () => {
    seedHistory([userTurn("u1", MARKER), assistantTurn("a1", "ok")], {
      parentId: PARENT_ID,
    })

    renderMessageList()

    // User bubble still shows the identical marker text.
    expect(screen.getByText(MARKER)).toBeInTheDocument()
    expect(screen.getByText("ok")).toBeInTheDocument()
  })

  it("keeps Response interrupted footer when outcome is interrupted on a delegated child", () => {
    seedHistory(
      [
        userTurn("u1", "hello"),
        {
          ...assistantTurn("a1", MARKER),
          outcome: {
            status: "interrupted",
            stop_reason: "cancelled",
            source: "user_stop",
            provider_turn_id: "turn-child",
          },
        },
      ],
      { parentId: PARENT_ID }
    )

    renderMessageList()

    expect(screen.queryByText(MARKER)).toBeNull()
    const footers = screen.getAllByTestId("response-interrupted-footer")
    expect(footers).toHaveLength(1)
    expect(footers[0]).toHaveTextContent("Response interrupted")
  })

  it("marks live transcript store as delegation child from parent_id", () => {
    seedHistory([userTurn("u1", "hello"), assistantTurn("a1", "partial")], {
      parentId: PARENT_ID,
    })

    renderMessageList()

    act(() => {
      // Effect should have registered the conversation as a delegated child.
      liveTranscriptStore.rebuild(
        CID,
        "c1",
        {
          id: "live-1",
          role: "assistant",
          content: [{ type: "text", text: MARKER }],
          startedAt: 1,
        },
        1
      )
    })

    const snap = liveTranscriptStore.getConversation(CID)
    const texts =
      snap?.segmentIds
        .map((id) => snap.segments.get(id))
        .filter((s) => s?.type === "text")
        .map((s) => (s && s.type === "text" ? s.text : "")) ?? []
    expect(texts).not.toContain(MARKER)
  })
})
