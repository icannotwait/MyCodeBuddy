import { createElement, useEffect, useReducer, useRef } from "react"
import { act, render } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { EventEnvelope } from "@/lib/types"
import {
  shouldClearTerminalDisconnectLatch,
  shouldLatchTerminalDisconnect,
  type TerminalDisconnectLatch,
} from "@/lib/terminal-reconnect"
import { shouldQueueDirectSend } from "@/lib/queue-flush"
import { createConversation } from "@/lib/api"

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
/** Connection transition fixtures (A → B). */
const CONN_A = "conn-surface-A"
const CONN_B = "conn-surface-B"

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

  it("captures baseline updated_at only on the first latch when summary does not clear", () => {
    const first = applyTerminalDisconnectEvent(
      { latch: null, queuePaused: false },
      errorEvent(CONN, true),
      CONN,
      summary("in_progress", BASELINE)
    )
    // Same-baseline in_progress: preserve original first-arm baseline.
    const sameBaseline = applyTerminalDisconnectEvent(
      first,
      statusEvent(CONN, "disconnected"),
      CONN,
      summary("in_progress", BASELINE)
    )
    expect(sameBaseline.latch).toEqual({ baselineUpdatedAt: BASELINE })
    expect(sameBaseline.queuePaused).toBe(true)
  })

  it("re-arms baseline when delivery summary would clear the prior latch", () => {
    const first = applyTerminalDisconnectEvent(
      { latch: null, queuePaused: false },
      errorEvent(CONN, true),
      CONN,
      summary("in_progress", BASELINE)
    )
    // Newer non-cancelled root would clear X; terminal for Y re-arms at Y.
    const rebased = applyTerminalDisconnectEvent(
      first,
      statusEvent(CONN, "disconnected"),
      CONN,
      summary("in_progress", NEWER)
    )
    expect(rebased.latch).toEqual({ baselineUpdatedAt: NEWER })
    expect(rebased.queuePaused).toBe(true)
  })

  it("preserves prior baseline when delivery summary is newer cancelled", () => {
    const first = applyTerminalDisconnectEvent(
      { latch: null, queuePaused: false },
      errorEvent(CONN, true),
      CONN,
      summary("in_progress", BASELINE)
    )
    // Cancelled is not a latch arm target (shouldLatch requires in_progress),
    // so this only documents preserve-when-not-arming path via direct clear.
    expect(
      shouldClearTerminalDisconnectLatch(
        first.latch,
        summary("cancelled", NEWER)
      )
    ).toBe(false)
    expect(
      applyPersistedSummaryToTerminalLatch(
        first.latch,
        summary("cancelled", NEWER)
      )
    ).toEqual({ baselineUpdatedAt: BASELINE })
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

describe("terminal-paused queue policy (pure helpers)", () => {
  it("direct send during terminal pause bypasses historical queued head", () => {
    // Head remains queued; direct send is not forced to the tail.
    expect(shouldQueueDirectSend(false, 2, true)).toBe(false)
    // Normal FIFO when not paused.
    expect(shouldQueueDirectSend(false, 2, false)).toBe(true)
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

type CapturedShellProps = {
  queuePaused?: boolean
  onResumeQueue?: () => void
  showReconnect?: boolean
  onReconnect?: () => void
  onSend?: (
    draft: {
      blocks: Array<{ type: "text"; text: string }>
      displayText: string
    },
    modeId?: string | null
  ) => void
  queue?: Array<{ id: string; draft: unknown; modeId: string | null }>
  children?: unknown
}

type QueueItem = {
  id: string
  draft: {
    blocks: Array<{ type: "text"; text: string }>
    displayText: string
  }
  modeId: string | null
}

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
  /** Drives lifecycle mock `conn.status` (flush + send readiness). */
  connStatus: null as string | null,
  /**
   * Mutable live current bound connection id for `useConnectionStore`.
   * Models provider map updates that land before passive ACP handler refresh.
   */
  currentConnectionId: "conn-surface-1" as string | null,
  /**
   * What lifecycle mock exposes as `conn.connectionId` (render-time only).
   * Transition tests leave this on A while `currentConnectionId` advances to B.
   */
  lifecycleConnectionId: "conn-surface-1" as string | null,
  queueItems: [] as QueueItem[],
  dequeueCalls: 0,
  shellProps: null as CapturedShellProps | null,
  /** Notify workspace-store mock subscribers (Zustand-like). */
  notifyWorkspace: null as null | (() => void),
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
        status: surfaceH.connStatus,
        sessionId: null,
        connectionId: surfaceH.lifecycleConnectionId,
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
  // Stable API; getConnection reads mutable currentConnectionId at call time
  // (models production: map updated before notifyRawSubscribers).
  useConnectionStore: () => ({
    getConnection: (key: string) => {
      if (key !== "tab-1") return undefined
      if (surfaceH.currentConnectionId == null) return undefined
      return { connectionId: surfaceH.currentConnectionId }
    },
    getActiveKey: () => null,
    subscribeKey: () => () => undefined,
    subscribeActiveKey: () => () => undefined,
  }),
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

vi.mock("@/stores/app-workspace-store", () => {
  type WorkspaceSlice = {
    conversations: typeof surfaceH.conversations
    allFolders: Array<{ id: number; path: string }>
    refreshConversations: () => void
    upsertFolder: () => void
  }
  const getWorkspaceSlice = (): WorkspaceSlice => ({
    conversations: surfaceH.conversations,
    allFolders: [{ id: 1, path: "/tmp/project" }],
    refreshConversations: vi.fn(),
    upsertFolder: vi.fn(),
  })
  // Match production Zustand: selector subscriptions re-render on notify +
  // getState() for delivery-time reads inside event callbacks.
  const listeners = new Set<() => void>()
  surfaceH.notifyWorkspace = () => {
    for (const listener of listeners) listener()
  }
  const useAppWorkspaceStore = Object.assign(
    (sel: (s: WorkspaceSlice) => unknown) => {
      // Subscribe so store patches can settle the clear effect without a
      // same-prop memo bailout (production Zustand notifies subscribers).
      const [, bump] = useReducer((n: number) => n + 1, 0)
      useEffect(() => {
        const listener = () => bump()
        listeners.add(listener)
        return () => {
          listeners.delete(listener)
        }
      }, [])
      return sel(getWorkspaceSlice())
    },
    { getState: () => getWorkspaceSlice() }
  )
  return { useAppWorkspaceStore }
})

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
    get queue() {
      return surfaceH.queueItems
    },
    enqueue: vi.fn(
      (draft: QueueItem["draft"], modeId: string | null = null): QueueItem => {
        const item: QueueItem = {
          id: `q-${surfaceH.queueItems.length + 1}`,
          draft,
          modeId,
        }
        surfaceH.queueItems.push(item)
        return item
      }
    ),
    requeueFront: vi.fn(),
    getQueueLength: () => surfaceH.queueItems.length,
    dequeue: () => {
      surfaceH.dequeueCalls += 1
      return surfaceH.queueItems.shift()
    },
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
  ConversationShell: (props: CapturedShellProps) => {
    surfaceH.shellProps = {
      queuePaused: props.queuePaused,
      onResumeQueue: props.onResumeQueue,
      showReconnect: props.showReconnect,
      onReconnect: props.onReconnect,
      onSend: props.onSend,
      queue: props.queue,
    }
    return props.children ?? null
  },
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
  // Report a usable agent once so draft surfaces can first-send / auto-connect
  // (production AgentSelector probes installs and calls onAgentsLoaded).
  AgentSelector: ({
    onAgentsLoaded,
  }: {
    onAgentsLoaded?: (
      agents: Array<{ enabled: boolean; available: boolean }>
    ) => void
  }) => {
    const reported = useRef(false)
    useEffect(() => {
      if (reported.current) return
      reported.current = true
      onAgentsLoaded?.([{ enabled: true, available: true }])
    }, [onAgentsLoaded])
    return null
  },
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

function historicalHead(text = "historical-head"): QueueItem {
  return {
    id: "head-1",
    draft: {
      blocks: [{ type: "text", text }],
      displayText: text,
    },
    modeId: null,
  }
}

function directDraft(text = "direct-now") {
  return {
    blocks: [{ type: "text" as const, text }],
    displayText: text,
  }
}

function armTerminalDisconnect() {
  for (const handler of surfaceH.acpEventHandlers) {
    handler(errorEvent(CONN, true))
  }
}

function resetSurfaceHarness() {
  lifecycleCapture.lastOptions = null
  lifecycleCapture.handleReconnect.mockClear()
  lifecycleCapture.handleSend.mockClear()
  surfaceH.conversations = []
  surfaceH.acpEventHandlers = []
  surfaceH.connStatus = null
  surfaceH.currentConnectionId = CONN
  surfaceH.lifecycleConnectionId = CONN
  surfaceH.queueItems = []
  surfaceH.dequeueCalls = 0
  surfaceH.shellProps = null
}

describe("ConversationSessionSurface useConnectionLifecycle options harness", () => {
  beforeEach(() => {
    resetSurfaceHarness()
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

/**
 * Timer race: a zero-delay flush already scheduled can fire after a terminal
 * disconnect event arms pause state but before the passive effect mirrors
 * pause into the ref. Arming must update the ref synchronously so the timer
 * cannot dequeue.
 *
 * Ordering is modeled inside a single `act()`: setState is scheduled but
 * React has not re-rendered / run passive effects until the act callback
 * returns. Firing timers inside that window still proves the sync-ref arm
 * without setState-outside-act warnings.
 */
describe("ConversationSessionSurface terminal pause timer race", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("sync-blocks an already-scheduled zero-delay flush before ref passive effect", () => {
    vi.useFakeTimers()
    try {
      surfaceH.conversations = [fullSummary(42, "in_progress")]
      surfaceH.connStatus = "connected"
      surfaceH.queueItems = [historicalHead()]
      surfaceH.dequeueCalls = 0

      act(() => {
        renderSurface(42)
      })
      // Auto-flush effect scheduled a zero-delay timer (no bounce backoff).
      expect(vi.getTimerCount()).toBeGreaterThan(0)

      act(() => {
        // Arm schedules pause setState; passive ref-mirror effect has not run
        // yet inside this act body. Sync ref arm must block the flush timer.
        armTerminalDisconnect()
        vi.runOnlyPendingTimers()
      })

      // Must not dequeue the historical head in the race window.
      expect(surfaceH.dequeueCalls).toBe(0)
      expect(surfaceH.queueItems.map((q) => q.id)).toEqual(["head-1"])
      expect(lifecycleCapture.handleSend).not.toHaveBeenCalled()
    } finally {
      vi.useRealTimers()
    }
  })
})

/**
 * Existing latch rebase race: arm at baseline X, advance store to newer
 * non-cancelled in_progress Y, deliver a terminal event for Y *before* the
 * summary-driven latch clear settles. Without re-arming in the terminal
 * updater (`prev ?? X` keeps X), the subsequent clear drops the latch
 * (Y > X) and auto-connect can reopen despite a terminal event for Y.
 */
describe("ConversationSessionSurface existing latch rebase race", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("re-arms latch at Y when terminal for Y arrives before clear effect settles", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    act(() => {
      renderSurface(42)
    })

    // First arm at X.
    act(() => {
      armTerminalDisconnect()
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.queuePaused).toBe(true)

    // Authoritative root advances to Y, then a terminal event for Y is
    // delivered *before* subscribers are notified (passive clear has not
    // settled). Updater must re-arm at Y if X would clear against Y.
    surfaceH.conversations = [fullSummary(42, "in_progress", NEWER)]
    act(() => {
      for (const handler of surfaceH.acpEventHandlers) {
        handler(errorEvent(CONN, true))
      }
    })

    // Settle clear via store notify (models delayed Zustand subscriber flush).
    // If baseline stayed X, clear drops the latch and re-enables auto-connect.
    // Re-arm at Y keeps the latch armed and the queue paused.
    expect(surfaceH.notifyWorkspace).toEqual(expect.any(Function))
    act(() => {
      surfaceH.notifyWorkspace?.()
    })

    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.queuePaused).toBe(true)
  })
})

/**
 * Bare disconnected status must arm lifecycle policy on the mounted surface
 * (production recognizes status_changed: disconnected; pure helper alone is
 * not enough coverage for the inline callback path).
 */
describe("ConversationSessionSurface bare disconnected integration", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("arms latch + queue pause on bare status_changed disconnected for current root", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)

    act(() => {
      for (const handler of surfaceH.acpEventHandlers) {
        handler(statusEvent(CONN, "disconnected"))
      }
    })

    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.queuePaused).toBe(true)
  })

  it("does not arm on bare disconnected for mismatched connection", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    act(() => {
      renderSurface(42)
    })

    act(() => {
      for (const handler of surfaceH.acpEventHandlers) {
        handler(statusEvent("other-conn", "disconnected"))
      }
    })

    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)
  })
})

/**
 * Store-patch-then-terminal-event race vs useAcpEvent's passive handler ref.
 *
 * Production installs the latest ACP handler only in a passive effect, so a
 * workspace root patch can land (and a terminal event can fire) while the
 * still-installed handler is the previous render's closure. Delivery must
 * read the authoritative root from the workspace store (`getState`) so:
 * - a newer cancelled row cannot arm / rebaseline the latch
 * - a newer in_progress row captures that newer updated_at (no instant clear)
 */
describe("ConversationSessionSurface patch-then-event delivery-time summary", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("captured old handler does not arm after a newer cancelled store patch", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    const view = renderSurface(42)
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)

    // Handlers registered after mount (= passive effect has installed them).
    const installed = [...surfaceH.acpEventHandlers]
    expect(installed.length).toBeGreaterThan(0)

    // Workspace cancelled patch lands before re-render / new handler install.
    surfaceH.conversations = [fullSummary(42, "cancelled", NEWER)]

    // Terminal event delivered to the still-installed (stale-closure) handler.
    act(() => {
      for (const handler of installed) {
        handler(errorEvent(CONN, true))
      }
    })

    // Restore a non-clearing in_progress at the old baseline and re-render.
    // If the stale handler armed with BASELINE, the latch would remain and
    // deny auto-connect. Delivery-time cancelled must leave latch unarmed.
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    act(() => {
      view.rerender(
        createElement(ConversationSessionSurface, {
          tabId: "tab-1",
          conversationId: 42,
          folderId: 1,
          agentType: "claude",
          workingDir: "/tmp/project",
          isActive: true,
          showActiveFlow: false,
          reloadSignal: 0,
        })
      )
    })

    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)
  })

  it("captured old handler baselines latch from newer in_progress store summary", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    const view = renderSurface(42)
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)

    const installed = [...surfaceH.acpEventHandlers]
    expect(installed.length).toBeGreaterThan(0)

    // Newer in_progress root arrives before the passive handler ref updates.
    surfaceH.conversations = [fullSummary(42, "in_progress", NEWER)]

    act(() => {
      for (const handler of installed) {
        handler(errorEvent(CONN, true))
      }
    })

    // Re-render with the same newer root. Stale baseline BASELINE would clear
    // immediately (NEWER > BASELINE) and re-enable auto-connect. Delivery-time
    // NEWER baseline must keep the latch armed.
    act(() => {
      view.rerender(
        createElement(ConversationSessionSurface, {
          tabId: "tab-1",
          conversationId: 42,
          folderId: 1,
          agentType: "claude",
          workingDir: "/tmp/project",
          isActive: true,
          showActiveFlow: false,
          reloadSignal: 0,
        })
      )
    })

    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.queuePaused).toBe(true)
  })
})

