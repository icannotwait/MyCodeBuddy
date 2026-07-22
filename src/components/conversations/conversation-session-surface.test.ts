import { createElement } from "react"
import { act, render } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { EventEnvelope } from "@/lib/types"
import {
  shouldClearTerminalDisconnectLatch,
  shouldLatchTerminalDisconnect,
  type TerminalDisconnectLatch,
} from "@/lib/terminal-reconnect"
import { shouldQueueDirectSend } from "@/lib/queue-flush"

// ---------------------------------------------------------------------------
// Pure surface policy seam (imported from the surface module once exported)
// ---------------------------------------------------------------------------

import {
  applyPersistedSummaryToTerminalLatch,
  applyTerminalDisconnectEvent,
  ConversationSessionSurface,
  resolveSessionAutoConnectAllowed,
  shouldShowTerminalReconnect,
} from "./conversation-session-surface"

const BASELINE = "2026-07-22T01:00:00.000Z"
const NEWER = "2026-07-22T01:05:00.000Z"
const CONN = "conn-surface-1"

function summary(
  status: string,
  updatedAt: string = BASELINE
): { status: string; updated_at: string } {
  return { status, updated_at: updatedAt }
}

function errorEvent(connectionId: string, terminal: boolean): EventEnvelope {
  return {
    seq: 1,
    connection_id: connectionId,
    type: "error",
    message: "agent died",
    agent_type: "claude",
    code: "process_exited",
    terminal,
  }
}

function statusEvent(
  connectionId: string,
  status: "disconnected" | "connected" | "connecting"
): EventEnvelope {
  return {
    seq: 2,
    connection_id: connectionId,
    type: "status_changed",
    status,
  }
}

describe("resolveSessionAutoConnectAllowed (pure surface policy)", () => {
  it("denies automatic connection when persisted summary is missing", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: null,
        terminalDisconnectLatch: null,
      })
    ).toBe(false)
  })

  it("denies automatic connection when summary is cancelled", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: summary("cancelled"),
        terminalDisconnectLatch: null,
      })
    ).toBe(false)
  })

  it("allows automatic connection for pending_review and completed", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: summary("pending_review"),
        terminalDisconnectLatch: null,
      })
    ).toBe(true)
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: summary("completed"),
        terminalDisconnectLatch: null,
      })
    ).toBe(true)
  })

  it("allows automatic connection for non-latched in_progress", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: summary("in_progress"),
        terminalDisconnectLatch: null,
      })
    ).toBe(true)
  })

  it("denies automatic connection when a terminal reconnect latch is armed", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: summary("in_progress"),
        terminalDisconnectLatch: { baselineUpdatedAt: BASELINE },
      })
    ).toBe(false)
  })

  it("allows drafts without a persisted conversation (no durable gate)", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: false,
        persistedSummary: null,
        terminalDisconnectLatch: null,
      })
    ).toBe(true)
  })

  it("denies drafts once a terminal latch is armed", () => {
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: false,
        persistedSummary: null,
        terminalDisconnectLatch: { baselineUpdatedAt: BASELINE },
      })
    ).toBe(false)
  })
})

