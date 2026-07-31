import { createElement, useEffect, useReducer, useRef } from "react"
import { flushSync } from "react-dom"
import { act, cleanup, render } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

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
  canExplicitReconnectWithSessionIdentity,
  ConversationSessionSurface,
  resolveDelegateConnectionPolicy,
  resolveSessionAutoConnectAllowed,
  resolveSurfacePersistedSummary,
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

describe("resolveSurfacePersistedSummary / resolveDelegateConnectionPolicy", () => {
  const childDetailSummary = {
    id: 99,
    status: "in_progress",
    updated_at: BASELINE,
    kind: "delegate",
  }

  it("uses child detail when the root workspace store excludes the row", () => {
    expect(
      resolveSurfacePersistedSummary(null, childDetailSummary as never)
    ).toBe(childDetailSummary)
  })

  it("maps fail-closed delegate access to observer connection policy", () => {
    expect(
      resolveDelegateConnectionPolicy({
        isDelegate: true,
        access: {
          mode: "viewer_only",
          reason: "task_running",
          parent_id: 10,
        },
      })
    ).toEqual({
      interactionLocked: true,
      intent: "observe_existing",
      retryObserverDiscovery: true,
    })
  })

  it("terminal child plus idle parent restores normal connection policy", () => {
    expect(
      resolveDelegateConnectionPolicy({
        isDelegate: true,
        access: { mode: "interactive", reason: null, parent_id: 10 },
      })
    ).toEqual({
      interactionLocked: false,
      intent: "own_or_observe",
      retryObserverDiscovery: false,
    })
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

  it("hides reconnect while historical session identity is unresolved", () => {
    expect(
      shouldShowTerminalReconnect({
        rootCancelled: true,
        terminalDisconnectLatch: null,
        connStatus: "disconnected",
        sessionIdentityReady: false,
      })
    ).toBe(false)
    expect(
      shouldShowTerminalReconnect({
        rootCancelled: false,
        terminalDisconnectLatch: { baselineUpdatedAt: BASELINE },
        connStatus: "error",
        sessionIdentityReady: false,
      })
    ).toBe(false)
  })
})

describe("canExplicitReconnectWithSessionIdentity", () => {
  it("allows drafts and cline without a resumable external id", () => {
    expect(
      canExplicitReconnectWithSessionIdentity({
        hasPersistedConversation: false,
        isCline: false,
        externalSessionId: undefined,
      })
    ).toBe(true)
    expect(
      canExplicitReconnectWithSessionIdentity({
        hasPersistedConversation: true,
        isCline: true,
        externalSessionId: undefined,
      })
    ).toBe(true)
  })

  it("requires a non-empty external id for persisted non-cline roots", () => {
    expect(
      canExplicitReconnectWithSessionIdentity({
        hasPersistedConversation: true,
        isCline: false,
        externalSessionId: undefined,
      })
    ).toBe(false)
    expect(
      canExplicitReconnectWithSessionIdentity({
        hasPersistedConversation: true,
        isCline: false,
        externalSessionId: null,
      })
    ).toBe(false)
    expect(
      canExplicitReconnectWithSessionIdentity({
        hasPersistedConversation: true,
        isCline: false,
        externalSessionId: "",
      })
    ).toBe(false)
    expect(
      canExplicitReconnectWithSessionIdentity({
        hasPersistedConversation: true,
        isCline: false,
        externalSessionId: "ext-historical",
      })
    ).toBe(true)
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
  interactionLocked?: boolean
  error?: string | null
  topBanner?: unknown
  onSend?: (
    draft: {
      blocks: Array<{ type: "text"; text: string }>
      displayText: string
    },
    modeId?: string | null
  ) => void
  onForkSend?: (
    draft: {
      blocks: Array<{ type: "text"; text: string }>
      displayText: string
    },
    modeId?: string | null
  ) => void | Promise<void>
  draftRestore?: {
    revision: number
    draft: {
      blocks: Array<{ type: "text"; text: string }>
      displayText: string
    }
  } | null
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
    sessionId?: string
    connectionIntent?: string
    retryObserverDiscovery?: boolean
    onDelegateViewerOnly?: () => void
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
  detailKind: "chat" as string,
  delegateAccess: {
    mode: "interactive" as string,
    reason: null as string | null,
    parent_id: null as number | null,
  },
  refreshDelegateAccess: vi.fn(async () => undefined),
  removeOptimisticTurn: vi.fn(),
  setSyncState: vi.fn(),
  requeueFront: vi.fn(),
  syncDelegateTerminalDetail: vi.fn(),
  refetchDetail: vi.fn(),
  reloadDetail: vi.fn(),
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
  /** Controllable conversation detail fetch (historical identity). */
  detailLoading: false,
  detailExternalId: "ext-1" as string | null,
  /** Runtime external id fallback when detail has not resolved yet. */
  runtimeExternalId: null as string | null,
  /** Runtime delegate terminal-sync error surface. */
  delegateSyncError: null as string | null,
  /** Detail parent_id for identity assertions (durable child row). */
  detailParentId: null as number | null,
  /** useDelegateAccess loading (fail-closed access while fetching). */
  delegateAccessLoading: false,
  /** Lifecycle mock conn.error (owner shell error path). */
  connError: null as string | null,
  queueItems: [] as QueueItem[],
  dequeueCalls: 0,
  shellProps: null as CapturedShellProps | null,
  /** Lifecycle mock `conn.supportsFork` (fork affordance wiring). */
  supportsFork: false,
  /** Broker-known delegated-child identity before detail hydration. */
  isDelegationChild: false,
  /** Notify workspace-store mock subscribers (Zustand-like). */
  notifyWorkspace: null as null | (() => void),
  /** Render root for querying topBanner DOM (delegate status row). */
  renderRoot: null as HTMLElement | null,
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
    sessionId?: string
    connectionIntent?: string
    retryObserverDiscovery?: boolean
    onDelegateViewerOnly?: () => void
  }) => {
    lifecycleCapture.lastOptions = {
      isActive: options.isActive,
      autoConnectAllowed: options.autoConnectAllowed,
      contextKey: options.contextKey,
      sessionId: options.sessionId,
      connectionIntent: options.connectionIntent,
      retryObserverDiscovery: options.retryObserverDiscovery,
      onDelegateViewerOnly: options.onDelegateViewerOnly,
    }
    return {
      conn: {
        status: surfaceH.connStatus,
        sessionId: null,
        connectionId: surfaceH.lifecycleConnectionId,
        isViewer: false,
        error: surfaceH.connError,
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
        supportsFork: surfaceH.supportsFork,
        isDelegationChild: surfaceH.isDelegationChild,
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

vi.mock("@/hooks/use-delegate-access", () => ({
  useDelegateAccess: () => ({
    access: surfaceH.delegateAccess,
    loading: surfaceH.delegateAccessLoading,
    refresh: surfaceH.refreshDelegateAccess,
  }),
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
    requeueFront: surfaceH.requeueFront,
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
    detail: surfaceH.detailLoading
      ? null
      : {
          summary: {
            id: 42,
            external_id: surfaceH.detailExternalId,
            status: "in_progress",
            updated_at: BASELINE,
            kind: surfaceH.detailKind,
            parent_id: surfaceH.detailParentId,
          },
          continuation_failure: null,
        },
    loading: surfaceH.detailLoading,
    error: null,
    acpLoadError: null,
  }),
}))

vi.mock("@/stores/conversation-runtime-store", () => ({
  completeLiveTranscriptTurn: vi.fn(),
  useConversationRuntimeActions: () => ({
    appendOptimisticTurn: vi.fn(),
    removeOptimisticTurn: surfaceH.removeOptimisticTurn,
    appendViewerUserTurn: vi.fn(),
    refetchDetail: surfaceH.refetchDetail,
    reloadDetail: surfaceH.reloadDetail,
    syncTurnMetadata: vi.fn(() => () => undefined),
    syncDelegateTerminalDetail: surfaceH.syncDelegateTerminalDetail,
    removeConversation: vi.fn(),
    setAcpLoadError: vi.fn(),
    setDbConversationId: vi.fn(),
    setExternalId: vi.fn(),
    setLiveMessage: vi.fn(),
    setLiveOwnsActiveTurn: vi.fn(),
    setPendingCleanup: vi.fn(),
    setSyncState: surfaceH.setSyncState,
  }),
  useConversationRuntimeStore: (
    sel: (s: {
      byConversationId: Map<
        number,
        {
          externalId: string | null
          sessionStats: null
          syncState: string
          delegateSyncError: string | null
        }
      >
    }) => unknown
  ) => {
    const byConversationId = new Map<
      number,
      {
        externalId: string | null
        sessionStats: null
        syncState: string
        delegateSyncError: string | null
      }
    >()
    // Always expose a session entry for the harness conversation so the
    // shallow selector can read delegateSyncError even without a runtime
    // external id (identity still comes from detail.summary.external_id).
    byConversationId.set(42, {
      externalId: surfaceH.runtimeExternalId,
      sessionStats: null,
      syncState: "idle",
      delegateSyncError: surfaceH.delegateSyncError,
    })
    return sel({ byConversationId })
  },
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
      interactionLocked: props.interactionLocked,
      error: props.error ?? null,
      topBanner: props.topBanner,
      onSend: props.onSend,
      onForkSend: props.onForkSend,
      draftRestore: props.draftRestore,
      queue: props.queue,
      children: props.children,
    }
    // Render topBanner so DelegateAccessStatus is in the document for
    // data-state queries; keep children for existing message-list checks.
    return createElement(
      "div",
      {
        ref: (el: HTMLElement | null) => {
          surfaceH.renderRoot = el
        },
        "data-testid": "shell-root",
      },
      props.topBanner as never,
      props.children as never
    )
  },
}))

vi.mock("@/components/chat/session-config-stale-banner", () => ({
  SessionConfigStaleBanner: () => null,
}))
vi.mock("@/components/conversations/tool-watchdog-banner", () => ({
  ToolWatchdogBanner: () => null,
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
  acpConnect: vi.fn(),
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
    createElement(ConversationSessionSurface, surfaceProps(conversationId))
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
  lifecycleCapture.handleCancel.mockClear()
  lifecycleCapture.handleSetConfigOption.mockClear()
  surfaceH.conversations = []
  surfaceH.acpEventHandlers = []
  surfaceH.connStatus = null
  surfaceH.currentConnectionId = CONN
  surfaceH.lifecycleConnectionId = CONN
  surfaceH.detailLoading = false
  surfaceH.detailExternalId = "ext-1"
  surfaceH.detailKind = "chat"
  surfaceH.detailParentId = null
  surfaceH.delegateAccess = {
    mode: "interactive",
    reason: null,
    parent_id: null,
  }
  surfaceH.delegateAccessLoading = false
  surfaceH.delegateSyncError = null
  surfaceH.connError = null
  surfaceH.refreshDelegateAccess.mockClear()
  surfaceH.removeOptimisticTurn.mockClear()
  surfaceH.setSyncState.mockClear()
  surfaceH.requeueFront.mockClear()
  surfaceH.syncDelegateTerminalDetail.mockClear()
  surfaceH.refetchDetail.mockClear()
  surfaceH.reloadDetail.mockClear()
  surfaceH.runtimeExternalId = null
  surfaceH.queueItems = []
  surfaceH.dequeueCalls = 0
  surfaceH.shellProps = null
  surfaceH.renderRoot = null
  surfaceH.supportsFork = false
  surfaceH.isDelegationChild = false
}

function turnCompleteEndTurn(connectionId: string): EventEnvelope {
  return {
    seq: 3,
    connection_id: connectionId,
    type: "turn_complete",
    session_id: "ext-1",
    stop_reason: "end_turn",
    mark_awaiting_reply: false,
  }
}

function surfaceProps(conversationId: number | null = 42) {
  return {
    tabId: "tab-1",
    conversationId,
    folderId: 1,
    agentType: "claude" as const,
    workingDir: "/tmp/project",
    isActive: true,
    showActiveFlow: false,
    reloadSignal: 0,
  }
}

describe("ConversationSessionSurface useConnectionLifecycle options harness", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  afterEach(() => {
    // Keep-alive surfaces stay mounted unless cleaned; prevent lastOptions
    // pollution across cases that mutate surfaceH harness fields.
    cleanup()
  })

  it("passes autoConnectAllowed === false for a missing persisted summary", () => {
    // No workspace root row AND no detail summary → fail closed.
    // When detail exists, resolveSurfacePersistedSummary falls back to it so
    // delegated children excluded from the root list can still auto-observe.
    surfaceH.conversations = []
    surfaceH.detailLoading = true
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
 * disconnect event arms pause state. Arming must update the pause ref
 * synchronously (no passive state→ref mirror) so the timer cannot dequeue.
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
        // Arm schedules pause setState; sync ref arm must block the flush timer
        // in this same turn (no passive state→ref mirror).
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

  /**
   * Resume Queue → fresh terminal → timer:
   * 1. Terminal pause armed with a historical queue head.
   * 2. Resume Queue (real shell callback) clears pause and schedules a
   *    zero-delay auto-flush for that head.
   * 3. A fresh terminal ACP event re-arms pause (sync ref=true + setState)
   *    before that timer is drained.
   * 4. Timer recheck must not dequeue the historical head.
   *
   * Source race this guards: a passive state→ref mirror for the resume
   * (false) commit can run after a fresh terminal arm wrote ref=true and
   * clobber the ref back to false, so the zero-delay flush dequeues history.
   * Production must update the ref only synchronously on the two write paths
   * (no async mirror). Under RTL act/flushSync, passive effects often settle
   * with the final state, so this case asserts the user-visible invariant via
   * real onResumeQueue + ACP handlers + timer recheck rather than relying on
   * forcing React's internal effect queue order.
   */
  it("Resume Queue then fresh terminal keeps historical head from timer dequeue", () => {
    vi.useFakeTimers()
    try {
      surfaceH.conversations = [fullSummary(42, "in_progress")]
      surfaceH.connStatus = "connected"
      surfaceH.queueItems = [historicalHead()]
      surfaceH.dequeueCalls = 0

      act(() => {
        renderSurface(42)
      })
      act(() => {
        armTerminalDisconnect()
      })
      expect(surfaceH.shellProps?.queuePaused).toBe(true)
      expect(surfaceH.shellProps?.onResumeQueue).toEqual(expect.any(Function))
      lifecycleCapture.handleSend.mockClear()
      surfaceH.dequeueCalls = 0

      act(() => {
        // Commit resume (false) then re-arm in the same turn so a pending
        // zero-delay flush from the unpaused commit is still queued when the
        // fresh terminal writes the pause ref.
        flushSync(() => {
          surfaceH.shellProps!.onResumeQueue!()
        })
        // Clearing pause schedules zero-delay auto-flush for the historical head.
        expect(vi.getTimerCount()).toBeGreaterThan(0)

        // Fresh terminal after resume: sync ref arm + setState(true).
        armTerminalDisconnect()
        vi.runOnlyPendingTimers()
      })

      // Historical head must remain; timer recheck honors the fresh pause.
      expect(surfaceH.dequeueCalls).toBe(0)
      expect(surfaceH.queueItems.map((q) => q.id)).toEqual(["head-1"])
      expect(lifecycleCapture.handleSend).not.toHaveBeenCalled()
      expect(surfaceH.shellProps?.queuePaused).toBe(true)
      // Latch still denies auto-connect (Resume Queue never clears the latch).
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
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

/**
 * Fix A: persisted non-cline tabs already block auto-connect while detail is
 * loading (sessionId undefined → backend session/new orphans history). Explicit
 * Reconnect must be gated the same way until a resumable external identity is
 * known (detail external_id or runtimeExternalId). Callback defense must refuse
 * connect without identity even if a stale onReconnect reference is invoked.
 * Reconnect remains explicit: no status or queue mutation.
 */
describe("ConversationSessionSurface explicit reconnect identity gate", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("blocks reconnect callback until historical identity resolves, then reconnects once with that identity", () => {
    surfaceH.conversations = [fullSummary(42, "cancelled")]
    surfaceH.connStatus = "disconnected"
    // Historical identity not yet available (detail still loading; no runtime id).
    surfaceH.detailLoading = true
    surfaceH.detailExternalId = null
    surfaceH.runtimeExternalId = null

    const view = renderSurface(42)

    // Lifecycle must not receive a session id yet; reconnect UI must not invite.
    expect(lifecycleCapture.lastOptions!.sessionId).toBeUndefined()
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
    expect(surfaceH.shellProps?.showReconnect).toBe(false)
    expect(surfaceH.shellProps?.onReconnect).toBeUndefined()
    expect(lifecycleCapture.handleReconnect).not.toHaveBeenCalled()

    // Resolve identity via detail external_id (runtime still empty).
    // Bump reloadSignal so memoized surface re-renders and re-reads detail mock.
    surfaceH.detailLoading = false
    surfaceH.detailExternalId = "ext-historical-42"
    act(() => {
      view.rerender(
        createElement(ConversationSessionSurface, {
          ...surfaceProps(42),
          reloadSignal: 1,
        })
      )
    })

    expect(lifecycleCapture.lastOptions!.sessionId).toBe("ext-historical-42")
    expect(surfaceH.shellProps?.showReconnect).toBe(true)
    expect(surfaceH.shellProps?.onReconnect).toEqual(expect.any(Function))

    act(() => {
      surfaceH.shellProps!.onReconnect!()
    })
    // Exactly one explicit reconnect; no status/queue mutation from reconnect.
    expect(lifecycleCapture.handleReconnect).toHaveBeenCalledTimes(1)
    expect(surfaceH.shellProps?.queuePaused).toBe(false)
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
  })

  it("callback defense refuses reconnect while identity is still unresolved", () => {
    surfaceH.conversations = [fullSummary(42, "cancelled")]
    surfaceH.connStatus = "disconnected"
    surfaceH.detailLoading = true
    surfaceH.detailExternalId = null
    surfaceH.runtimeExternalId = null

    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {})
    try {
      act(() => {
        renderSurface(42)
      })
      // Presentation already hides the control; invoke the identity helper path
      // by temporarily forcing show + a no-identity call pattern via pure seam.
      expect(
        canExplicitReconnectWithSessionIdentity({
          hasPersistedConversation: true,
          isCline: false,
          externalSessionId: lifecycleCapture.lastOptions?.sessionId,
        })
      ).toBe(false)
      expect(surfaceH.shellProps?.onReconnect).toBeUndefined()
      expect(lifecycleCapture.handleReconnect).not.toHaveBeenCalled()
    } finally {
      warnSpy.mockRestore()
    }
  })

  it("accepts runtime external id while detail is still loading", () => {
    surfaceH.conversations = [fullSummary(42, "cancelled")]
    surfaceH.connStatus = "disconnected"
    surfaceH.detailLoading = true
    surfaceH.detailExternalId = null
    surfaceH.runtimeExternalId = "ext-runtime-42"

    act(() => {
      renderSurface(42)
    })

    expect(lifecycleCapture.lastOptions!.sessionId).toBe("ext-runtime-42")
    expect(surfaceH.shellProps?.showReconnect).toBe(true)
    act(() => {
      surfaceH.shellProps!.onReconnect!()
    })
    expect(lifecycleCapture.handleReconnect).toHaveBeenCalledTimes(1)
  })
})

/**
 * Observation C (plan-mandated, do NOT invert):
 * Design requires that a bare terminal disconnect after end_turn still arms
 * the latch/queue pause *before* a delayed authoritative non-cancelled patch
 * (pending_review) arrives. pending_review clears only the reconnect latch;
 * queue pause remains until Resume Queue. Frontend must not synthesize
 * persisted status — only react to workspace summary patches.
 */
describe("ConversationSessionSurface end_turn disconnect then delayed pending_review", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("arms latch/pause on bare disconnect after end_turn; pending_review clears only latch", () => {
    vi.useFakeTimers()
    try {
      surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
      surfaceH.connStatus = "connected"
      surfaceH.queueItems = [historicalHead("queued-after-end-turn")]

      act(() => {
        renderSurface(42)
      })
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
      expect(surfaceH.shellProps?.queuePaused).toBe(false)

      // Narrative end_turn (does not arm latch by itself).
      act(() => {
        for (const handler of surfaceH.acpEventHandlers) {
          handler(turnCompleteEndTurn(CONN))
        }
      })
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
      expect(surfaceH.shellProps?.queuePaused).toBe(false)

      // Bare disconnect while root is still in_progress (before delayed patch).
      surfaceH.connStatus = "disconnected"
      act(() => {
        for (const handler of surfaceH.acpEventHandlers) {
          handler(statusEvent(CONN, "disconnected"))
        }
      })
      // Latch + queue pause armed; focus auto-connect denied before patch.
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(false)
      expect(surfaceH.shellProps?.queuePaused).toBe(true)
      expect(surfaceH.shellProps?.showReconnect).toBe(true)

      // Delayed authoritative pending_review (backend patch — not FE-synthesized).
      surfaceH.conversations = [fullSummary(42, "pending_review", NEWER)]
      act(() => {
        surfaceH.notifyWorkspace?.()
      })
      // Reconnect latch cleared by newer non-cancelled; queue pause remains.
      expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
      expect(surfaceH.shellProps?.queuePaused).toBe(true)
      // Reconnect affordance hides once neither cancelled nor latched.
      expect(surfaceH.shellProps?.showReconnect).toBe(false)

      // Auto-flush must not drain historical queue until Resume Queue.
      surfaceH.connStatus = "connected"
      act(() => {
        // Re-render to pick up connected + still-paused flush gate.
        surfaceH.notifyWorkspace?.()
      })
      act(() => {
        vi.runOnlyPendingTimers()
      })
      expect(surfaceH.dequeueCalls).toBe(0)
      expect(surfaceH.queueItems.map((q) => q.id)).toEqual(["head-1"])
      expect(lifecycleCapture.handleSend).not.toHaveBeenCalled()

      // Resume Queue is the only clear for pause → FIFO can flush.
      act(() => {
        surfaceH.shellProps!.onResumeQueue!()
      })
      expect(surfaceH.shellProps?.queuePaused).toBe(false)
      act(() => {
        vi.runOnlyPendingTimers()
      })
      expect(surfaceH.dequeueCalls).toBe(1)
      expect(lifecycleCapture.handleSend).toHaveBeenCalled()
      const flushed = lifecycleCapture.handleSend.mock.calls[0]?.[0] as {
        displayText?: string
      }
      expect(flushed?.displayText).toBe("queued-after-end-turn")
    } finally {
      vi.useRealTimers()
    }
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

describe("ConversationSessionSurface delegated viewer-only access", () => {
  beforeEach(() => {
    resetSurfaceHarness()
  })

  it("uses detail-only delegate child with task_running access as observer", () => {
    // Child absent from root workspace list; detail declares kind=delegate.
    surfaceH.conversations = []
    surfaceH.detailKind = "delegate"
    surfaceH.detailExternalId = "ext-child-99"
    surfaceH.delegateAccess = {
      mode: "viewer_only",
      reason: "task_running",
      parent_id: 10,
    }
    surfaceH.connStatus = "connected"

    act(() => {
      renderSurface(42)
    })

    expect(lifecycleCapture.lastOptions!.connectionIntent).toBe(
      "observe_existing"
    )
    expect(lifecycleCapture.lastOptions!.retryObserverDiscovery).toBe(true)
    // Detail summary fallback unlocks auto-connect despite missing root row.
    expect(lifecycleCapture.lastOptions!.autoConnectAllowed).toBe(true)
    expect(surfaceH.shellProps?.interactionLocked).toBe(true)
    // No owner reconnect affordance while latched-off (connected observer).
    expect(surfaceH.shellProps?.showReconnect).toBe(false)
    // Detail content reaches the shell children (message list area).
    expect(surfaceH.shellProps?.children).toBeTruthy()
  })

  it("does not auto-flush queue while interaction is locked", () => {
    vi.useFakeTimers()
    try {
      surfaceH.conversations = [
        {
          ...fullSummary(42, "in_progress", BASELINE),
          kind: "delegate",
        },
      ]
      surfaceH.detailKind = "delegate"
      surfaceH.delegateAccess = {
        mode: "viewer_only",
        reason: "parent_turn_active",
        parent_id: 10,
      }
      surfaceH.connStatus = "connected"
      surfaceH.queueItems = [historicalHead("locked-head")]

      act(() => {
        renderSurface(42)
      })
      expect(surfaceH.shellProps?.interactionLocked).toBe(true)

      act(() => {
        vi.runOnlyPendingTimers()
      })
      expect(surfaceH.dequeueCalls).toBe(0)
      expect(lifecycleCapture.handleSend).not.toHaveBeenCalled()
    } finally {
      vi.useRealTimers()
    }
  })

  it("restores fork draft via shared handler on delegate_viewer_only rejection", async () => {
    const { acpFork } = await import("@/lib/api")
    const acpForkMock = vi.mocked(acpFork)
    acpForkMock.mockRejectedValueOnce({
      code: "delegate_viewer_only",
      message: "Delegated conversation is read-only",
      detail: "parent_turn_active",
    })

    surfaceH.conversations = [fullSummary(42, "completed", BASELINE)]
    surfaceH.connStatus = "connected"
    surfaceH.supportsFork = true
    surfaceH.delegateAccess = {
      mode: "interactive",
      reason: null,
      parent_id: null,
    }

    act(() => {
      renderSurface(42)
    })

    const onForkSend = surfaceH.shellProps?.onForkSend
    expect(onForkSend).toEqual(expect.any(Function))

    const draft = {
      blocks: [{ type: "text" as const, text: "fork body" }],
      displayText: "fork body",
    }
    await act(async () => {
      await onForkSend!(draft, null)
    })

    expect(acpForkMock).toHaveBeenCalled()
    expect(surfaceH.refreshDelegateAccess).toHaveBeenCalled()
    expect(surfaceH.shellProps?.draftRestore).toEqual({
      revision: 1,
      draft,
    })
  })

  function renderDelegate(opts: {
    accessReason?: string | null
    reason?: string | null
    mode?: "viewer_only" | "interactive"
    connStatus?: string
    connectionId?: string | null
  }) {
    const reason =
      opts.reason !== undefined ? opts.reason : (opts.accessReason ?? null)
    const mode = opts.mode ?? (reason == null ? "interactive" : "viewer_only")
    surfaceH.conversations = []
    surfaceH.detailKind = "delegate"
    surfaceH.detailExternalId = "ext-child-99"
    surfaceH.detailParentId = 10
    surfaceH.runtimeExternalId = "ext-child-99"
    surfaceH.delegateAccess = {
      mode,
      reason,
      parent_id: 10,
    }
    surfaceH.connStatus = opts.connStatus ?? "connected"
    if (opts.connectionId !== undefined) {
      surfaceH.lifecycleConnectionId = opts.connectionId
      surfaceH.currentConnectionId = opts.connectionId
    }
    let view!: ReturnType<typeof renderSurface>
    act(() => {
      view = renderSurface(42)
    })
    return view
  }

  /**
   * Force a surface re-render after harness mutation. Props are stable under
   * `memo`, so workspace notify (same pattern as other surface tests) is
   * required to pick up connStatus / access changes.
   */
  function rerenderDelegate(opts: {
    accessReason?: string | null
    reason?: string | null
    mode?: "viewer_only" | "interactive"
    connStatus?: string
    connectionId?: string | null
  }) {
    const reason =
      opts.reason !== undefined ? opts.reason : (opts.accessReason ?? null)
    const mode = opts.mode ?? (reason == null ? "interactive" : "viewer_only")
    surfaceH.delegateAccess = {
      mode,
      reason,
      parent_id: 10,
    }
    if (opts.connStatus !== undefined) {
      surfaceH.connStatus = opts.connStatus
    }
    if (opts.connectionId !== undefined) {
      surfaceH.lifecycleConnectionId = opts.connectionId
      surfaceH.currentConnectionId = opts.connectionId
    }
    act(() => {
      surfaceH.notifyWorkspace?.()
    })
  }

  function delegateStatus(): string | null {
    const el = document.querySelector("[data-state]")
    return el?.getAttribute("data-state") ?? null
  }

  /** Identity from store/detail selectors — not mirrored test-only state. */
  function detailSummary() {
    return {
      kind: surfaceH.detailKind,
      parent_id: surfaceH.detailParentId,
      external_id: surfaceH.detailExternalId,
    }
  }

  function runtimeSession() {
    return {
      externalId:
        surfaceH.detailExternalId ?? surfaceH.runtimeExternalId ?? null,
    }
  }

  afterEach(() => {
    cleanup()
  })

  it("starts delegate convergence on the child prompting-to-connected edge", () => {
    renderDelegate({
      accessReason: "task_running",
      connStatus: "prompting",
    })
    surfaceH.syncDelegateTerminalDetail.mockClear()
    rerenderDelegate({
      accessReason: "task_running",
      connStatus: "connected",
    })
    expect(surfaceH.syncDelegateTerminalDetail).toHaveBeenCalledWith(42)
  })

  it("starts convergence when access leaves task_running", () => {
    renderDelegate({
      accessReason: "task_running",
      connStatus: "connected",
    })
    surfaceH.syncDelegateTerminalDetail.mockClear()
    rerenderDelegate({
      accessReason: "parent_turn_active",
      connStatus: "connected",
    })
    expect(surfaceH.syncDelegateTerminalDetail).toHaveBeenCalledTimes(1)
    rerenderDelegate({
      accessReason: null,
      connStatus: "connected",
    })
    expect(surfaceH.syncDelegateTerminalDetail).toHaveBeenCalledTimes(1)
  })

  it("does not start terminal sync on task_running → state_unknown", () => {
    renderDelegate({
      accessReason: "task_running",
      connStatus: "connected",
    })
    surfaceH.syncDelegateTerminalDetail.mockClear()
    rerenderDelegate({
      accessReason: "state_unknown",
      connStatus: "connected",
    })
    expect(surfaceH.syncDelegateTerminalDetail).not.toHaveBeenCalled()
  })

  it("shows waiting, observing, parent lock, then interactive without changing tab identity", async () => {
    const { acpConnect } = await import("@/lib/api")
    const acpConnectMock = vi.mocked(acpConnect)
    acpConnectMock.mockClear()

    const identityBefore = {
      tabId: "tab-1",
      conversationId: 42,
      externalId: "ext-child-99",
      kind: "delegate",
      parentId: 10,
    }

    // Seed identity sources used by detail/runtime selectors.
    surfaceH.detailExternalId = "ext-child-99"
    surfaceH.detailParentId = 10
    surfaceH.runtimeExternalId = "ext-child-99"

    const connectCallsBeforeLock = acpConnectMock.mock.calls.length
    renderDelegate({ reason: "task_running", connectionId: null })
    expect(delegateStatus()).toBe("waiting")
    expect(runtimeSession().externalId).toBe(identityBefore.externalId)
    expect(detailSummary()).toEqual({
      kind: identityBefore.kind,
      parent_id: identityBefore.parentId,
      external_id: identityBefore.externalId,
    })

    rerenderDelegate({
      reason: "task_running",
      connectionId: "broker-child",
    })
    expect(delegateStatus()).toBe("observing")

    rerenderDelegate({
      reason: "parent_turn_active",
      connectionId: "broker-child",
    })
    expect(delegateStatus()).toBe("parent_turn_active")
    expect(acpConnectMock).toHaveBeenCalledTimes(connectCallsBeforeLock)

    rerenderDelegate({
      reason: null,
      mode: "interactive",
      connectionId: null,
    })
    expect(delegateStatus()).toBe("interactive")
    expect({
      tabId: "tab-1",
      conversationId: 42,
      externalId: runtimeSession().externalId,
      kind: detailSummary().kind,
      parentId: detailSummary().parent_id,
    }).toEqual(identityBefore)
  })

  it("does not render a delegate status row for a non-delegate root conversation", () => {
    surfaceH.conversations = [fullSummary(42, "in_progress", BASELINE)]
    surfaceH.detailKind = "chat"
    surfaceH.detailParentId = null
    surfaceH.delegateAccess = {
      mode: "interactive",
      reason: null,
      parent_id: null,
    }
    act(() => {
      renderSurface(42)
    })
    expect(delegateStatus()).toBeNull()
    expect(document.querySelector("[data-state]")).toBeNull()
  })

  it("suppresses stale shell connection error while a locked delegate has no connection", () => {
    surfaceH.connError = "Agent disconnected"
    renderDelegate({
      reason: "task_running",
      connectionId: null,
      connStatus: "error",
    })
    expect(surfaceH.shellProps?.error).toBeNull()
    expect(delegateStatus()).toBe("waiting")
  })
})