/**
 * Draft first-send bind vs useAcpEvent's passive handler ref.
 *
 * First send assigns `dbConvIdRef.current = newConversationId` synchronously,
 * then schedules `setCreatedConversationId` / tab bind. Until the passive
 * handler ref updates, an already-installed ACP callback still closes over
 * `dbConversationId === null`. Delivery must resolve the root id from the
 * sync-maintained ref and look up the authoritative store summary so a valid
 * same-connection terminal event still arms latch + queue pause.
 */
describe("ConversationSessionSurface draft-bind stale ACP handler", () => {
  const DRAFT_BOUND_ID = 99

  beforeEach(() => {
    resetSurfaceHarness()
    vi.mocked(createConversation).mockReset()
  })

  it("captured old draft handler arms latch after first-send sync bind", async () => {
    surfaceH.conversations = []
    surfaceH.connStatus = "connected"

    act(() => {
      renderSurface(null)
    })
    // Draft + usable agent reported by AgentSelector mock → auto-connect open.
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)

    // Capture handlers installed while dbConversationId is still null.
    const installedWhileUnbound = [...surfaceH.acpEventHandlers]
    expect(installedWhileUnbound.length).toBeGreaterThan(0)

    // First-send create resolves with a real DB id. Populate the authoritative
    // root summary so delivery-time store lookup succeeds after the sync ref
    // bind (refreshConversations is mocked and does not seed the store).
    vi.mocked(createConversation).mockImplementation(async () => {
      surfaceH.conversations = [
        fullSummary(DRAFT_BOUND_ID, "in_progress", BASELINE),
      ]
      return DRAFT_BOUND_ID
    })

    expect(surfaceH.shellProps?.onSend).toEqual(expect.any(Function))
    await act(async () => {
      surfaceH.shellProps!.onSend!(directDraft("first-send-bind"))
      // Flush the unbound create + sync dbConvIdRef assignment path.
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(createConversation).toHaveBeenCalled()
    expect(lifecycleCapture.handleSend).toHaveBeenCalled()

    // After bind, non-cancelled root would allow auto-connect unless latched.
    // Force a render so shell/lifecycle options reflect the bound summary
    // without replacing the captured unbound-era ACP closures.
    act(() => {
      // no-op: state from create already committed under the prior act
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)

    // Deliver a terminal event through the *old* unbound-era handlers only —
    // they still close over dbConversationId === null. Delivery must resolve
    // the id from dbConvIdRef (sync-written on first send) + store summary.
    act(() => {
      for (const handler of installedWhileUnbound) {
        handler(errorEvent(CONN, true))
      }
    })

    // Must arm latch + queue pause from the new in_progress root/baseline.
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.queuePaused).toBe(true)
  })
})

/**
 * Component-level wiring: capture real ConversationShell seams (not local
 * boolean simulations) for direct-send bypass, Resume Queue FIFO, and latch
 * independence from queue pause.
 */
/**
 * Connection A→B transition vs useAcpEvent's passive handler ref.
 *
 * Production installs the latest ACP handler only in a passive effect. During
 * a reconnect the connection store map already holds B when raw events for A
 * or B are delivered, but a still-installed handler may have closed over A's
 * `conn.connectionId`. Delivery must resolve the current bound id from
 * `connectionStore.getConnection(tabId)` so:
 * - a late terminal event for old A does not arm latch/pause after B is current
 * - a terminal event for current B still arms through that old handler
 *
 * These fail if the production predicate is restored to captured
 * `conn.connectionId` (lifecycle mock stays fixed at CONN / CONN_A).
 */
describe("ConversationSessionSurface connection-transition stale ACP handler", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("captured old handler does not arm on terminal for previous connection A after B is current", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    // Mount while A is bound: store + render-time lifecycle id both A.
    surfaceH.currentConnectionId = CONN_A
    surfaceH.lifecycleConnectionId = CONN_A
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)

    const installedWhileA = [...surfaceH.acpEventHandlers]
    expect(installedWhileA.length).toBeGreaterThan(0)

    // A→B: provider map updates before passive handler refresh / re-render.
    // lifecycleConnectionId stays A so a captured conn.connectionId would still
    // match late A events (RED proof if production uses the closure).
    surfaceH.currentConnectionId = CONN_B

    // Late terminal for old A delivered to still-installed (stale) handlers.
    act(() => {
      for (const handler of installedWhileA) {
        handler(errorEvent(CONN_A, true))
      }
    })

    // Must NOT arm — only current bound connection B may latch.
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)
  })

  it("captured old handler arms on terminal for current connection B before passive refresh", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    surfaceH.currentConnectionId = CONN_A
    surfaceH.lifecycleConnectionId = CONN_A
    act(() => {
      renderSurface(42)
    })
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)

    const installedWhileA = [...surfaceH.acpEventHandlers]
    expect(installedWhileA.length).toBeGreaterThan(0)

    // Transition to B without reinstalling handlers (passive-ref lag).
    // Captured conn.connectionId would still be A and reject B (RED proof).
    surfaceH.currentConnectionId = CONN_B

    act(() => {
      for (const handler of installedWhileA) {
        handler(errorEvent(CONN_B, true))
      }
    })

    // Delivery-time store lookup sees B → arm latch + queue pause.
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.queuePaused).toBe(true)
  })
})