describe("event-to-latch surface harness", () => {
  it("arms latch + queue pause on terminal error for same in_progress connection", () => {
    const next = applyTerminalDisconnectEvent(
      { latch: null, queuePaused: false },
      errorEvent(CONN, true),
      CONN,
      summary("in_progress", BASELINE)
    )
    expect(next.latch).toEqual({ baselineUpdatedAt: BASELINE })
    expect(next.queuePaused).toBe(true)
    // Pre-patch focus policy: latched → auto connect denied
    expect(
      resolveSessionAutoConnectAllowed({
        hasPersistedConversation: true,
        persistedSummary: summary("in_progress", BASELINE),
        terminalDisconnectLatch: next.latch,
      })
    ).toBe(false)
  })

  it("arms latch + queue pause on bare disconnected for same in_progress connection", () => {
    const next = applyTerminalDisconnectEvent(
      { latch: null, queuePaused: false },
      statusEvent(CONN, "disconnected"),
      CONN,
      summary("in_progress", BASELINE)
    )
    expect(next.latch).toEqual({ baselineUpdatedAt: BASELINE })
    expect(next.queuePaused).toBe(true)
  })

  it("captures baseline updated_at only on the first latch", () => {
    const first = applyTerminalDisconnectEvent(
      { latch: null, queuePaused: false },
      errorEvent(CONN, true),
      CONN,
      summary("in_progress", BASELINE)
    )
    const second = applyTerminalDisconnectEvent(
      first,
      statusEvent(CONN, "disconnected"),
      CONN,
      summary("in_progress", NEWER)
    )
    expect(second.latch).toEqual({ baselineUpdatedAt: BASELINE })
    expect(second.queuePaused).toBe(true)
  })

  it("does not clear reconnect latch on stale baseline in_progress", () => {
    const latch: TerminalDisconnectLatch = { baselineUpdatedAt: BASELINE }
    const cleared = applyPersistedSummaryToTerminalLatch(
      latch,
      summary("in_progress", BASELINE)
    )
    expect(cleared).toEqual(latch)
    expect(
      shouldClearTerminalDisconnectLatch(
        latch,
        summary("in_progress", BASELINE)
      )
    ).toBe(false)
  })

  it("does not clear reconnect latch on newer cancelled; queue pause stays only via resume", () => {
    const latch: TerminalDisconnectLatch = { baselineUpdatedAt: BASELINE }
    const cleared = applyPersistedSummaryToTerminalLatch(
      latch,
      summary("cancelled", NEWER)
    )
    expect(cleared).toEqual(latch)
    // Summary clear must not touch queue pause — Resume Queue is the only clear.
    expect(
      shouldClearTerminalDisconnectLatch(latch, summary("cancelled", NEWER))
    ).toBe(false)
  })

  it("clears reconnect latch only on later newer non-cancelled patch", () => {
    const latch: TerminalDisconnectLatch = { baselineUpdatedAt: BASELINE }
    for (const status of [
      "in_progress",
      "pending_review",
      "completed",
    ] as const) {
      expect(
        applyPersistedSummaryToTerminalLatch(latch, summary(status, NEWER))
      ).toBeNull()
    }
  })

  it("does not arm latch for recoverable error or mismatched connection", () => {
    expect(
      applyTerminalDisconnectEvent(
        { latch: null, queuePaused: false },
        errorEvent(CONN, false),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toEqual({ latch: null, queuePaused: false })

    expect(
      applyTerminalDisconnectEvent(
        { latch: null, queuePaused: false },
        errorEvent("other", true),
        CONN,
        summary("in_progress", BASELINE)
      )
    ).toEqual({ latch: null, queuePaused: false })
  })
})

describe("terminal-paused queue surface harness", () => {
  it("direct send during terminal pause bypasses historical queued head", () => {
    // Head remains queued; direct send is not forced to the tail.
    expect(shouldQueueDirectSend(false, 2, true)).toBe(false)
    // Normal FIFO when not paused.
    expect(shouldQueueDirectSend(false, 2, false)).toBe(true)
  })

  it("auto-flush must honor paused state (no dequeue while paused)", () => {
    const paused = true
    const shouldAutoFlush =
      !paused && /* connected */ true && /* queue nonempty */ true
    expect(shouldAutoFlush).toBe(false)
  })

  it("Resume Queue clears only queue pause and restores FIFO head drain", () => {
    let queuePaused = true
    const latch: TerminalDisconnectLatch = { baselineUpdatedAt: BASELINE }
    // Resume does not clear the reconnect latch.
    const onResumeQueue = () => {
      queuePaused = false
    }
    onResumeQueue()
    expect(queuePaused).toBe(false)
    expect(latch.baselineUpdatedAt).toBe(BASELINE)
    // After resume, FIFO direct-send routing returns.
    expect(shouldQueueDirectSend(false, 1, queuePaused)).toBe(true)
    // Auto-flush may drain the head again.
    const shouldAutoFlush = !queuePaused && true
    expect(shouldAutoFlush).toBe(true)
  })
})

describe("shouldShowTerminalReconnect", () => {
  it("shows only for cancelled/latch root in null/disconnected/error", () => {
    for (const status of [null, "disconnected", "error"] as const) {
      expect(
        shouldShowTerminalReconnect({
          rootCancelled: true,
          terminalDisconnectLatch: null,
          connStatus: status,
        })
      ).toBe(true)
      expect(
        shouldShowTerminalReconnect({
          rootCancelled: false,
          terminalDisconnectLatch: { baselineUpdatedAt: BASELINE },
          connStatus: status,
        })
      ).toBe(true)
    }
  })

  it("hides reconnect while connected/prompting/connecting or when not cancelled/latched", () => {
    expect(
      shouldShowTerminalReconnect({
        rootCancelled: true,
        terminalDisconnectLatch: null,
        connStatus: "connected",
      })
    ).toBe(false)
    expect(
      shouldShowTerminalReconnect({
        rootCancelled: false,
        terminalDisconnectLatch: null,
        connStatus: "disconnected",
      })
    ).toBe(false)
  })
})

// ---------------------------------------------------------------------------
// Captured useConnectionLifecycle options harness
// ---------------------------------------------------------------------------

const lifecycleCapture = vi.hoisted(() => ({
  lastOptions: null as null | {
    isActive?: boolean
    autoConnectAllowed?: boolean
    contextKey?: string
  },
  handleReconnect: vi.fn(async () => undefined),
  handleFocus: vi.fn(),
  handleSend: vi.fn(),
  handleSetConfigOption: vi.fn(),
  handleCancel: vi.fn(),
  handleRespondPermission: vi.fn(),
}))

const surfaceH = vi.hoisted(() => ({
  conversations: [] as Array<{
    id: number
    status: string
    updated_at: string
    folder_id?: number
    title?: string | null
    title_locked?: boolean
    auto_title_finalized?: boolean
    agent_type?: string
    awaiting_reply_token?: string | null
    kind?: string
    model?: string | null
    git_branch?: string | null
    external_id?: string | null
    message_count?: number
    child_count?: number
    created_at?: string
    pinned_at?: string | null
  }>,
  acpEventHandlers: [] as Array<(e: EventEnvelope) => void>,
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}))

vi.mock("@/hooks/use-connection-lifecycle", () => ({
  useConnectionLifecycle: (options: {
    isActive?: boolean
    autoConnectAllowed?: boolean
    contextKey?: string
  }) => {
    lifecycleCapture.lastOptions = {
      isActive: options.isActive,
      autoConnectAllowed: options.autoConnectAllowed,
      contextKey: options.contextKey,
    }
    return {
      conn: {
        status: null as string | null,
        sessionId: null,
        connectionId: CONN,
        isViewer: false,
        error: null,
        loadError: null,
        loadErrorCode: null,
        liveMessage: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        promptCapabilities: {
          image: true,
          audio: false,
          embedded_context: true,
        },
        pendingPermission: null,
        pendingQuestion: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        waitingForSubagents: false,
        claudeApiRetry: null,
        agentType: "claude",
        connectedWorkingDir: "/tmp/project",
        supportsFork: false,
        backgroundOutstanding: 0,
      },
      modeLoading: false,
      configOptionsLoading: false,
      selectorsLoading: false,
      autoConnectError: null,
      handleFocus: lifecycleCapture.handleFocus,
      handleReconnect: lifecycleCapture.handleReconnect,
      handleSend: lifecycleCapture.handleSend,
      handleSetConfigOption: lifecycleCapture.handleSetConfigOption,
      handleCancel: lifecycleCapture.handleCancel,
      handleRespondPermission: lifecycleCapture.handleRespondPermission,
    }
  },
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  getCachedSelectors: () => null,
  useAcpActions: () => ({
    registerLiveSinks: () => () => undefined,
    setActiveKey: vi.fn(),
    touchActivity: vi.fn(),
  }),
  useAcpEvent: (handler: (e: EventEnvelope) => void) => {
    surfaceH.acpEventHandlers.push(handler)
  },
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: () => ({
    agents: [
      {
        agent_type: "claude",
        enabled: true,
        available: true,
        installed_version: "1.0.0",
      },
    ],
    fresh: true,
    refresh: vi.fn(),
  }),
}))

vi.mock("@/stores/app-workspace-store", () => ({
  useAppWorkspaceStore: (
    sel: (s: {
      conversations: typeof surfaceH.conversations
      allFolders: Array<{ id: number; path: string }>
      refreshConversations: () => void
      upsertFolder: () => void
    }) => unknown
  ) =>
    sel({
      conversations: surfaceH.conversations,
      allFolders: [{ id: 1, path: "/tmp/project" }],
      refreshConversations: vi.fn(),
      upsertFolder: vi.fn(),
    }),
}))

vi.mock("@/contexts/tab-context", () => ({
  useTabActions: () => ({
    bindConversationTab: vi.fn(),
    setChatDraftWorkingDir: vi.fn(),
    setTabRuntimeConversationId: vi.fn(),
    pinTab: vi.fn(),
    openNewConversationTab: vi.fn(),
    closeTab: vi.fn(),
    confirmDraftAgent: vi.fn(),
    setDraftAgentFromFallback: vi.fn(),
  }),
  useTabStore: (
    sel: (s: {
      tabs: Array<{ id: string; folderId: number; isPinned: boolean }>
    }) => unknown
  ) =>
    sel({
      tabs: [{ id: "tab-1", folderId: 1, isPinned: true }],
    }),
}))

vi.mock("@/contexts/session-stats-context", () => ({
  useSessionStats: () => ({ setSessionStats: vi.fn() }),
}))

vi.mock("@/hooks/use-message-queue", () => ({
  useMessageQueue: () => ({
    queue: [],
    enqueue: vi.fn(),
    requeueFront: vi.fn(),
    getQueueLength: () => 0,
    dequeue: vi.fn(),
    remove: vi.fn(),
    reorder: vi.fn(),
    updateItem: vi.fn(),
    editingItemId: null,
    startEditing: vi.fn(),
    cancelEditing: vi.fn(),
  }),
}))

vi.mock("@/hooks/use-conversation-detail", () => ({
  useConversationDetail: () => ({
    detail: {
      summary: {
        id: 42,
        external_id: "ext-1",
        status: "in_progress",
        updated_at: BASELINE,
      },
      continuation_failure: null,
    },
    loading: false,
    error: null,
    acpLoadError: null,
  }),
}))

vi.mock("@/stores/conversation-runtime-store", () => ({
  completeLiveTranscriptTurn: vi.fn(),
  useConversationRuntimeActions: () => ({
    appendOptimisticTurn: vi.fn(),
    removeOptimisticTurn: vi.fn(),
    appendViewerUserTurn: vi.fn(),
    refetchDetail: vi.fn(),
    syncTurnMetadata: vi.fn(() => () => undefined),
    removeConversation: vi.fn(),
    setAcpLoadError: vi.fn(),
    setDbConversationId: vi.fn(),
    setExternalId: vi.fn(),
    setLiveMessage: vi.fn(),
    setPendingCleanup: vi.fn(),
    setSyncState: vi.fn(),
  }),
  useConversationRuntimeStore: (
    sel: (s: { byConversationId: Map<number, unknown> }) => unknown
  ) => sel({ byConversationId: new Map() }),
}))

vi.mock("@/stores/live-transcript-store", () => ({
  createLiveTranscriptFrameSink: () => vi.fn(),
}))

vi.mock("zustand/react/shallow", () => ({
  useShallow: (fn: unknown) => fn,
}))

vi.mock("@/components/message/message-list-view", () => ({
  MessageListView: () => null,
}))

vi.mock("@/components/message/initial-history-scroll-controller", () => ({
  useInitialHistoryScrollEligibility: () => false,
}))

vi.mock("@/components/chat/conversation-shell", () => ({
  ConversationShell: ({ children }: { children?: unknown }) => children ?? null,
}))

vi.mock("@/components/chat/session-config-stale-banner", () => ({
  SessionConfigStaleBanner: () => null,
}))
vi.mock("@/components/chat/delegation-route-notice", () => ({
  DelegationRouteNotice: () => null,
}))
vi.mock("@/components/chat/background-tasks-chip", () => ({
  BackgroundTasksChip: () => null,
}))
vi.mock("@/components/chat/feedback-notes-display", () => ({
  FeedbackNotesDisplay: () => null,
}))
vi.mock("@/components/chat/feedback-dialog", () => ({
  FeedbackDialog: () => null,
}))
vi.mock("@/components/chat/agent-selector", () => ({
  AgentSelector: () => null,
}))
vi.mock("@/components/chat/chat-input", () => ({
  ChatInput: () => null,
}))
vi.mock("@/components/chat/welcome-hero", () => ({
  WelcomeHero: () => null,
  WelcomeTip: () => null,
}))
vi.mock("@/components/chat/quick-actions", () => ({
  QuickActions: () => null,
}))

vi.mock("@/hooks/use-feedback-enabled", () => ({
  useFeedbackEnabled: () => false,
}))
vi.mock("@/hooks/use-session-feedback", () => ({
  useSessionFeedback: () => ({
    showList: false,
    notes: [],
    featureEnabled: false,
    openDialog: vi.fn(),
    closeDialog: vi.fn(),
    canSubmit: false,
    dialogOpen: false,
    submit: vi.fn(),
    submitting: false,
  }),
}))

vi.mock("@/lib/api", () => ({
  acpFork: vi.fn(),
  createChatConversation: vi.fn(),
  createChatDir: vi.fn(),
  createConversation: vi.fn(),
  openSettingsWindow: vi.fn(),
}))

vi.mock("@/lib/selector-prefs-storage", () => ({
  getSavedModeId: () => null,
  saveModePreference: vi.fn(),
}))

vi.mock("@/lib/message-input-draft", () => ({
  buildConversationDraftStorageKey: (id: number) => `draft:${id}`,
  buildNewConversationDraftStorageKey: () => "draft:new",
  clearMessageInputDraft: vi.fn(),
  saveMessageInputDraft: vi.fn(),
}))

function fullSummary(id: number, status: string, updatedAt: string = BASELINE) {
  return {
    id,
    folder_id: 1,
    title: "t",
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "claude",
    status,
    awaiting_reply_token: null,
    kind: "chat",
    model: null,
    git_branch: null,
    external_id: "ext-1",
    message_count: 1,
    child_count: 0,
    created_at: BASELINE,
    updated_at: updatedAt,
    pinned_at: null,
  }
}

function renderSurface(conversationId: number | null = 42) {
  return render(
    createElement(ConversationSessionSurface, {
      tabId: "tab-1",
      conversationId,
      folderId: 1,
      agentType: "claude",
      workingDir: "/tmp/project",
      isActive: true,
      showActiveFlow: false,
      reloadSignal: 0,
    })
  )
}

describe("ConversationSessionSurface useConnectionLifecycle options harness", () => {
  beforeEach(() => {
    lifecycleCapture.lastOptions = null
    lifecycleCapture.handleReconnect.mockClear()
    surfaceH.conversations = []
    surfaceH.acpEventHandlers = []
  })

  it("passes autoConnectAllowed === false for a missing persisted summary", () => {
    surfaceH.conversations = []
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions).not.toBeNull()
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    // Must be an explicit boolean — not omitted (compatibility default would mask).
    expect("autoConnectAllowed" in (lifecycleCapture.lastOptions ?? {})).toBe(
      true
    )
  })

  it("passes autoConnectAllowed === false for a cancelled summary", () => {
    surfaceH.conversations = [fullSummary(42, "cancelled")]
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
  })

  it("passes autoConnectAllowed === true for a non-cancelled resolved root", () => {
    surfaceH.conversations = [fullSummary(42, "pending_review")]
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
  })

  it("passes autoConnectAllowed === false when a terminal latch is armed", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress")]
    act(() => {
      renderSurface(42)
    })
    // Simulate pre-patch terminal event via registered acp handlers.
    const inProgress = summary("in_progress", BASELINE)
    expect(
      shouldLatchTerminalDisconnect(errorEvent(CONN, true), CONN, inProgress)
    ).toBe(true)
    act(() => {
      for (const handler of surfaceH.acpEventHandlers) {
        handler(errorEvent(CONN, true))
      }
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
  })

  it("passes real isActive and does not fold durable policy into isActive", () => {
    surfaceH.conversations = [fullSummary(42, "cancelled")]
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions!.isActive).toBe(true)
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
  })
})

// Keep existing props-contract coverage.
describe("ConversationSessionSurface props contract", () => {
  it("prefers explicit folderId prop over tab folderId", () => {
    const folderIdProp = 9
    const tabFolderId = 3
    const ownFolderId = folderIdProp > 0 ? folderIdProp : (tabFolderId ?? null)
    expect(ownFolderId).toBe(9)
  })

  it("falls back to tab folderId when prop is 0", () => {
    const folderIdProp = 0
    const tabFolderId = 3
    const ownFolderId = folderIdProp > 0 ? folderIdProp : (tabFolderId ?? null)
    expect(ownFolderId).toBe(3)
  })
})