describe("ConversationSessionSurface queue pause / Resume Queue wiring", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("direct send during terminal pause does not dequeue the historical head", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress")]
    surfaceH.connStatus = "connected"
    surfaceH.queueItems = [historicalHead()]

    act(() => {
      renderSurface(42)
    })
    act(() => {
      armTerminalDisconnect()
    })

    expect(surfaceH.shellProps?.queuePaused).toBe(true)
    expect(surfaceH.shellProps?.onSend).toEqual(expect.any(Function))

    const headIdBefore = surfaceH.queueItems[0]?.id
    act(() => {
      surfaceH.shellProps!.onSend!(directDraft("live-after-pause"))
    })

    // Historical head remains; direct send bypassed the queue (no dequeue).
    expect(surfaceH.dequeueCalls).toBe(0)
    expect(surfaceH.queueItems[0]?.id).toBe(headIdBefore)
    expect(surfaceH.queueItems).toHaveLength(1)
    // Real surface handleSend reached lifecycle send for the new prompt.
    expect(lifecycleCapture.handleSend).toHaveBeenCalled()
    const sentDraft = lifecycleCapture.handleSend.mock.calls[0]?.[0] as {
      displayText?: string
    }
    expect(sentDraft?.displayText).toBe("live-after-pause")
  })

  it("Resume Queue clears only the pause and drains the historical head FIFO", () => {
    vi.useFakeTimers()
    try {
      surfaceH.conversations = [fullSummary(42, "in_progress")]
      surfaceH.connStatus = "connected"
      surfaceH.queueItems = [
        historicalHead("first"),
        {
          id: "head-2",
          draft: {
            blocks: [{ type: "text", text: "second" }],
            displayText: "second",
          },
          modeId: null,
        },
      ]

      act(() => {
        renderSurface(42)
      })
      act(() => {
        armTerminalDisconnect()
      })

      expect(surfaceH.shellProps?.queuePaused).toBe(true)
      expect(surfaceH.shellProps?.onResumeQueue).toEqual(expect.any(Function))
      // Latched → auto-connect still denied before resume.
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)

      // No drain while paused (flush effect returns early).
      act(() => {
        vi.runOnlyPendingTimers()
      })
      expect(surfaceH.dequeueCalls).toBe(0)
      expect(surfaceH.queueItems.map((q) => q.id)).toEqual(["head-1", "head-2"])

      act(() => {
        surfaceH.shellProps!.onResumeQueue!()
      })
      // Pause cleared via real shell callback; reconnect latch stays armed.
      expect(surfaceH.shellProps?.queuePaused).toBe(false)
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)

      // After resume, auto-flush dequeues the historical head first (FIFO).
      act(() => {
        vi.runOnlyPendingTimers()
      })
      expect(surfaceH.dequeueCalls).toBe(1)
      expect(surfaceH.queueItems.map((q) => q.id)).toEqual(["head-2"])
      expect(lifecycleCapture.handleSend).toHaveBeenCalled()
      const flushed = lifecycleCapture.handleSend.mock.calls[0]?.[0] as {
        displayText?: string
      }
      // Historical head drained first (FIFO); remaining item stays queued.
      expect(flushed?.displayText).toBe("first")
    } finally {
      vi.useRealTimers()
    }
  })

  it("terminal reconnect latch stays independent of Resume Queue", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress")]
    surfaceH.connStatus = "disconnected"
    surfaceH.queueItems = [historicalHead()]

    act(() => {
      renderSurface(42)
    })
    act(() => {
      armTerminalDisconnect()
    })
    expect(surfaceH.shellProps?.queuePaused).toBe(true)
    expect(surfaceH.shellProps?.showReconnect).toBe(true)
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)

    act(() => {
      surfaceH.shellProps!.onResumeQueue!()
    })
    // Queue pause cleared only; latch / reconnect affordance remain.
    expect(surfaceH.shellProps?.queuePaused).toBe(false)
    expect(surfaceH.shellProps?.onResumeQueue).toBeUndefined()
    expect(surfaceH.shellProps?.showReconnect).toBe(true)
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)

    // Explicit Reconnect still only calls handleReconnect.
    act(() => {
      surfaceH.shellProps!.onReconnect!()
    })
    expect(lifecycleCapture.handleReconnect).toHaveBeenCalledTimes(1)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)
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
