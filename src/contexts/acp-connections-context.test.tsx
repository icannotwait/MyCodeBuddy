import { useEffect, type ReactNode } from "react"
import { act, render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  AcpConnectionsProvider,
  useAcpActions,
  useConnectionStore,
  isRetryableObserverDiscoveryError,
  isValidConversationConnectionInfo,
  __getPublishedConnectionMapsCount,
  __resetPublishedConnectionMapsCount,
  __resetStreamingConfigForProviderTests,
  __connectionsReducerForTests,
  __resetWritableConnectionsCloneCount,
  __getWritableConnectionsCloneCount,
} from "@/contexts/acp-connections-context"
import type {
  DbConversationDetail,
  DbConversationSummary,
  DesktopAcpEventBatch,
  DesktopDeliveryFailure,
} from "@/lib/types"
import { parsePermissionToolCall } from "@/lib/permission-request"
import { subscribeDesktopAcpEvents } from "@/lib/transport/desktop-acp-events"
import { saveConfigPreference } from "@/lib/selector-prefs-storage"
import { useConnection } from "@/hooks/use-connection"
import {
  getPublishedRequestUsage,
  subscribeRequestUsage,
} from "@/lib/request-usage-live"
import { EMPTY_REQUEST_USAGE } from "@/lib/request-usage-speed"
import type { SnapshotPatch } from "@/lib/snapshot-denormalize"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import type { AttachHandlers } from "@/lib/transport/types"
import type {
  AcpEventMetricsSnapshot,
  EventEnvelope,
  EventBusMetricsSnapshot,
  LiveSessionSnapshot,
  SessionConfigOptionInfo,
} from "@/lib/types"

// Shared spies + a stub EventStream. `vi.hoisted` runs before the mock
// factories so they can close over this state. Mocking `getEventStream` to a
// non-null stub forces the "web / attach" transport path: the mount listener
// effect sets `listenerReadyRef` synchronously (so `waitForListenerReady` is a
// no-op) and `connectAsViewer` / the owner spawn both route through
// `stream.attach`.
const h = vi.hoisted(() => {
  const attach = vi.fn(() => ({ detach: vi.fn() }))
  const stream = { attach }
  const rafQueue: FrameRequestCallback[] = []
  const subscribeHandlers = new Map<string, (payload: unknown) => void>()
  const state: {
    onBatch: ((batch: DesktopAcpEventBatch) => void) | null
    onFailure: ((failure: DesktopDeliveryFailure) => void) | null
  } = { onBatch: null, onFailure: null }
  return {
    attach,
    stream,
    // getEventStream() returns this — default the web/attach stub; set to null
    // per-test to exercise the desktop firehose path.
    eventStreamValue: stream as { attach: typeof attach } | null,
    actions: null as unknown as ReturnType<typeof useAcpActions> | null,
    store: null as unknown as ReturnType<typeof useConnectionStore> | null,
    // api spies
    acpGetAgentStatus: vi.fn(),
    acpFindConnectionForConversation: vi.fn(),
    acpConnect: vi.fn(),
    acpConnectOrAttach: vi.fn(),
    acpDisconnect: vi.fn(),
    acpReleaseLease: vi.fn(),
    acpTerminateSharedSession: vi.fn(),
    acpCancel: vi.fn(),
    acpCancelQueuedPrompt: vi.fn(),
    acpGetSessionSnapshot: vi.fn(),
    acpGetDesktopDeliveryCapabilities: vi.fn(),
    buildDelegationSeedEnvelopes: vi.fn(() => []),
    denormalizeSnapshot: vi.fn(),
    pushAlert: vi.fn(),
    recordFrontendTurnTrace: vi.fn(),
    isDesktop: true,
    subscribeHandlers,
    subscribe: vi.fn(
      async (event: string, handler: (payload: unknown) => void) => {
        subscribeHandlers.set(event, handler)
        return () => {
          subscribeHandlers.delete(event)
        }
      }
    ),
    rafQueue,
    desktopBatchHandler: null as ((batch: DesktopAcpEventBatch) => void) | null,
    desktopFailureHandler: null as
      | ((failure: DesktopDeliveryFailure) => void)
      | null,
    desktopUnsubscribe: vi.fn(),
    setDesktopHandlers(
      onBatch: (batch: DesktopAcpEventBatch) => void,
      onFailure: (failure: DesktopDeliveryFailure) => void
    ) {
      state.onBatch = onBatch
      state.onFailure = onFailure
    },
    emitDesktopBatch(batch: DesktopAcpEventBatch) {
      state.onBatch?.(batch)
    },
    emitDesktopFailure(failure: DesktopDeliveryFailure) {
      state.onFailure?.(failure)
    },
    runAnimationFrame() {
      const queued = rafQueue.splice(0, rafQueue.length)
      for (const cb of queued) cb(16)
    },
    publishedConnectionMaps: () => __getPublishedConnectionMapsCount(),
    reconnectListeners: new Set<() => void>(),
    fireReconnect() {
      for (const cb of this.reconnectListeners) cb()
    },
    subscribeRaw(handler: (event: EventEnvelope) => void) {
      // Registered via useAcpEvent after mount — tests call actions path.
      // Provider exposes subscribers only through the hook; for raw tests we
      // use a lightweight wrapper registered in the describe block.
      void handler
    },
    // Stable across renders so tests can assert on what the error handler
    // routes to the status-bar alert vs. to the OS notification.
    sendSystemNotification: vi.fn(async () => undefined),
    toastWarning: vi.fn(),
  }
})

vi.mock("next-intl", () => ({
  // Emulate next-intl resolving a real message (never identity-returns the
  // key) so toLocalizedErrorMessage accepts structured i18n_key payloads.
  // Existing tests match on the key substring.
  useTranslations:
    () => (key: string, params?: Record<string, string | number>) => {
      if (params && Object.keys(params).length > 0) {
        const rendered = Object.entries(params)
          .map(([k, v]) => `${k}=${v}`)
          .join(",")
        return `${key}(${rendered})`
      }
      return `§${key}`
    },
}))

vi.mock("@/lib/platform", () => ({
  subscribe: h.subscribe,
  getEventStream: () => h.eventStreamValue,
}))

vi.mock("@/lib/delegation-seed", () => ({
  buildDelegationSeedEnvelopes: h.buildDelegationSeedEnvelopes,
}))

vi.mock("@/contexts/alert-context", () => ({
  useAlertContext: () => ({ pushAlert: h.pushAlert }),
}))

vi.mock("@/lib/acp/frontend-turn-trace", () => ({
  recordFrontendTurnTrace: (...args: unknown[]) =>
    h.recordFrontendTurnTrace(...args),
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({ activeFolder: { path: "/tmp/x", name: "x" } }),
}))

vi.mock("@/lib/notification", () => ({
  sendSystemNotification: h.sendSystemNotification,
}))

vi.mock("sonner", () => ({
  toast: { warning: h.toastWarning },
}))

vi.mock("@/lib/selector-prefs-storage", () => ({
  getSavedPrefsForConnect: () => ({ modeId: undefined, configValues: {} }),
  saveModePreference: vi.fn(),
  saveConfigPreference: vi.fn(),
}))

vi.mock("@/lib/snapshot-denormalize", () => ({
  denormalizeSnapshot: h.denormalizeSnapshot,
}))

const acpPromptMock = vi.hoisted(() => vi.fn())
const acpAnswerQuestionMock = vi.hoisted(() => vi.fn())
const acpAnswerPlanApprovalMock = vi.hoisted(() => vi.fn())
const acpSetModeMock = vi.hoisted(() => vi.fn())
const acpSetConfigOptionMock = vi.hoisted(() => vi.fn())
const acpGoalControlMock = vi.hoisted(() => vi.fn())
const acpRespondPermissionMock = vi.hoisted(() => vi.fn())
const acpTouchConnectionMock = vi.hoisted(() => vi.fn())

vi.mock("@/lib/api", () => ({
  acpGetAgentStatus: h.acpGetAgentStatus,
  acpFindConnectionForConversation: h.acpFindConnectionForConversation,
  acpConnect: h.acpConnect,
  acpConnectOrAttach: h.acpConnectOrAttach,
  acpDisconnect: h.acpDisconnect,
  acpReleaseLease: h.acpReleaseLease,
  acpTerminateSharedSession: h.acpTerminateSharedSession,
  acpGetSessionSnapshot: h.acpGetSessionSnapshot,
  acpGetDesktopDeliveryCapabilities: h.acpGetDesktopDeliveryCapabilities,
  acpPrompt: acpPromptMock,
  acpAnswerQuestion: acpAnswerQuestionMock,
  acpAnswerPlanApproval: acpAnswerPlanApprovalMock,
  acpSetMode: acpSetModeMock,
  acpSetConfigOption: acpSetConfigOptionMock,
  acpGoalControl: acpGoalControlMock,
  acpCancel: h.acpCancel,
  acpCancelQueuedPrompt: h.acpCancelQueuedPrompt,
  acpRespondPermission: acpRespondPermissionMock,
  acpTouchConnection: acpTouchConnectionMock,
  // Imported by the conversation runtime store (a real dependency of the
  // provider via the background-activity bridge). The settled path no longer
  // refetches (it flips the launch card in-memory); reject any stray call so a
  // regression that reintroduces a settle-triggered refetch fails loudly.
  getFolderConversation: vi.fn(async () => {
    throw new Error("detail not seeded in this suite")
  }),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({
    isDesktop: () => h.isDesktop,
    subscribe: h.subscribe,
    call: vi.fn(),
    onReconnect: (callback: () => void) => {
      h.reconnectListeners.add(callback)
      return () => {
        h.reconnectListeners.delete(callback)
      }
    },
  }),
  isRemoteDesktopMode: () => false,
}))

vi.mock("@/lib/transport/desktop-acp-events", () => ({
  subscribeDesktopAcpEvents: vi.fn(
    async (
      _caps: unknown,
      handlers: {
        onBatch: (batch: DesktopAcpEventBatch) => void
        onFailure: (failure: DesktopDeliveryFailure) => void
      }
    ) => {
      h.setDesktopHandlers(handlers.onBatch, handlers.onFailure)
      return () => {
        h.desktopUnsubscribe()
        h.setDesktopHandlers(
          () => {},
          () => {}
        )
      }
    }
  ),
}))

function Probe() {
  const actions = useAcpActions()
  const store = useConnectionStore()
  // Capture in an effect (not during render) so the lint rule that forbids
  // mutating external state mid-render stays happy; mountProvider flushes
  // effects before any test reads h.actions.
  useEffect(() => {
    h.actions = actions
    h.store = store
  }, [actions, store])
  return null
}

function ConnectionProjectionProbe({ contextKey }: { contextKey: string }) {
  const connection = useConnection(contextKey)
  const queue =
    connection.sharedSession?.queue
      .map(
        (item) =>
          `${item.queueItemId}:${item.state}:${item.errorCode ?? "none"}`
      )
      .join(",") ?? "none"
  const failures = connection.sessionFailures
    .map((failure) => `${failure.id}:${failure.title}`)
    .join(",")
  return (
    <output data-testid="connection-projection">{`${queue}|${failures}`}</output>
  )
}

async function mountProvider(children?: ReactNode) {
  const view = render(
    <AcpConnectionsProvider>
      <Probe />
      {children}
    </AcpConnectionsProvider>
  )
  await act(async () => {})
  return view
}

const TAB = "conv-1-claude_code-42"

function sharedResponse(
  overrides: Partial<import("@/lib/types").AcpConnectOrAttachResponse> = {}
) {
  return {
    connectionId: "conn",
    generation: 1,
    leaseId: "lease-1",
    leaseExpiresAt: "2026-01-01T00:01:00.000Z",
    disposition: "created" as const,
    phase: "ready" as const,
    eventSeq: 0,
    ...overrides,
  }
}

function estimatorSnapshotPatch(
  overrides: Partial<SnapshotPatch> = {}
): SnapshotPatch {
  return {
    connectionId: "spawned-conn",
    conversationId: 4_300,
    status: "prompting",
    sessionId: "codex-session",
    modes: null,
    configOptions: null,
    availableCommands: null,
    usage: null,
    liveMessage: null,
    pendingPermission: null,
    pendingAskQuestion: null,
    pendingPlanApproval: null,
    pendingUserMessage: null,
    promptCapabilities: null,
    selectorsReady: false,
    supportsFork: false,
    configStale: false,
    configStaleKind: null,
    backgroundOutstanding: 0,
    backgroundDetailRevision: 0,
    backgroundTranscriptGeneration: 0,
    sessionFailures: [],
    lastError: null,
    lastErrorDetails: null,
    eventSeq: 1,
    activeDelegations: [],
    delegationRoute: null,
    waitingForSubagents: null,
    toolWatchdogProjections: {},
    toolWatchdogMaxVersions: {},
    lastToolWatchdogDiagnostic: null,
    sharedSession: null,
    ...overrides,
  }
}

describe("AcpConnectionsProvider shared server roots", () => {
  it("connects server roots directly and installs shared state", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(
      sharedResponse({ disposition: "attached" })
    )
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })

    expect(h.acpFindConnectionForConversation).not.toHaveBeenCalled()
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(1)
    expect(h.store!.getConnection(TAB)?.sharedSession).toMatchObject({
      generation: 1,
      leaseId: "lease-1",
    })
  })

  it("does not promote another client's optimistic turn while the shared owner completes", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "queued turn B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b"
    )

    try {
      h.isDesktop = false
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 1,
        type: "prompt_dispatch_started",
        generation: 1,
        turn: {
          turn_id: "turn-a",
          queue_item_id: "queue-a",
          enqueue_seq: 1,
          client_message_id: "message-a",
          stop_requested: false,
        },
      })
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 2,
        type: "status_changed",
        status: "prompting",
      })
      emitAcpEvent(handlers, content("conn", 3, "reply A"))
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 4,
        type: "turn_complete",
        session_id: "sess",
        stop_reason: "end_turn",
        mark_awaiting_reply: false,
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "message-b",
      ])
      expect(runtime?.queuedOptimisticTurnIds).toEqual(["message-b"])
      expect(
        runtime?.localTurns
          .filter((turn) => turn.role === "assistant")
          .map((turn) => turn.blocks)
      ).toEqual([[{ type: "text", text: "reply A" }]])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("parks a future shared optimistic turn before a user-stop completion", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "queued turn B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b"
    )
    noteUserStopTurnOwnership(42)

    try {
      h.isDesktop = false
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 1,
        type: "prompt_dispatch_started",
        generation: 1,
        turn: {
          turn_id: "turn-a",
          queue_item_id: "queue-a",
          enqueue_seq: 1,
          client_message_id: "message-a",
          stop_requested: true,
        },
      })
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 2,
        type: "status_changed",
        status: "prompting",
      })
      emitAcpEvent(handlers, content("conn", 3, "cancelled reply A"))
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 4,
        type: "turn_complete",
        session_id: "sess",
        stop_reason: "cancelled",
        mark_awaiting_reply: false,
        termination_source: "user_stop",
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "message-b",
      ])
      expect(runtime?.queuedOptimisticTurnIds).toEqual(["message-b"])
      expect(
        runtime?.localTurns
          .filter((turn) => turn.role === "assistant")
          .map((turn) => turn.blocks)
      ).toEqual([[{ type: "text", text: "cancelled reply A" }]])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("completes an admitted shared turn across a sink registration gap", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "future turn B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b"
    )

    try {
      h.isDesktop = false
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })
      const handlers = latestAttachHandlers()
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "conn",
              seq: 1,
              type: "prompt_dispatch_started",
              generation: 1,
              turn: {
                turn_id: "turn-a",
                queue_item_id: "queue-a",
                enqueue_seq: 1,
                client_message_id: "message-a",
                stop_requested: false,
              },
            },
            {
              connection_id: "conn",
              seq: 2,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "turn A" }],
            },
            {
              connection_id: "conn",
              seq: 3,
              type: "status_changed",
              status: "prompting",
            },
            content("conn", 4, "reply A"),
            {
              connection_id: "conn",
              seq: 5,
              type: "turn_complete",
              session_id: "sess",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          5,
          0
        )
      })

      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.map((turn) => ({
            role: turn.role,
            blocks: turn.blocks,
          }))
      ).toEqual([
        { role: "user", blocks: [{ type: "text", text: "turn A" }] },
        { role: "assistant", blocks: [{ type: "text", text: "reply A" }] },
      ])
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.optimisticTurns.map((turn) => turn.id)
      ).toEqual(["message-b"])
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.queuedOptimisticTurnIds
      ).toEqual(["message-b"])
      expect(h.store!.getConnection(TAB)).toMatchObject({
        acceptedCompletionMessageId: expect.any(String),
        acceptedCompletionRuntimeConversationIds: [42],
      })

      const canonical = vi.fn((message: LiveMessage, isLive: boolean) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical,
      })
      expect(canonical).toHaveBeenCalledTimes(1)
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.map((turn) => turn.role)
      ).toEqual(["user", "assistant"])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it.each([
    { label: "without a sink", registerSink: false, queuePending: false },
    {
      label: "with a registered sink",
      registerSink: true,
      queuePending: false,
    },
    {
      label: "from a queued dispatch with a registered sink",
      registerSink: true,
      queuePending: true,
    },
  ])(
    "completes a fresh mapped turn $label when history already exists",
    async ({ registerSink, queuePending }) => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess")
      runtimeActions.appendViewerUserTurn(42, {
        id: "history-user",
        role: "user",
        blocks: [{ type: "text", text: "history prompt" }],
        timestamp: "2026-08-25T07:30:00.000Z",
      })
      const historyReply: LiveMessage = {
        id: "history-reply",
        role: "assistant",
        content: [{ type: "text", text: "history reply" }],
        startedAt: Date.parse("2026-08-25T07:30:01.000Z"),
      }
      runtimeActions.setLiveMessage(42, historyReply, true)
      runtimeActions.completeTurn(42, historyReply)
      if (queuePending) {
        runtimeActions.appendOptimisticTurn(
          42,
          {
            id: "message-a",
            role: "user",
            blocks: [{ type: "text", text: "turn A" }],
            timestamp: "2026-08-25T07:31:50.000Z",
          },
          "message-a",
          { queuePending: true }
        )
      }

      try {
        h.isDesktop = false
        await mountProvider()
        await act(async () => {
          await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
        })
        if (registerSink) {
          h.actions!.registerLiveSinks(TAB, {
            runtimeConversationId: 42,
            canonical: (message, isLive) => {
              runtimeActions.setLiveMessage(42, message, isLive)
              return (
                useConversationRuntimeStore.getState().byConversationId.get(42)
                  ?.liveMessage === message
              )
            },
          })
        }
        const handlers = latestAttachHandlers()
        act(() => {
          handlers.onReplay(
            [
              {
                connection_id: "conn",
                seq: 1,
                type: "prompt_dispatch_started",
                generation: 1,
                turn: {
                  turn_id: "turn-a",
                  queue_item_id: "queue-a",
                  enqueue_seq: 1,
                  client_message_id: "message-a",
                  stop_requested: false,
                },
              },
              {
                connection_id: "conn",
                seq: 2,
                type: "user_message",
                message_id: "message-a",
                blocks: [{ type: "text", text: "turn A" }],
              },
              {
                connection_id: "conn",
                seq: 3,
                type: "status_changed",
                status: "prompting",
              },
              content("conn", 4, "reply A"),
              {
                connection_id: "conn",
                seq: 5,
                type: "turn_complete",
                session_id: "sess",
                stop_reason: "end_turn",
                mark_awaiting_reply: false,
              },
            ],
            5,
            0
          )
        })

        expect(
          useConversationRuntimeStore
            .getState()
            .byConversationId.get(42)
            ?.localTurns.map((turn) => ({
              id: turn.id,
              role: turn.role,
              blocks: turn.blocks,
            }))
        ).toEqual([
          {
            id: "history-user",
            role: "user",
            blocks: [{ type: "text", text: "history prompt" }],
          },
          {
            id: "live-42-history-reply",
            role: "assistant",
            blocks: [{ type: "text", text: "history reply" }],
          },
          {
            id: "message-a",
            role: "user",
            blocks: [{ type: "text", text: "turn A" }],
          },
          {
            id: expect.stringMatching(/^live-42-/),
            role: "assistant",
            blocks: [{ type: "text", text: "reply A" }],
          },
        ])
        expect(h.store!.getConnection(TAB)).toMatchObject({
          acceptedCompletionMessageId: expect.any(String),
          acceptedCompletionRuntimeConversationIds: [42],
        })
        expect(
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.optimisticTurns
        ).toEqual([])
        expect(
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.queuedOptimisticTurnIds
        ).toEqual([])
      } finally {
        resetConversationRuntimeStore()
      }
    }
  )

  it.each(
    [false, true].flatMap((registerSink) =>
      (["empty", "tool-only", "partial"] as const).map((replayKind) => ({
        label: `${replayKind} ${registerSink ? "with" : "without"} a sink`,
        registerSink,
        replayKind,
      }))
    )
  )(
    "keeps a parser-id settled replay $label",
    async ({ registerSink, replayKind }) => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess")
      const settledTurns = [
        {
          id: "parser-user-a",
          role: "user" as const,
          blocks: [{ type: "text" as const, text: "repeat prompt" }],
          timestamp: "2026-08-25T07:30:00.000Z",
        },
        {
          id: "parser-assistant-a",
          role: "assistant" as const,
          blocks: [
            {
              type: "text" as const,
              text: "already persisted full reply",
            },
          ],
          timestamp: "2026-08-25T07:30:01.000Z",
        },
      ]
      useConversationRuntimeStore.setState((state) => {
        const current = state.byConversationId.get(42)!
        const byConversationId = new Map(state.byConversationId)
        byConversationId.set(42, {
          ...current,
          detail: {
            summary: {
              id: 42,
              folder_id: 1,
              title: null,
              title_locked: false,
              auto_title_finalized: false,
              agent_type: "claude_code",
              status: "active",
              awaiting_reply_token: null,
              kind: "regular",
              model: null,
              git_branch: null,
              external_id: "sess",
              message_count: 2,
              child_count: 0,
              created_at: "2026-08-25T07:30:00.000Z",
              updated_at: "2026-08-25T07:30:01.000Z",
              pinned_at: null,
            },
            turns: settledTurns,
          },
        })
        return { byConversationId }
      })

      try {
        h.isDesktop = false
        await mountProvider()
        await act(async () => {
          await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
        })
        if (registerSink) {
          h.actions!.registerLiveSinks(TAB, {
            runtimeConversationId: 42,
            canonical: (message, isLive) => {
              runtimeActions.setLiveMessage(42, message, isLive)
              return (
                useConversationRuntimeStore.getState().byConversationId.get(42)
                  ?.liveMessage === message
              )
            },
          })
        }

        const outputEvents: EventEnvelope[] =
          replayKind === "empty"
            ? []
            : replayKind === "partial"
              ? [content("conn", 4, "already persisted")]
              : [
                  {
                    connection_id: "conn",
                    seq: 4,
                    type: "tool_call",
                    tool_call_id: "stale-tool",
                    title: "Read",
                    kind: "read",
                    status: "completed",
                    content: null,
                    raw_input: '{"path":"old.txt"}',
                    raw_output: "old output",
                  },
                ]
        const terminalSeq = 4 + outputEvents.length
        const handlers = latestAttachHandlers()
        act(() => {
          handlers.onReplay(
            [
              {
                connection_id: "conn",
                seq: 1,
                type: "prompt_dispatch_started",
                generation: 1,
                turn: {
                  turn_id: "turn-a",
                  queue_item_id: "queue-a",
                  enqueue_seq: 1,
                  client_message_id: "message-a",
                  stop_requested: false,
                },
              },
              {
                connection_id: "conn",
                seq: 2,
                type: "user_message",
                message_id: "message-a",
                blocks: [{ type: "text", text: "repeat prompt" }],
              },
              {
                connection_id: "conn",
                seq: 3,
                type: "status_changed",
                status: "prompting",
              },
              ...outputEvents,
              {
                connection_id: "conn",
                seq: terminalSeq,
                type: "turn_complete",
                session_id: "sess",
                stop_reason: "end_turn",
                mark_awaiting_reply: false,
              },
            ],
            terminalSeq
          )
        })

        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.detail?.turns).toBe(settledTurns)
        expect(runtime?.localTurns).toEqual([])
        expect(runtime?.optimisticTurns).toEqual([])
        expect(runtime?.liveMessage).toBeNull()
        expect(h.store!.getConnection(TAB)).toMatchObject({
          acceptedCompletionMessageId: null,
          acceptedCompletionRuntimeConversationIds: null,
        })
      } finally {
        resetConversationRuntimeStore()
      }
    }
  )

  it("does not authorize an untrusted settled replay for a runtime that mounts later", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()

    try {
      h.isDesktop = false
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })
      expect(
        useConversationRuntimeStore.getState().byConversationId.has(42)
      ).toBe(false)

      const handlers = latestAttachHandlers()
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "conn",
              seq: 1,
              type: "prompt_dispatch_started",
              generation: 1,
              turn: {
                turn_id: "turn-a",
                queue_item_id: "queue-a",
                enqueue_seq: 1,
                client_message_id: "message-a",
                stop_requested: false,
              },
            },
            {
              connection_id: "conn",
              seq: 2,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "old prompt A" }],
            },
            {
              connection_id: "conn",
              seq: 3,
              type: "status_changed",
              status: "prompting",
            },
            content("conn", 4, "old reply A"),
            {
              connection_id: "conn",
              seq: 5,
              type: "turn_complete",
              session_id: "sess",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          5
        )
      })

      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess")
      const canonical = vi.fn((message: LiveMessage, isLive: boolean) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical,
      })

      expect(canonical).not.toHaveBeenCalled()
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage
      ).toBeNull()
      expect(h.store!.getConnection(TAB)).toMatchObject({
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it.each([
    {
      label: "ordinary",
      stopReason: "end_turn" as const,
      terminationSource: undefined,
    },
    {
      label: "user-stop",
      stopReason: "cancelled" as const,
      terminationSource: "user_stop" as const,
    },
  ])(
    "does not retain an accepted marker for an exact-owner untrusted $label replay",
    async ({ stopReason, terminationSource }) => {
      const {
        noteUserStopTurnOwnership,
        useConversationRuntimeStore,
        resetConversationRuntimeStore,
      } = await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess")
      runtimeActions.appendOptimisticTurn(
        42,
        {
          id: "message-a",
          role: "user",
          blocks: [{ type: "text", text: "current prompt A" }],
          timestamp: "2026-08-25T07:31:50.000Z",
        },
        "message-a"
      )
      if (terminationSource === "user_stop") {
        noteUserStopTurnOwnership(42)
      }

      try {
        h.isDesktop = false
        await mountProvider()
        await act(async () => {
          await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
        })

        const handlers = latestAttachHandlers()
        act(() => {
          handlers.onReplay(
            [
              {
                connection_id: "conn",
                seq: 1,
                type: "user_message",
                message_id: "message-a",
                blocks: [{ type: "text", text: "current prompt A" }],
              },
              {
                connection_id: "conn",
                seq: 2,
                type: "status_changed",
                status: "prompting",
              },
              content("conn", 3, "current reply A"),
              {
                connection_id: "conn",
                seq: 4,
                type: "turn_complete",
                session_id: "sess",
                stop_reason: stopReason,
                mark_awaiting_reply: false,
                ...(terminationSource
                  ? {
                      termination_source: terminationSource,
                      provider_turn_id: null,
                    }
                  : {}),
              },
            ],
            4
          )
        })

        expect(
          useConversationRuntimeStore
            .getState()
            .byConversationId.get(42)
            ?.localTurns.map((turn) => turn.role)
        ).toEqual(["user", "assistant"])
        expect(h.store!.getConnection(TAB)).toMatchObject({
          acceptedCompletionMessageId: null,
          acceptedCompletionRuntimeConversationIds: null,
        })

        runtimeActions.removeConversation(42)
        runtimeActions.setExternalId(42, "sess")
        const canonical = vi.fn((message: LiveMessage, isLive: boolean) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return true
        })
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical,
        })

        expect(canonical).not.toHaveBeenCalled()
        expect(
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.localTurns
        ).toEqual([])
      } finally {
        resetConversationRuntimeStore()
      }
    }
  )

  it("does not let an untrusted replay consume a different optimistic prompt", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "codex-session")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "current prompt B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b"
    )

    try {
      h.isDesktop = false
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "codex", "/work", "codex-session", 42)
      })

      const handlers = latestAttachHandlers()
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "spawned-conn",
              seq: 1,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "old prompt A" }],
            },
            {
              connection_id: "spawned-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("spawned-conn", 3, "old reply A"),
            {
              connection_id: "spawned-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "codex-session",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          4
        )
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "message-b",
      ])
      expect(runtime?.localTurns).toEqual([])
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.syncState).toBe("awaiting_persist")
      expect(h.store!.getConnection(TAB)).toMatchObject({
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("parks a newer optimistic prompt before an authoritative terminal-only frame", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:00.000Z",
      },
      "message-a"
    )

    try {
      h.eventStreamValue = null
      h.acpConnect.mockResolvedValue("owner-conn")
      await mountProvider()
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
        await h.actions!.connect(TAB, "claude_code", "/work", "sess-1", 42)
      })
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "turn A" }],
            },
            {
              connection_id: "owner-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 3, "reply A"),
          ])
        )
        h.runAnimationFrame()
      })

      runtimeActions.appendOptimisticTurn(
        42,
        {
          id: "message-b",
          role: "user",
          blocks: [{ type: "text", text: "future turn B" }],
          timestamp: "2026-08-25T07:31:50.000Z",
        },
        "message-b"
      )

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(
        runtime?.localTurns
          .filter((turn) => turn.role === "user")
          .map((turn) => turn.id)
      ).toEqual(["message-a"])
      expect(
        runtime?.localTurns
          .filter((turn) => turn.role === "assistant")
          .map((turn) => turn.blocks)
      ).toEqual([[{ type: "text", text: "reply A" }]])
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "message-b",
      ])
      expect(runtime?.queuedOptimisticTurnIds).toEqual(["message-b"])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("completes an authoritative terminal-only frame across a full sink gap", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:00.000Z",
      },
      "message-a"
    )

    try {
      h.eventStreamValue = null
      h.acpConnect.mockResolvedValue("owner-conn")
      await mountProvider()
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
        await h.actions!.connect(TAB, "claude_code", "/work", "sess-1", 42)
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            {
              connection_id: "owner-conn",
              seq: 2,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "turn A" }],
            },
            content("owner-conn", 3, "reply A"),
          ])
        )
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage
      ).toBeNull()
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        pendingUserMessage: { messageId: "message-a" },
      })

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns).toEqual([])
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.localTurns.map((turn) => turn.role)).toEqual([
        "user",
        "assistant",
      ])
      expect(runtime?.localTurns.at(-1)?.blocks).toEqual([
        { type: "text", text: "reply A" },
      ])
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "connected",
        lastAppliedSeq: 4,
        acceptedCompletionRuntimeConversationIds: [42],
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("does not let an untrusted user-stop replay borrow another turn's cancel fence", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "codex-session")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "current prompt B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b"
    )
    noteUserStopTurnOwnership(42)

    try {
      h.isDesktop = false
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "codex", "/work", "codex-session", 42)
      })

      const handlers = latestAttachHandlers()
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "spawned-conn",
              seq: 1,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "old prompt A" }],
            },
            {
              connection_id: "spawned-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("spawned-conn", 3, "old cancelled reply A"),
            {
              connection_id: "spawned-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "codex-session",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
            },
          ],
          4
        )
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "message-b",
      ])
      expect(runtime?.queuedOptimisticTurnIds).toEqual([])
      expect(runtime?.localTurns).toEqual([])
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.syncState).toBe("awaiting_persist")
      expect(h.store!.getConnection(TAB)).toMatchObject({
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("rejects an untrusted user-stop replay without an exact prompt id", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "codex-session")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "current prompt B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b"
    )
    noteUserStopTurnOwnership(42)

    try {
      h.isDesktop = false
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "codex", "/work", "codex-session", 42)
      })

      const handlers = latestAttachHandlers()
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "spawned-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("spawned-conn", 2, "old cancelled reply A"),
            {
              connection_id: "spawned-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "codex-session",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
            },
          ],
          3
        )
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "message-b",
      ])
      expect(runtime?.queuedOptimisticTurnIds).toEqual([])
      expect(runtime?.localTurns).toEqual([])
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.syncState).toBe("awaiting_persist")
      expect(h.store!.getConnection(TAB)).toMatchObject({
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it.each(
    [false, true].flatMap((registerSink) =>
      (["empty", "tool-only", "same-text", "prefix-text"] as const).map(
        (replyKind) => ({
          label: `${replyKind} ${registerSink ? "with" : "without"} a sink`,
          registerSink,
          replyKind,
        })
      )
    )
  )(
    "keeps a fresh repeated-prompt resume replay $label",
    async ({ registerSink, replyKind }) => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess")
      const settledTurns = [
        {
          id: "parser-user-old",
          role: "user" as const,
          blocks: [{ type: "text" as const, text: "repeat prompt" }],
          timestamp: "2026-08-25T07:30:00.000Z",
        },
        {
          id: "parser-assistant-old",
          role: "assistant" as const,
          blocks: [
            {
              type: "text" as const,
              text: "already persisted full reply",
            },
          ],
          timestamp: "2026-08-25T07:30:01.000Z",
        },
      ]
      useConversationRuntimeStore.setState((state) => {
        const current = state.byConversationId.get(42)!
        const byConversationId = new Map(state.byConversationId)
        byConversationId.set(42, {
          ...current,
          detail: {
            summary: {
              id: 42,
              folder_id: 1,
              title: null,
              title_locked: false,
              auto_title_finalized: false,
              agent_type: "claude_code",
              status: "active",
              awaiting_reply_token: null,
              kind: "regular",
              model: null,
              git_branch: null,
              external_id: "sess",
              message_count: 2,
              child_count: 0,
              created_at: "2026-08-25T07:30:00.000Z",
              updated_at: "2026-08-25T07:30:01.000Z",
              pinned_at: null,
            },
            turns: settledTurns,
          },
        })
        return { byConversationId }
      })

      try {
        h.isDesktop = false
        await mountProvider()
        await act(async () => {
          await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
        })
        const handlers = latestAttachHandlers()
        h.denormalizeSnapshot.mockReturnValue(
          estimatorSnapshotPatch({
            connectionId: "conn",
            conversationId: 42,
            status: "connected",
            sessionId: "sess",
            liveMessage: null,
            eventSeq: 10,
          })
        )
        hydrateSnapshot(handlers, { event_seq: 10 } as LiveSessionSnapshot)

        if (registerSink) {
          h.actions!.registerLiveSinks(TAB, {
            runtimeConversationId: 42,
            canonical: (message, isLive) => {
              runtimeActions.setLiveMessage(42, message, isLive)
              return (
                useConversationRuntimeStore.getState().byConversationId.get(42)
                  ?.liveMessage === message
              )
            },
          })
        }

        const outputEvents: EventEnvelope[] =
          replyKind === "empty"
            ? []
            : replyKind === "tool-only"
              ? [
                  {
                    connection_id: "conn",
                    seq: 14,
                    type: "tool_call",
                    tool_call_id: "fresh-tool",
                    title: "Read",
                    kind: "read",
                    status: "completed",
                    content: null,
                    raw_input: '{"path":"fresh.txt"}',
                    raw_output: "fresh output",
                  },
                ]
              : [
                  content(
                    "conn",
                    14,
                    replyKind === "same-text"
                      ? "already persisted full reply"
                      : "already persisted"
                  ),
                ]
        const terminalSeq = 14 + outputEvents.length
        act(() => {
          handlers.onReplay(
            [
              {
                connection_id: "conn",
                seq: 11,
                type: "prompt_dispatch_started",
                generation: 1,
                turn: {
                  turn_id: "turn-new",
                  queue_item_id: "queue-new",
                  enqueue_seq: 2,
                  client_message_id: "message-new",
                  stop_requested: false,
                },
              },
              {
                connection_id: "conn",
                seq: 12,
                type: "user_message",
                message_id: "message-new",
                blocks: [{ type: "text", text: "repeat prompt" }],
              },
              {
                connection_id: "conn",
                seq: 13,
                type: "status_changed",
                status: "prompting",
              },
              ...outputEvents,
              {
                connection_id: "conn",
                seq: terminalSeq,
                type: "turn_complete",
                session_id: "sess",
                stop_reason: "end_turn",
                mark_awaiting_reply: false,
              },
            ],
            terminalSeq,
            10
          )
        })

        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.detail?.turns).toBe(settledTurns)
        expect(
          runtime?.localTurns
            .filter((turn) => turn.role === "user")
            .map((turn) => turn.id)
        ).toEqual(["message-new"])
        const localAssistants =
          runtime?.localTurns.filter((turn) => turn.role === "assistant") ?? []
        if (replyKind === "empty") {
          expect(localAssistants).toEqual([])
        } else if (replyKind === "tool-only") {
          expect(localAssistants).toHaveLength(1)
          expect(localAssistants[0]?.blocks).toEqual(
            expect.arrayContaining([
              expect.objectContaining({
                type: "tool_use",
                tool_use_id: "fresh-tool",
              }),
            ])
          )
        } else {
          expect(
            localAssistants.flatMap((turn) =>
              turn.blocks.flatMap((block) =>
                block.type === "text" ? [block.text] : []
              )
            )
          ).toEqual([
            replyKind === "same-text"
              ? "already persisted full reply"
              : "already persisted",
          ])
        }
        expect(runtime?.optimisticTurns).toEqual([])
        expect(runtime?.liveMessage).toBeNull()
        expect(h.store!.getConnection(TAB)).toMatchObject({
          acceptedCompletionMessageId: expect.any(String),
          acceptedCompletionRuntimeConversationIds: [42],
        })
      } finally {
        resetConversationRuntimeStore()
      }
    }
  )

  it("keeps an already-promoted mapped round settled without a sink", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess")
    runtimeActions.appendViewerUserTurn(42, {
      id: "message-a",
      role: "user",
      blocks: [{ type: "text", text: "turn A" }],
      timestamp: "2026-08-25T07:30:00.000Z",
    })
    const settledReply: LiveMessage = {
      id: "settled-reply",
      role: "assistant",
      content: [{ type: "text", text: "reply A" }],
      startedAt: Date.parse("2026-08-25T07:30:01.000Z"),
    }
    runtimeActions.setLiveMessage(42, settledReply, true)
    runtimeActions.completeTurn(42, settledReply)
    const originalTurns = useConversationRuntimeStore
      .getState()
      .byConversationId.get(42)?.localTurns

    try {
      h.isDesktop = false
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })
      const handlers = latestAttachHandlers()
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "conn",
              seq: 1,
              type: "prompt_dispatch_started",
              generation: 1,
              turn: {
                turn_id: "turn-a",
                queue_item_id: "queue-a",
                enqueue_seq: 1,
                client_message_id: "message-a",
                stop_requested: false,
              },
            },
            {
              connection_id: "conn",
              seq: 2,
              type: "user_message",
              message_id: "message-a",
              blocks: [{ type: "text", text: "turn A" }],
            },
            {
              connection_id: "conn",
              seq: 3,
              type: "status_changed",
              status: "prompting",
            },
            content("conn", 4, "reply A"),
            {
              connection_id: "conn",
              seq: 5,
              type: "turn_complete",
              session_id: "sess",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          5
        )
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.localTurns).toBe(originalTurns)
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.optimisticTurns).toEqual([])
      expect(h.store!.getConnection(TAB)).toMatchObject({
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("promotes a queued optimistic turn after shared dispatch starts", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "message-b",
        role: "user",
        blocks: [{ type: "text", text: "queued turn B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "message-b",
      { queuePending: true }
    )

    try {
      h.isDesktop = false
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 1,
        type: "prompt_dispatch_started",
        generation: 1,
        turn: {
          turn_id: "turn-b",
          queue_item_id: "queue-b",
          enqueue_seq: 2,
          client_message_id: "message-b",
          stop_requested: false,
        },
      })
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 2,
        type: "user_message",
        message_id: "message-b",
        blocks: [{ type: "text", text: "queued turn B" }],
      })
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 3,
        type: "status_changed",
        status: "prompting",
      })
      emitAcpEvent(handlers, content("conn", 4, "reply B"))
      emitAcpEvent(handlers, {
        connection_id: "conn",
        seq: 5,
        type: "turn_complete",
        session_id: "sess",
        stop_reason: "end_turn",
        mark_awaiting_reply: false,
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns).toEqual([])
      expect(runtime?.queuedOptimisticTurnIds).toEqual([])
      expect(
        runtime?.localTurns.map((turn) => ({
          role: turn.role,
          blocks: turn.blocks,
        }))
      ).toEqual([
        { role: "user", blocks: [{ type: "text", text: "queued turn B" }] },
        { role: "assistant", blocks: [{ type: "text", text: "reply B" }] },
      ])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it.each([
    ["sess-a", "sess-b", "sess-a"],
    [null, null, undefined],
  ] as const)(
    "does not activate a same-id queued turn from another shared session",
    async (ownerSessionId, otherSessionId, connectionSessionId) => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      for (const [conversationId, sessionId] of [
        [42, ownerSessionId],
        [43, otherSessionId],
      ] as const) {
        if (sessionId != null) {
          runtimeActions.setExternalId(conversationId, sessionId)
        }
        runtimeActions.appendOptimisticTurn(
          conversationId,
          {
            id: "shared-message-id",
            role: "user",
            blocks: [{ type: "text", text: `queued in ${conversationId}` }],
            timestamp: "2026-08-25T07:31:50.000Z",
          },
          "shared-message-id",
          { queuePending: true }
        )
      }

      try {
        h.isDesktop = false
        await mountProvider()
        await act(async () => {
          await h.actions!.connect(
            TAB,
            "claude_code",
            "/work",
            connectionSessionId,
            42
          )
        })

        emitAcpEvent(latestAttachHandlers(), {
          connection_id: "conn",
          seq: 1,
          type: "prompt_dispatch_started",
          generation: 1,
          turn: {
            turn_id: "turn-a",
            queue_item_id: "queue-a",
            enqueue_seq: 1,
            client_message_id: "shared-message-id",
            stop_requested: false,
          },
        })

        const owner = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        const other = useConversationRuntimeStore
          .getState()
          .byConversationId.get(43)
        expect(owner?.queuedOptimisticTurnIds).toEqual([])
        expect(owner?.activeTurnToken).toBe("shared-message-id")
        expect(other?.optimisticTurns.map((turn) => turn.id)).toEqual([
          "shared-message-id",
        ])
        expect(other?.queuedOptimisticTurnIds).toEqual(["shared-message-id"])
        expect(other?.activeTurnToken).toBeNull()
        expect(other?.syncState).toBe("idle")
      } finally {
        resetConversationRuntimeStore()
      }
    }
  )

  it("notifies useConnection for queue and queue-failure projections", async () => {
    h.isDesktop = false
    await mountProvider(<ConnectionProjectionProbe contextKey={TAB} />)
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })

    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "conn",
      type: "prompt_queued",
      generation: 1,
      item: {
        queue_item_id: "queue-1",
        enqueue_seq: 1,
        client_message_id: "message-1",
        visible_text: "keep visible",
        visible_text_truncated: false,
        attachment_count: 0,
        submitted_at: "2026-01-01T00:00:00.000Z",
        state: "queued",
      },
    })
    await waitFor(() => {
      expect(screen.getByTestId("connection-projection")).toHaveTextContent(
        "queue-1:queued:none"
      )
    })

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "conn",
      type: "prompt_queue_item_failed",
      generation: 1,
      queue_item_id: "queue-1",
      error_code: "prompt_hydration_failed",
    })
    await waitFor(() => {
      expect(screen.getByTestId("connection-projection")).toHaveTextContent(
        "queue-1:failed:prompt_hydration_failed"
      )
    })
  })

  it("notifies useConnection for a session-failure projection", async () => {
    h.isDesktop = false
    await mountProvider(<ConnectionProjectionProbe contextKey={TAB} />)
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })

    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "conn",
      type: "session_failure",
      record: {
        id: "failure-1",
        revision: 1,
        category: "connection",
        severity: "error",
        title: "Connection dropped",
        actions: ["new_session"],
      },
    })

    await waitFor(() => {
      expect(screen.getByTestId("connection-projection")).toHaveTextContent(
        "failure-1:Connection dropped"
      )
    })
  })

  it("locally dismisses a failed shared prompt without backend cancellation", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "conn",
      type: "prompt_queued",
      generation: 1,
      item: {
        queue_item_id: "queue-failed",
        enqueue_seq: 1,
        client_message_id: "message-failed",
        visible_text: "recover me",
        visible_text_truncated: false,
        attachment_count: 0,
        submitted_at: "2026-01-01T00:00:00.000Z",
        state: "queued",
      },
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "conn",
      type: "prompt_queue_item_failed",
      generation: 1,
      queue_item_id: "queue-failed",
      error_code: "prompt_hydration_failed",
    })
    expect(h.store!.getConnection(TAB)?.sharedSession?.queue).toEqual([
      expect.objectContaining({
        queueItemId: "queue-failed",
        state: "failed",
      }),
    ])

    act(() => {
      h.actions!.dismissFailedSharedPrompt(TAB, "queue-failed")
    })

    expect(h.store!.getConnection(TAB)?.sharedSession?.queue).toEqual([])
    expect(h.acpCancelQueuedPrompt).not.toHaveBeenCalled()
  })

  it.each(["created", "attached"] as const)(
    "%s shared dispositions install the same client-owned state and attach immediately",
    async (disposition) => {
      h.isDesktop = false
      h.acpConnectOrAttach.mockResolvedValue(
        sharedResponse({ disposition, phase: "bootstrapping" })
      )
      await mountProvider()

      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      })

      expect(h.store!.getConnection(TAB)?.sharedSession).toMatchObject({
        generation: 1,
        leaseId: "lease-1",
        phase: { phase: "bootstrapping" },
      })
      expect(h.attach).toHaveBeenCalledWith(
        "conn",
        {
          reconnectMode: "cold",
          shared: { generation: 1, leaseId: "lease-1" },
        },
        expect.anything()
      )
    }
  )

  it("releases a shared lease on provider teardown without disconnecting", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(sharedResponse())
    const provider = await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    provider.unmount()

    await waitFor(() =>
      expect(h.acpReleaseLease).toHaveBeenCalledWith("conn", 1, "lease-1")
    )
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpTerminateSharedSession).not.toHaveBeenCalled()
  })

  it("does not fall through to process disconnect when unmount lease release fails", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(sharedResponse())
    h.acpReleaseLease.mockRejectedValueOnce(new Error("release failed"))
    const provider = await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })

    provider.unmount()
    await act(async () => {
      await Promise.resolve()
    })

    expect(h.acpReleaseLease).toHaveBeenCalledWith("conn", 1, "lease-1")
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpTerminateSharedSession).not.toHaveBeenCalled()
  })

  it("terminates only on an explicit shared disconnect", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(sharedResponse())
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      await h.actions!.disconnect(TAB, "explicit_user")
    })

    expect(h.acpTerminateSharedSession).toHaveBeenCalledWith("conn", 1)
    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })

  it("threads the current shared guard through mode, configuration, and goal actions", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(sharedResponse())
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      await h.actions!.setMode(TAB, "plan")
      await h.actions!.setConfigOption(TAB, "model", "gpt-5")
      await h.actions!.goalControl(TAB, "pause")
    })

    const guard = { generation: 1, leaseId: "lease-1" }
    expect(acpSetModeMock).toHaveBeenCalledWith("conn", "plan", guard)
    expect(acpSetConfigOptionMock).toHaveBeenCalledWith(
      "conn",
      "model",
      "gpt-5",
      guard
    )
    expect(acpGoalControlMock).toHaveBeenCalledWith("conn", "pause", guard)
  })

  it("refuses shared configuration reapply without releasing or reconnecting", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(sharedResponse())
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    h.acpConnectOrAttach.mockClear()
    h.acpReleaseLease.mockClear()
    h.acpDisconnect.mockClear()

    let reapplied = true
    await act(async () => {
      reapplied = await h.actions!.reapplyConfig(TAB)
    })

    expect(reapplied).toBe(false)
    expect(h.acpReleaseLease).not.toHaveBeenCalled()
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpConnectOrAttach).not.toHaveBeenCalled()
  })

  it("retains the event cursor when a shared detach reattaches to the same generation", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach
      .mockResolvedValueOnce(sharedResponse())
      .mockResolvedValueOnce(sharedResponse({ leaseId: "lease-2" }))
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 7,
    })
    hydrateSnapshot(latestAttachHandlers(), {} as LiveSessionSnapshot)

    act(() => latestAttachHandlers().onDetached("lease_expired"))
    await waitFor(() => expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(2))

    expect(h.attach.mock.calls.at(-1)?.[1]).toMatchObject({
      sinceSeq: 7,
      shared: { generation: 1, leaseId: "lease-2" },
    })
    expect(h.attach.mock.calls.at(-1)?.[1]).not.toHaveProperty("reconnectMode")
    expect(h.acpConnectOrAttach.mock.calls[1]?.[0].requestId).toBe(
      h.acpConnectOrAttach.mock.calls[0]?.[0].requestId
    )
  })

  it("hydrates equal-sequence authoritative shared phase, queue, and active turn", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(
      sharedResponse({ phase: "bootstrapping" })
    )
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 0,
      sharedSession: {
        generation: 1,
        leaseExpiresAt: "2026-01-01T00:02:00.000Z",
        phase: { phase: "ready" },
        queue: [
          {
            queueItemId: "queue-2",
            enqueueSeq: 2,
            clientMessageId: "message-2",
            visibleText: "next",
            visibleTextTruncated: false,
            attachmentCount: 0,
            submittedAt: "2026-01-01T00:00:02.000Z",
            state: "queued",
          },
        ],
        activeTurn: {
          turnId: "turn-1",
          queueItemId: "queue-1",
          enqueueSeq: 1,
          clientMessageId: "message-1",
          stopRequested: false,
        },
      },
    })

    hydrateSnapshot(latestAttachHandlers(), {
      event_seq: 0,
    } as LiveSessionSnapshot)

    expect(h.store!.getConnection(TAB)?.sharedSession).toMatchObject({
      phase: { phase: "ready" },
      queue: [{ queueItemId: "queue-2" }],
      activeTurn: { turnId: "turn-1" },
      leaseExpiresAt: "2026-01-01T00:02:00.000Z",
    })
  })

  it("keeps newer shared state when an older snapshot merges a latched field", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    const newerSharedSession = {
      generation: 1,
      leaseExpiresAt: "2026-01-01T00:03:00.000Z",
      phase: { phase: "closing" as const },
      queue: [],
      activeTurn: null,
    }
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 2,
      sharedSession: newerSharedSession,
    })
    hydrateSnapshot(latestAttachHandlers(), {
      event_seq: 2,
    } as LiveSessionSnapshot)

    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 1,
      selectorsReady: true,
      sharedSession: {
        generation: 1,
        leaseExpiresAt: "2026-01-01T00:01:00.000Z",
        phase: { phase: "ready" },
        queue: [],
        activeTurn: null,
      },
    })
    hydrateSnapshot(latestAttachHandlers(), {
      event_seq: 1,
    } as LiveSessionSnapshot)

    const connection = h.store!.getConnection(TAB)
    expect(connection?.selectorsReady).toBe(true)
    expect(connection?.lastAppliedSeq).toBe(2)
    expect(connection?.sharedSession).toMatchObject(newerSharedSession)
  })

  it("cold-attaches when a shared detach reconnects to a new generation", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach
      .mockResolvedValueOnce(sharedResponse())
      .mockResolvedValueOnce(
        sharedResponse({ generation: 2, leaseId: "lease-2" })
      )
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 9,
    })
    hydrateSnapshot(latestAttachHandlers(), {} as LiveSessionSnapshot)

    act(() => latestAttachHandlers().onDetached("session_replaced"))
    await waitFor(() => expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(2))

    expect(h.attach.mock.calls.at(-1)?.[1]).toMatchObject({
      reconnectMode: "cold",
      shared: { generation: 2, leaseId: "lease-2" },
    })
    expect(h.attach.mock.calls.at(-1)?.[1]).not.toHaveProperty("sinceSeq")
  })

  it("coalesces duplicate shared detach reconnect signals into one broker call", async () => {
    h.isDesktop = false
    let resolveReconnect:
      | ((response: ReturnType<typeof sharedResponse>) => void)
      | null = null
    const reconnectResponse = new Promise<ReturnType<typeof sharedResponse>>(
      (resolve) => {
        resolveReconnect = resolve
      }
    )
    h.acpConnectOrAttach
      .mockResolvedValueOnce(sharedResponse())
      .mockReturnValueOnce(reconnectResponse)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    const handlers = latestAttachHandlers()
    act(() => {
      handlers.onDetached("lease_expired")
      handlers.onDetached("generation_stale")
    })
    await waitFor(() => expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(2))
    resolveReconnect?.(sharedResponse({ leaseId: "lease-2" }))
    await waitFor(() => expect(h.attach).toHaveBeenCalledTimes(2))
  })

  it("retries a cleanup-complete shared failure with a fresh request id and generation fence", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach
      .mockResolvedValueOnce(
        sharedResponse({
          phase: "failed",
          error: { code: "bootstrap_failed", cleanupComplete: true },
        })
      )
      .mockResolvedValueOnce(
        sharedResponse({ generation: 2, leaseId: "lease-2" })
      )
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    const originalRequestId = h.acpConnectOrAttach.mock.calls[0]?.[0].requestId

    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    expect(h.acpReleaseLease).toHaveBeenCalledWith("conn", 1, "lease-1")
    expect(h.acpConnectOrAttach.mock.calls[1]?.[0]).toMatchObject({
      retryFailedGeneration: 1,
    })
    expect(h.acpConnectOrAttach.mock.calls[1]?.[0].requestId).not.toBe(
      originalRequestId
    )
    expect(h.store!.getConnection(TAB)?.sharedSession).toMatchObject({
      generation: 2,
      leaseId: "lease-2",
    })
  })

  it("mints a fresh shared request id when a draft folder or agent changes", async () => {
    h.isDesktop = false
    await mountProvider()
    const draft = "new-tab"

    await act(async () => {
      await h.actions!.connect(draft, "claude_code", "/work")
    })
    const firstId = h.acpConnectOrAttach.mock.calls[0]?.[0].requestId

    await act(async () => {
      await h.actions!.connect(draft, "claude_code", "/other")
    })
    const afterFolderChange = h.acpConnectOrAttach.mock.calls[1]?.[0].requestId
    expect(afterFolderChange).not.toBe(firstId)

    await act(async () => {
      await h.actions!.connect(draft, "codex", "/other")
    })
    expect(h.acpConnectOrAttach.mock.calls[2]?.[0].requestId).not.toBe(
      afterFolderChange
    )
  })

  it("reuses the shared request id when a draft reconnects with the same folder and agent", async () => {
    h.isDesktop = false
    await mountProvider()
    const draft = "new-tab"

    await act(async () => {
      await h.actions!.connect(draft, "claude_code", "/work")
    })
    const originalRequestId = h.acpConnectOrAttach.mock.calls[0]?.[0].requestId
    await act(async () => {
      await h.actions!.disconnect(draft, "connection_superseded")
    })
    h.acpConnectOrAttach.mockClear()

    await act(async () => {
      await h.actions!.connect(draft, "claude_code", "/work")
    })

    expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(1)
    expect(h.acpConnectOrAttach.mock.calls[0]?.[0].requestId).toBe(
      originalRequestId
    )
  })

  it("retries a shared config conflict once with a fresh request id", async () => {
    h.isDesktop = false
    const conflict = {
      code: "shared_session_config_conflict",
      message:
        "shared session configuration conflicts with connection d4cae8e7-d85f-4c92-82a2-c2eb3315d561",
    }
    h.acpConnectOrAttach
      .mockRejectedValueOnce(conflict)
      .mockResolvedValueOnce(
        sharedResponse({ connectionId: "conn-retry", leaseId: "lease-2" })
      )
    await mountProvider()

    await act(async () => {
      await h.actions!.connect("new-tab", "claude_code", "/work")
    })

    expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(2)
    expect(h.acpConnectOrAttach.mock.calls[1]?.[0].requestId).not.toBe(
      h.acpConnectOrAttach.mock.calls[0]?.[0].requestId
    )
    expect(h.store!.getConnection("new-tab")?.sharedSession).toMatchObject({
      leaseId: "lease-2",
      connectRequestId: h.acpConnectOrAttach.mock.calls[1]?.[0].requestId,
    })
    expect(h.pushAlert).not.toHaveBeenCalled()
  })

  it("does not retry a shared config conflict more than once", async () => {
    h.isDesktop = false
    const conflict = {
      code: "shared_session_config_conflict",
      message:
        "shared session configuration conflicts with connection d4cae8e7-d85f-4c92-82a2-c2eb3315d561",
    }
    h.acpConnectOrAttach.mockRejectedValue(conflict)
    await mountProvider()

    await act(async () => {
      await expect(
        h.actions!.connect("new-tab", "claude_code", "/work")
      ).rejects.toMatchObject(conflict)
    })

    expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(2)
    expect(h.acpConnectOrAttach.mock.calls[1]?.[0].requestId).not.toBe(
      h.acpConnectOrAttach.mock.calls[0]?.[0].requestId
    )
  })

  it("does not retry a shared failure until cleanup completes", async () => {
    h.isDesktop = false
    h.acpConnectOrAttach.mockResolvedValue(
      sharedResponse({
        phase: "failed",
        error: { code: "bootstrap_failed", cleanupComplete: false },
      })
    )
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })

    await expect(h.actions!.reconnect(TAB)).resolves.toBe(false)
    expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(1)
    expect(h.acpReleaseLease).not.toHaveBeenCalled()
  })

  it("guards shared cancellation and queued-item cancellation with the active lease", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 1,
      sharedSession: {
        generation: 1,
        leaseExpiresAt: "2026-01-01T00:01:00.000Z",
        phase: { phase: "ready" },
        queue: [],
        activeTurn: {
          turnId: "turn-7",
          queueItemId: "queue-7",
          enqueueSeq: 7,
          clientMessageId: null,
          stopRequested: false,
        },
      },
    })
    hydrateSnapshot(latestAttachHandlers(), {} as LiveSessionSnapshot)

    await act(async () => {
      await h.actions!.cancel(TAB)
      await h.actions!.cancelQueuedPrompt(TAB, "queue-9")
    })

    expect(h.acpCancel).toHaveBeenCalledWith("conn", {
      generation: 1,
      leaseId: "lease-1",
      turnId: "turn-7",
    })
    expect(h.acpCancelQueuedPrompt).toHaveBeenCalledWith("conn", "queue-9", {
      generation: 1,
      leaseId: "lease-1",
    })
  })

  it("sends prompt and interactive shared mutations with their generation and lease", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "hello" }])
      await h.actions!.respondPermission(TAB, "permission-1", "allow")
      await h.actions!.answerQuestion(TAB, "question-1", {
        answers: [],
        declined: false,
      })
      await h.actions!.answerPlanApproval(TAB, "approval-1", {
        decision: "approve",
      })
    })

    expect(acpPromptMock).toHaveBeenCalledWith(
      "conn",
      [{ type: "text", text: "hello" }],
      null,
      null,
      null,
      expect.anything(),
      expect.objectContaining({
        generation: 1,
        leaseId: "lease-1",
        clientInstanceId: expect.any(String),
        clientRequestId: expect.any(String),
      })
    )
    expect(acpRespondPermissionMock).toHaveBeenCalledWith(
      "conn",
      "permission-1",
      "allow",
      { generation: 1, leaseId: "lease-1" }
    )
    expect(acpAnswerQuestionMock).toHaveBeenCalledWith(
      "conn",
      "question-1",
      { answers: [], declined: false },
      { generation: 1, leaseId: "lease-1" }
    )
    expect(acpAnswerPlanApprovalMock).toHaveBeenCalledWith(
      "conn",
      "approval-1",
      { decision: "approve" },
      { generation: 1, leaseId: "lease-1" }
    )
  })

  it("uses one prompt request id per logical client message", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "first" }], {
        clientMessageId: "message-first",
      })
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "first" }], {
        clientMessageId: "message-first",
      })
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "second" }], {
        clientMessageId: "message-second",
      })
    })

    const requestId = (call: number) =>
      acpPromptMock.mock.calls[call]?.[6]?.clientRequestId
    expect(requestId(1)).toBe(requestId(0))
    expect(requestId(2)).not.toBe(requestId(0))
    expect(requestId(0)).not.toBe(
      h.acpConnectOrAttach.mock.calls[0]?.[0].requestId
    )
  })

  it("releases a busy shared preview tab instead of retaining its lease", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "conn",
      type: "status_changed",
      status: "prompting",
    })

    await act(async () => {
      await h.actions!.disconnectIfIdle(TAB)
    })

    expect(h.acpReleaseLease).toHaveBeenCalledWith("conn", 1, "lease-1")
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("cold-reattaches instead of surfacing a stale shared turn error", async () => {
    h.isDesktop = false
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/work", "sess", 42)
    })
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "conn",
      eventSeq: 1,
      sharedSession: {
        generation: 1,
        leaseExpiresAt: "2026-01-01T00:01:00.000Z",
        phase: { phase: "ready" },
        queue: [],
        activeTurn: {
          turnId: "turn-7",
          queueItemId: "queue-7",
          enqueueSeq: 7,
          clientMessageId: null,
          stopRequested: false,
        },
      },
    })
    hydrateSnapshot(latestAttachHandlers(), {} as LiveSessionSnapshot)
    h.acpCancel.mockRejectedValueOnce({
      code: "stale_turn",
      message: "turn is already settled",
    })

    await expect(h.actions!.cancel(TAB)).resolves.toBeUndefined()
    expect(h.attach.mock.calls.at(-1)?.[1]).toEqual({
      reconnectMode: "cold",
      shared: { generation: 1, leaseId: "lease-1" },
    })
  })
})

function makeSummary(
  overrides: Partial<DbConversationSummary> & { id: number }
): DbConversationSummary {
  return {
    folder_id: 1,
    title: null,
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "claude_code",
    status: "in_progress",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    pinned_at: null,
    parent_id: null,
    parent_tool_use_id: null,
    delegation_call_id: null,
    ...overrides,
  }
}

beforeEach(() => {
  h.attach.mockClear()
  h.store = null
  h.eventStreamValue = h.stream
  h.isDesktop = true
  h.reconnectListeners.clear()
  h.subscribeHandlers.clear()
  h.subscribe.mockClear()
  h.rafQueue.length = 0
  h.buildDelegationSeedEnvelopes.mockClear()
  h.acpGetAgentStatus.mockReset()
  h.acpFindConnectionForConversation.mockReset()
  h.acpConnect.mockReset()
  h.acpConnectOrAttach.mockReset()
  h.acpDisconnect.mockReset()
  h.acpReleaseLease.mockReset()
  h.acpTerminateSharedSession.mockReset()
  h.acpCancel.mockReset()
  h.acpCancel.mockResolvedValue(undefined)
  h.acpCancelQueuedPrompt.mockReset()
  h.acpCancelQueuedPrompt.mockResolvedValue(undefined)
  h.acpGetSessionSnapshot.mockReset()
  h.acpGetDesktopDeliveryCapabilities.mockReset()
  h.denormalizeSnapshot.mockReset()
  h.pushAlert.mockReset()
  h.recordFrontendTurnTrace.mockReset()
  acpPromptMock.mockReset()
  acpPromptMock.mockResolvedValue(undefined)
  acpAnswerQuestionMock.mockReset()
  acpAnswerQuestionMock.mockResolvedValue(undefined)
  acpAnswerPlanApprovalMock.mockReset()
  acpAnswerPlanApprovalMock.mockResolvedValue(undefined)
  acpSetModeMock.mockReset()
  acpSetModeMock.mockResolvedValue(undefined)
  acpSetConfigOptionMock.mockReset()
  acpSetConfigOptionMock.mockResolvedValue(undefined)
  acpGoalControlMock.mockReset()
  acpGoalControlMock.mockResolvedValue(undefined)
  acpRespondPermissionMock.mockReset()
  acpRespondPermissionMock.mockResolvedValue(undefined)
  acpTouchConnectionMock.mockReset()
  acpTouchConnectionMock.mockResolvedValue(undefined)
  resetAppWorkspaceStore()
  useAppWorkspaceStore
    .getState()
    .applyConversationUpsert(makeSummary({ id: 2 }))
  __resetStreamingConfigForProviderTests()
  __resetPublishedConnectionMapsCount()
  __resetWritableConnectionsCloneCount()
  // Durable delivery-failure flag must not leak across tests.
  try {
    sessionStorage.removeItem("codeg.desktopAcpDeliveryFailed")
  } catch {
    // ignore
  }
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    h.rafQueue.push(cb)
    return h.rafQueue.length
  })
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    if (id > 0 && id <= h.rafQueue.length) {
      h.rafQueue[id - 1] = () => {}
    }
  })
  h.denormalizeSnapshot.mockReturnValue({
    connectionId: "owner-conn",
    conversationId: null,
    status: "connected",
    sessionId: null,
    modes: null,
    configOptions: null,
    availableCommands: null,
    usage: null,
    liveMessage: null,
    pendingPermission: null,
    pendingAskQuestion: null,
    pendingUserMessage: null,
    promptCapabilities: null,
    selectorsReady: false,
    supportsFork: false,
    configStale: false,
    configStaleKind: null,
    lastError: null,
    eventSeq: 0,
    activeDelegations: [],
    toolWatchdogProjections: {},
    delegationRoute: null,
  })
  // Agent is installed + available so the connect preflight passes.
  h.acpGetAgentStatus.mockResolvedValue({
    agent_type: "claude_code",
    enabled: true,
    available: true,
    installed_version: "1.0.0",
    host_tools_agent_mode: false,
    is_acp_adapter: true,
  })
  h.acpConnect.mockResolvedValue("spawned-conn")
  h.acpConnectOrAttach.mockResolvedValue(sharedResponse())
  h.acpReleaseLease.mockResolvedValue(undefined)
  h.acpTerminateSharedSession.mockResolvedValue(undefined)
  h.acpDisconnect.mockResolvedValue(undefined)
  h.acpGetSessionSnapshot.mockResolvedValue(null)
  h.acpGetDesktopDeliveryCapabilities.mockResolvedValue({
    mode: "batched",
    flags: {
      desktop_acp_event_batching: true,
      incremental_live_transcript: false,
      deferred_streaming_rich_content: false,
    },
    perf_replay_available: true,
    failure_event: "acp://delivery-failed",
  })
})

function latestAttachHandlers(): AttachHandlers {
  const calls = h.attach.mock.calls as unknown as Array<
    [unknown, unknown, AttachHandlers]
  >
  const call = calls[calls.length - 1]
  expect(call).toBeTruthy()
  if (!call) throw new Error("expected attach handlers")
  return call[2]
}

function emitAcpEvent(handlers: AttachHandlers, envelope: EventEnvelope) {
  act(() => {
    handlers.onEvent(envelope)
  })
}

function hydrateSnapshot(
  handlers: AttachHandlers,
  snapshot: LiveSessionSnapshot
) {
  act(() => {
    handlers.onSnapshot(snapshot, snapshot.event_seq)
  })
}

async function connectCodex(conversationId: number) {
  h.acpFindConnectionForConversation.mockResolvedValue(null)
  await mountProvider()
  await act(async () => {
    await h.actions!.connect(
      TAB,
      "codex",
      "/tmp/x",
      "codex-session",
      conversationId
    )
  })
  const handlers = latestAttachHandlers()
  emitAcpEvent(handlers, {
    seq: 1,
    connection_id: "spawned-conn",
    type: "status_changed",
    status: "prompting",
    received_at: 10,
  })
  return handlers
}

describe("Codex estimated request usage", () => {
  it("publishes one notification after committing an estimated sample", async () => {
    const conversationId = 4_210
    const handlers = await connectCodex(conversationId)
    const committedSampleCounts: number[] = []
    const unsubscribe = subscribeRequestUsage(() => {
      committedSampleCounts.push(
        h.store!.getConnection(TAB)?.requestUsage?.sampleCount ?? -1
      )
    })

    try {
      emitAcpEvent(handlers, {
        seq: 2,
        connection_id: "spawned-conn",
        type: "content_delta",
        text: "abcd",
        received_at: 100,
      })
      emitAcpEvent(handlers, {
        seq: 3,
        connection_id: "spawned-conn",
        type: "usage_update",
        used: 10,
        size: 100,
        received_at: 1_100,
      })
    } finally {
      unsubscribe()
    }

    expect(committedSampleCounts).toEqual([1])
  })

  it("settles root output at a plain usage_update using ingest duration", async () => {
    const conversationId = 4_201
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "abcd",
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 10,
      size: 100,
      received_at: 1_100,
    })

    expect(getPublishedRequestUsage(conversationId)).toEqual({
      outputTokens: 2,
      generationMs: 1_000,
      tps: 2,
      sampleCount: 1,
      estimatedSampleCount: 1,
    })
    expect(h.store!.getConnection(TAB)?.usage).toEqual({ used: 10, size: 100 })
  })

  it("uses ingest stamps when output and boundary share one replay frame", async () => {
    const conversationId = 4_208
    const handlers = await connectCodex(conversationId)
    const reducerNow = vi.spyOn(performance, "now").mockReturnValue(50_000)

    try {
      act(() => {
        handlers.onReplay(
          [
            {
              seq: 2,
              connection_id: "spawned-conn",
              type: "content_delta",
              text: "abcd",
              received_at: 100,
            },
            {
              seq: 3,
              connection_id: "spawned-conn",
              type: "usage_update",
              used: 10,
              size: 100,
              received_at: 1_100,
            },
          ],
          3
        )
      })
    } finally {
      reducerNow.mockRestore()
    }

    expect(getPublishedRequestUsage(conversationId)).toMatchObject({
      outputTokens: 2,
      generationMs: 1_000,
      tps: 2,
      sampleCount: 1,
      estimatedSampleCount: 1,
    })
  })

  it("does not settle boundaries before output or duplicate boundaries", async () => {
    const conversationId = 4_202
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "abcd",
      received_at: 200,
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 2,
      size: 100,
      received_at: 1_200,
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 2,
      size: 100,
      received_at: 1_300,
    })

    expect(getPublishedRequestUsage(conversationId).sampleCount).toBe(1)
  })

  it("processes exact usage first and suppresses its matching estimate", async () => {
    const conversationId = 4_203
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "a".repeat(400),
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "request_usage",
      output_tokens: 77,
      received_at: 1_100,
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 77,
      size: 100_000,
      received_at: 1_101,
    })

    expect(getPublishedRequestUsage(conversationId)).toEqual({
      outputTokens: 77,
      generationMs: 1_000,
      tps: 77,
      sampleCount: 1,
      estimatedSampleCount: 0,
    })
  })

  it("settles exact usage after 10 to 12 to 3 retracts visible output to zero", async () => {
    const conversationId = 4_209
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "reconciled-tool",
      title: "terminal",
      kind: "execute",
      status: "in_progress",
      content: null,
      raw_input: "a".repeat(40),
      raw_input_is_model_authored: true,
      raw_output: null,
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "request_usage",
      output_tokens: 10,
      received_at: 1_100,
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 10,
      size: 100_000,
      received_at: 1_101,
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "tool_call_update",
      tool_call_id: "reconciled-tool",
      title: null,
      status: "in_progress",
      content: null,
      raw_input: "a".repeat(48),
      raw_input_is_model_authored: true,
      raw_output: null,
      received_at: 2_000,
    })
    emitAcpEvent(handlers, {
      seq: 6,
      connection_id: "spawned-conn",
      type: "tool_call_update",
      tool_call_id: "reconciled-tool",
      title: null,
      status: "in_progress",
      content: null,
      raw_input: "b".repeat(12),
      raw_input_is_model_authored: true,
      raw_output: null,
      received_at: 2_100,
    })

    expect(h.store!.getConnection(TAB)?.requestEstimator).toMatchObject({
      startedAt: 2_000,
      visibleTokens: 0,
    })

    emitAcpEvent(handlers, {
      seq: 7,
      connection_id: "spawned-conn",
      type: "request_usage",
      output_tokens: 7,
      received_at: 3_000,
    })
    emitAcpEvent(handlers, {
      seq: 8,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 17,
      size: 100_000,
      received_at: 3_001,
    })

    expect(getPublishedRequestUsage(conversationId)).toEqual({
      outputTokens: 17,
      generationMs: 2_000,
      tps: 8.5,
      sampleCount: 2,
      estimatedSampleCount: 0,
    })
    expect(h.store!.getConnection(TAB)?.requestEstimator?.startedAt).toBeNull()
  })

  it("ignores a late exact Codex sample before new output", async () => {
    const conversationId = 4_204
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "abcd",
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })
    const before = getPublishedRequestUsage(conversationId)
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "request_usage",
      output_tokens: 999,
      received_at: 1_200,
    })

    expect(getPublishedRequestUsage(conversationId)).toBe(before)
  })

  it("counts root thinking, plan text, and only explicitly authored tool input", async () => {
    const conversationId = 4_205
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "thinking",
      text: "a".repeat(40),
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "plan_update",
      entries: [
        { content: "b".repeat(40), priority: "medium", status: "pending" },
      ],
      received_at: 200,
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "synthetic",
      title: "edit",
      kind: "edit",
      status: "pending",
      content: null,
      raw_input: "c".repeat(400),
      raw_output: null,
      received_at: 300,
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "authored",
      title: "terminal",
      kind: "execute",
      status: "pending",
      content: null,
      raw_input: "d".repeat(40),
      raw_input_is_model_authored: true,
      raw_output: null,
      received_at: 400,
    })
    emitAcpEvent(handlers, {
      seq: 6,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })

    expect(getPublishedRequestUsage(conversationId).outputTokens).toBe(56)
  })

  it("excludes parented subagent output", async () => {
    const conversationId = 4_206
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "a".repeat(400),
      parent_tool_use_id: "parent-tool",
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })

    expect(getPublishedRequestUsage(conversationId).sampleCount).toBe(0)
  })

  it("starts the second request after tool time instead of including the gap", async () => {
    const conversationId = 4_207
    const handlers = await connectCodex(conversationId)
    for (const event of [
      {
        seq: 2,
        type: "content_delta" as const,
        text: "abcd",
        received_at: 100,
      },
      {
        seq: 3,
        type: "usage_update" as const,
        used: 1,
        size: 100,
        received_at: 1_100,
      },
      {
        seq: 4,
        type: "tool_call_update" as const,
        tool_call_id: "tool",
        title: null,
        status: "completed",
        content: null,
        raw_input: null,
        raw_output: "result",
        received_at: 8_000,
      },
      {
        seq: 5,
        type: "content_delta" as const,
        text: "efgh",
        received_at: 10_000,
      },
      {
        seq: 6,
        type: "usage_update" as const,
        used: 2,
        size: 100,
        received_at: 11_000,
      },
    ]) {
      emitAcpEvent(handlers, {
        connection_id: "spawned-conn",
        ...event,
      })
    }

    expect(getPublishedRequestUsage(conversationId)).toMatchObject({
      sampleCount: 2,
      generationMs: 2_000,
      estimatedSampleCount: 2,
    })
  })
})

describe("request estimator hydration", () => {
  it("preserves active state for a stale snapshot", async () => {
    const conversationId = 4_301
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "abcd",
      received_at: 100,
    })

    h.denormalizeSnapshot.mockReturnValue(
      estimatorSnapshotPatch({ eventSeq: 1, conversationId })
    )
    hydrateSnapshot(handlers, {
      event_seq: 1,
    } as unknown as LiveSessionSnapshot)
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })

    expect(getPublishedRequestUsage(conversationId).sampleCount).toBe(1)
  })

  it("accepted hydration clears the ledger and seeds unchanged plan/tool input", async () => {
    const conversationId = 4_302
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "abcd",
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })
    expect(getPublishedRequestUsage(conversationId).estimatedSampleCount).toBe(
      1
    )

    h.denormalizeSnapshot.mockReturnValue(
      estimatorSnapshotPatch({
        eventSeq: 4,
        conversationId,
        liveMessage: {
          id: "hydrated-live",
          role: "assistant",
          startedAt: Date.now(),
          content: [
            {
              type: "plan",
              entries: [
                {
                  content: "seeded plan",
                  priority: "medium",
                  status: "pending",
                },
              ],
            },
            {
              type: "tool_call",
              info: {
                tool_call_id: "seeded-tool",
                title: "terminal",
                kind: "execute",
                status: "pending",
                content: null,
                raw_input: "seeded args",
                raw_output_chunks: [],
                raw_output_total_bytes: 0,
                locations: null,
                meta: null,
                images: [],
              },
            },
          ],
        },
      })
    )
    hydrateSnapshot(handlers, {
      event_seq: 4,
    } as unknown as LiveSessionSnapshot)

    expect(getPublishedRequestUsage(conversationId)).toEqual(
      EMPTY_REQUEST_USAGE
    )

    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "tool_call_update",
      tool_call_id: "seeded-tool",
      title: null,
      status: "in_progress",
      content: null,
      raw_input: "seeded args",
      raw_input_is_model_authored: true,
      raw_output: null,
      received_at: 2_000,
    })
    emitAcpEvent(handlers, {
      seq: 6,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 2,
      size: 100,
      received_at: 3_000,
    })
    expect(getPublishedRequestUsage(conversationId).sampleCount).toBe(0)

    emitAcpEvent(handlers, {
      seq: 7,
      connection_id: "spawned-conn",
      type: "tool_call_update",
      tool_call_id: "seeded-tool",
      title: null,
      status: "in_progress",
      content: null,
      raw_input: "seeded args plus new model text",
      raw_input_is_model_authored: true,
      raw_output: null,
      received_at: 4_000,
    })
    emitAcpEvent(handlers, {
      seq: 8,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 3,
      size: 100,
      received_at: 5_000,
    })
    expect(getPublishedRequestUsage(conversationId)).toMatchObject({
      sampleCount: 1,
      estimatedSampleCount: 1,
    })
  })

  it.each([
    { type: "turn_attempt_rollback" as const, attempt: 1 },
    { type: "status_changed" as const, status: "connected" as const },
  ])("discards unsettled output on $type", async (resetEvent) => {
    const conversationId =
      resetEvent.type === "turn_attempt_rollback" ? 4_303 : 4_304
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "unsettled output",
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      received_at: 200,
      ...resetEvent,
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })

    expect(getPublishedRequestUsage(conversationId).sampleCount).toBe(0)
  })

  it("new prompting state clears the prior ledger and active request", async () => {
    const conversationId = 4_305
    const handlers = await connectCodex(conversationId)
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "abcd",
      received_at: 100,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
      received_at: 1_100,
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
      received_at: 2_000,
    })

    expect(getPublishedRequestUsage(conversationId)).toEqual(
      EMPTY_REQUEST_USAGE
    )
  })
})

describe("AcpConnectionsProvider cross-client viewer lifecycle", () => {
  it("attaches as a viewer (no spawn) when a live connection is discovered", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 5,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })

    // Discovery ran for the conversation (with the sessionId + agentType
    // fallback), and we attached to the owner's connection instead of spawning.
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledWith(
      42,
      "sess-1",
      "claude_code"
    )
    expect(h.acpConnect).not.toHaveBeenCalled()
    // COLD attach: a viewer has applied no prior events, so it must request a
    // full snapshot (sinceSeq undefined) — NOT the discovered event_seq, which
    // could yield only a post-cursor replay and miss all earlier live state.
    expect(h.attach).toHaveBeenCalledWith(
      "owner-conn",
      { sinceSeq: undefined },
      expect.anything()
    )
  })

  it("spawns + owns when no live connection is discovered", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })

    expect(h.acpFindConnectionForConversation).toHaveBeenCalledWith(
      42,
      "sess-1",
      "claude_code"
    )
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    expect(h.attach).toHaveBeenCalledWith(
      "spawned-conn",
      expect.anything(),
      expect.anything()
    )
  })

  it("skips discovery entirely when no persisted conversationId is given", async () => {
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    expect(h.acpFindConnectionForConversation).not.toHaveBeenCalled()
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
  })

  it("viewer teardown detaches WITHOUT acpDisconnect (never kills the owner's agent)", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.acpConnect).not.toHaveBeenCalled()

    await act(async () => {
      await h.actions!.disconnect(TAB)
    })

    // The critical safety property: a viewer must never disconnect the backend
    // connection — it belongs to another client.
    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })

  it("replacing a viewer (changed params) detaches WITHOUT acpDisconnect", async () => {
    // A re-connect at the same tab with a different workingDir releases the
    // observer alias and runs the own_or_observe handoff settle poll. While
    // the prior owner connection is still live, handoff re-attaches as viewer
    // and must NEVER acpDisconnect the owner's connection.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    vi.useFakeTimers()
    try {
      let pending!: Promise<void>
      await act(async () => {
        pending = h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/other",
          "sess-1",
          42
        )
      })
      // Full handoff settle schedule: [0, 300, 700, 1500, 2500]
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0)
        await vi.advanceTimersByTimeAsync(300)
        await vi.advanceTimersByTimeAsync(700)
        await vi.advanceTimersByTimeAsync(1500)
        await vi.advanceTimersByTimeAsync(2500)
        await pending
      })
    } finally {
      vi.useRealTimers()
    }

    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })

  it("owner teardown DOES acpDisconnect its own connection", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.acpConnect).toHaveBeenCalledTimes(1)

    await act(async () => {
      await h.actions!.disconnect(TAB)
    })

    expect(h.acpDisconnect).toHaveBeenCalledWith("spawned-conn", {
      origin: "explicit_user",
    })
  })

  it("labels provider cleanup, disconnectAll, idle reap, and supersession", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)

    const provider = await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "codex", "/repo")
    })
    h.acpDisconnect.mockClear()
    provider.unmount()
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "provider_unmount" })
    )

    h.acpDisconnect.mockClear()
    vi.useFakeTimers()
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "codex", "/repo")
      await h.actions!.disconnectAll()
    })
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "disconnect_all" })
    )

    h.acpDisconnect.mockClear()
    await act(async () => {
      await h.actions!.connect(TAB, "codex", "/repo")
      await h.actions!.connect(TAB, "claude_code", "/other")
    })
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "connection_superseded" })
    )

    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })
    h.acpDisconnect.mockClear()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(121_000)
    })
    vi.useRealTimers()
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "idle_timeout" })
    )
  })

  it("desktop viewer torn down DURING snapshot fetch does not seed delegations or route", async () => {
    // Desktop firehose path (no EventStream). If the viewer's tab disconnects
    // while acpGetSessionSnapshot is in flight, the resumed attach must NOT
    // hydrate / seed child delegation streams / install reverse-map routing for
    // a viewer that no longer exists.
    h.eventStreamValue = null
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    let resolveSnapshot: (v: unknown) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSnapshot = res
        })
    )
    await mountProvider()

    // Start the viewer connect; it suspends on the pending snapshot AFTER
    // dispatching CONNECTION_CREATED (the entry now exists in the store).
    let connectPromise: Promise<void> | undefined
    await act(async () => {
      connectPromise = h.actions!.connect(TAB, "claude_code", "/tmp/x", "s", 42)
    })
    // Tear the viewer down while the snapshot is still in flight.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    // Snapshot resolves only AFTER teardown; the resumed attach must bail.
    await act(async () => {
      resolveSnapshot({ connection_id: "owner-conn" })
      await connectPromise
    })

    expect(h.buildDelegationSeedEnvelopes).not.toHaveBeenCalled()
    // And teardown never killed the owner's connection.
    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })
})

// Single-clicking a sidebar conversation opens a PREVIEW tab; the next
// single-click replaces it. That release must never end a turn the user only
// clicked in to watch — an owner's acpDisconnect kills the agent CLI mid-turn,
// which the agent writes into its transcript as an interrupted request.
describe("AcpConnectionsProvider preview-tab release (disconnectIfIdle)", () => {
  async function connectOwner(): Promise<AttachHandlers> {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    return latestAttachHandlers()
  }

  it("keeps a PROMPTING owner alive when its preview tab is replaced", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })

    await act(async () => {
      await h.actions!.disconnectIfIdle(TAB)
    })

    expect(h.acpDisconnect).not.toHaveBeenCalled()
    // Left in the store, still streaming: the idle sweep reclaims it once the
    // turn settles (the tab is gone, so nothing else keeps it alive).
    expect(h.store!.getConnection(TAB)?.status).toBe("prompting")
  })

  it("keeps an owner with outstanding background work alive", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })
    // Turn is over, but launched sub-agents / background shells are not:
    // disconnecting would kill the agent CLI and that work with it.
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-1",
      turns: [],
      outstanding: 1,
      settled: [],
      watermark: 0,
    })
    expect(h.store!.getConnection(TAB)?.backgroundOutstanding).toBe(1)

    await act(async () => {
      await h.actions!.disconnectIfIdle(TAB)
    })

    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })

  it("disconnects an IDLE owner right away (the reclaim this release exists for)", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })

    await act(async () => {
      await h.actions!.disconnectIfIdle(TAB)
    })

    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "explicit_user" })
    )
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("detaches a mid-turn VIEWER without killing the owner's agent", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "owner-conn",
      type: "status_changed",
      status: "prompting",
    })

    await act(async () => {
      await h.actions!.disconnectIfIdle(TAB)
    })

    // A viewer never owns the backend process, so busy or not it detaches —
    // and the idle sweep skips viewers, so leaving one would leak its stream.
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })
})

// AIR typed session failures: retry warnings must settle ONLY at a clean
// `end_turn` — a cancelled/failed exit did not recover, and a failed turn's
// terminal failure arrives as a `session_failure` event emitted just before
// its `turn_complete` (the record rides the prompt RESPONSE `_meta`; both
// adapters disguise that response as `end_turn`). Settling on any
// leave-prompting transition painted a still-dead connection as a recovered
// warning (2026-08-15 field report).
describe("AcpConnectionsProvider AIR session-failure lifecycle", () => {
  async function connectOwner(): Promise<AttachHandlers> {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    return latestAttachHandlers()
  }

  function failure(
    id: string,
    revision: number,
    severity: string,
    title: string
  ) {
    return {
      id,
      revision,
      category: "connection",
      severity,
      title,
      actions: ["new_session"],
    }
  }

  it("escalates the response-borne terminal error instead of settling it at the disguised end_turn", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "session_failure",
      record: failure(
        "t1:error",
        5,
        "warning",
        "Reconnecting to Claude, attempt 5 of 5."
      ),
    })
    // The terminal record rides the prompt response; the backend emits it
    // BEFORE turn_complete as a same-id higher-revision error escalation.
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "session_failure",
      record: failure(
        "t1:error",
        6,
        "error",
        "The connection to Claude was lost."
      ),
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sess-1",
      stop_reason: "end_turn",
    })

    const failures = h.store!.getConnection(TAB)?.sessionFailures
    expect(failures).toHaveLength(1)
    expect(failures?.[0]).toMatchObject({
      id: "t1:error",
      revision: 6,
      severity: "error",
      resolved: false,
    })
  })

  it("keeps warnings active across a cancelled exit and settles them only on a clean end_turn", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "session_failure",
      record: failure(
        "t1:error",
        1,
        "warning",
        "Reconnecting to Claude, attempt 1 of 5."
      ),
    })
    // Cancelled exit: not recovery — the amber strip must survive it.
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sess-1",
      stop_reason: "cancelled",
    })
    expect(h.store!.getConnection(TAB)?.sessionFailures?.[0]).toMatchObject({
      id: "t1:error",
      resolved: false,
    })

    // A later clean turn end is the recovery evidence that settles it.
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sess-1",
      stop_reason: "end_turn",
    })
    expect(h.store!.getConnection(TAB)?.sessionFailures?.[0]).toMatchObject({
      id: "t1:error",
      resolved: true,
    })
  })

  // Issue #496: with `end_turn` as the only mid-flight settle point, a long
  // turn that reconnected N times stacked N permanent amber strips under the
  // composer. Turn PROGRESS settles the incident — codex's own
  // `completeRetryIncidentOnTurnProgress`.
  it("settles retry incidents as soon as the turn produces output again", async () => {
    const handlers = await connectOwner()
    const categorized = (id: string, category: string, severity: string) => ({
      id,
      revision: 1,
      category,
      severity,
      title: `${id} title`,
      actions: [],
    })
    const failuresNow = () => {
      const table = h.store!.getConnection(TAB)?.sessionFailures ?? []
      return Object.fromEntries(table.map((f) => [f.id, f.resolved]))
    }

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    for (const [i, rec] of [
      categorized("i1", "connection", "warning"),
      categorized("i2", "service", "warning"),
      // Informational, not an incident: codex config/skill-budget notices and
      // claude advisories both land on category "unknown". Progress must leave
      // them readable.
      categorized("notice", "unknown", "warning"),
      categorized("err", "connection", "error"),
    ].entries()) {
      emitAcpEvent(handlers, {
        seq: 2 + i,
        connection_id: "spawned-conn",
        type: "session_failure",
        record: rec,
      })
    }
    expect(failuresNow()).toEqual({
      i1: false,
      i2: false,
      notice: false,
      err: false,
    })

    emitAcpEvent(handlers, {
      seq: 6,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "back online",
    })
    expect(failuresNow()).toEqual({
      i1: true,
      i2: true,
      notice: false,
      err: false,
    })

    // A local tool call ADVANCING proves nothing about the upstream, so
    // `tool_call_update` deliberately does not settle.
    emitAcpEvent(handlers, {
      seq: 7,
      connection_id: "spawned-conn",
      type: "session_failure",
      record: categorized("i3", "limit", "warning"),
    })
    emitAcpEvent(handlers, {
      seq: 8,
      connection_id: "spawned-conn",
      type: "tool_call_update",
      tool_call_id: "call_1",
      title: "Bash",
      status: "in_progress",
      content: null,
      raw_input: null,
      raw_output: null,
    })
    expect(failuresNow().i3).toBe(false)

    // A NEW tool call is model output, so it does.
    emitAcpEvent(handlers, {
      seq: 9,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "call_2",
      title: "Read",
      kind: "read",
      status: "pending",
      content: null,
      raw_input: null,
      raw_output: null,
    })
    expect(failuresNow().i3).toBe(true)

    // The notice still waits for the clean boundary; the error outlives it.
    emitAcpEvent(handlers, {
      seq: 10,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sess-1",
      stop_reason: "end_turn",
    })
    expect(failuresNow()).toMatchObject({ notice: true, err: false })
  })
})

// The composer's connection-status popover. Unlike `reapplyConfig` (live owners
// only), this has to work from EVERY state the icon can show — including the
// states where the store holds no entry at all.
describe("AcpConnectionsProvider reconnect (status-icon button)", () => {
  async function connectOwner() {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
  }

  it("restarts a live owner with the same identity", async () => {
    await connectOwner()
    h.acpConnect.mockResolvedValue("respawned-conn")

    let result: boolean | undefined
    await act(async () => {
      result = await h.actions!.reconnect(TAB)
    })

    expect(result).toBe(true)
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "connection_superseded" })
    )
    // Same agent / cwd / session — the point is a fresh PROCESS, not new params,
    // which is exactly what connect()'s "nothing changed" fast path would skip.
    expect(h.acpConnect).toHaveBeenLastCalledWith(
      "claude_code",
      "/tmp/x",
      "sess-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("respawned-conn")
  })

  it("reconnects with the live conversation linked after an external-only connect", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "external-only")
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "conversation_linked",
      conversation_id: 99,
      folder_id: 1,
    })
    expect(h.store!.getConnection(TAB)?.conversationId).toBe(99)

    h.acpConnect.mockClear()
    h.acpConnect.mockResolvedValue("respawned-linked")
    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    expect(h.acpConnect).toHaveBeenLastCalledWith(
      "claude_code",
      "/tmp/x",
      "external-only",
      undefined,
      {},
      99,
      undefined,
      null
    )
  })

  it("rebuilds even when the backend no longer knows the connection", async () => {
    await connectOwner()
    // The single most important case for this button: the agent process is
    // already gone (reaped by another window, crashed, backend restarted), so
    // the teardown 404s. That must not abort the respawn.
    h.acpDisconnect.mockRejectedValue(new Error("Connection not found"))
    h.acpConnect.mockResolvedValue("respawned-conn")

    let result: boolean | undefined
    await act(async () => {
      result = await h.actions!.reconnect(TAB)
    })

    expect(result).toBe(true)
    expect(h.acpConnect).toHaveBeenLastCalledWith(
      "claude_code",
      "/tmp/x",
      "sess-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("respawned-conn")
  })

  it("reconnects a tab whose connection is gone entirely", async () => {
    await connectOwner()
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    h.acpConnect.mockClear()
    h.acpDisconnect.mockClear()

    let result: boolean | undefined
    await act(async () => {
      result = await h.actions!.reconnect(TAB)
    })

    expect(result).toBe(true)
    // Nothing to tear down — the params come from what connect() recorded.
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sess-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
  })

  it("reconnects after a connect that never produced a connection", async () => {
    // The `error` state the icon shows for an agent that failed its preflight:
    // no store entry was ever created, so only the recorded params survive.
    h.acpGetAgentStatus.mockResolvedValue({
      agent_type: "claude_code",
      enabled: true,
      available: false,
      installed_version: null,
      host_tools_agent_mode: false,
      is_acp_adapter: true,
    })
    await mountProvider()
    await act(async () => {
      await h
        .actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
        .catch(() => {})
    })
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.actions!.getReconnectInfo(TAB)).toEqual({
      agentType: "claude_code",
      workingDir: "/tmp/x",
      sessionId: "sess-1",
    })

    h.acpGetAgentStatus.mockResolvedValue({
      agent_type: "claude_code",
      enabled: true,
      available: true,
      installed_version: "1.0.0",
      host_tools_agent_mode: false,
      is_acp_adapter: true,
    })
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sess-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
  })

  it("re-attaches a viewer without killing the owner's agent", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)

    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)
  })

  it("is a no-op for a key that was never connected", async () => {
    await mountProvider()

    expect(h.actions!.getReconnectInfo(TAB)).toBeNull()
    let result: boolean | undefined
    await act(async () => {
      result = await h.actions!.reconnect(TAB)
    })

    expect(result).toBe(false)
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })

  it("refuses a delegation child — the broker owns its lifetime", async () => {
    await mountProvider()
    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "child-conn",
        parentConnectionId: "parent-conn",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
    })
    expect(h.store!.getConnection("child-conn")?.isDelegationChild).toBe(true)

    let result: boolean | undefined
    await act(async () => {
      result = await h.actions!.reconnect("child-conn")
    })

    expect(result).toBe(false)
    expect(h.actions!.getReconnectInfo("child-conn")).toBeNull()
    expect(h.acpDisconnect).not.toHaveBeenCalled()
  })

  it("forgets remembered params on disconnectAll, so a recycled key can't resurrect the old session", async () => {
    await connectOwner()
    await act(async () => {
      await h.actions!.disconnectAll()
    })

    expect(h.actions!.getReconnectInfo(TAB)).toBeNull()
  })

  it("still rebuilds when a connect for the same key is already in flight", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    let releaseConnect: (connectionId: string) => void = () => {}
    h.acpConnect.mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          releaseConnect = resolve
        })
    )

    let firstConnect: Promise<void> | undefined
    await act(async () => {
      firstConnect = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sess-1",
        42
      )
      await Promise.resolve()
    })

    // The state users actually click this button in: the connect is HUNG, so
    // there is no store entry to tear down and the params are identical.
    // connect() parks a same-parameter request as pending and its `finally`
    // then drops it as a duplicate — so this used to spin the button once and
    // change nothing at all.
    h.acpConnect.mockResolvedValue("respawned-conn")
    let reconnectResult: Promise<boolean> | undefined
    await act(async () => {
      reconnectResult = h.actions!.reconnect(TAB)
      await Promise.resolve()
    })

    await act(async () => {
      releaseConnect("spawned-conn")
      await firstConnect
      await reconnectResult
    })

    expect(await reconnectResult).toBe(true)
    // Waited for the hung attempt to settle, then rebuilt what it produced.
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "connection_superseded" })
    )
    expect(h.acpConnect).toHaveBeenLastCalledWith(
      "claude_code",
      "/tmp/x",
      "sess-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("respawned-conn")
  })

  it("gives the button back when the in-flight connect never answers", async () => {
    vi.useFakeTimers()
    try {
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await mountProvider()
      // Never resolves: a wedged IPC, which is a state users click Reconnect
      // from. Waiting on it unbounded would spin the button forever.
      h.acpConnect.mockImplementationOnce(() => new Promise<string>(() => {}))
      await act(async () => {
        void h
          .actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
          .catch(() => {})
        await Promise.resolve()
      })

      let settled: boolean | undefined
      const pending = h.actions!.reconnect(TAB).then((r) => {
        settled = r
        return r
      })
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15_000)
      })
      await act(async () => {
        await pending
      })

      // Reports "nothing happened" rather than hanging — the user can retry.
      expect(settled).toBe(false)
    } finally {
      vi.useRealTimers()
    }
  })

  it("resumes a session known only from a snapshot hydrate (cold attach)", async () => {
    // The event that carries the sessionId fired BEFORE this client attached,
    // so it is never replayed — the snapshot is the only place identity
    // appears, and it lands on the store entry alone.
    h.denormalizeSnapshot.mockReturnValue({
      connectionId: "spawned-conn",
      status: "connected",
      sessionId: "snapshot-session",
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      lastError: null,
      eventSeq: 7,
      activeDelegations: [],
    })
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", undefined, 42)
    })
    hydrateSnapshot(latestAttachHandlers(), {
      event_seq: 7,
    } as unknown as LiveSessionSnapshot)
    expect(h.store!.getConnection(TAB)?.sessionId).toBe("snapshot-session")

    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    expect(h.actions!.getReconnectInfo(TAB)?.sessionId).toBe("snapshot-session")

    h.acpConnect.mockClear()
    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "snapshot-session",
      undefined,
      {},
      42,
      undefined,
      null
    )
  })

  it("resumes the session the BACKEND minted once the entry is gone", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    // A new conversation connects with no sessionId at all — the backend mints
    // one later, and it only ever lands on the store entry.
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", undefined, 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "session_started",
      session_id: "minted-1",
    })
    expect(h.store!.getConnection(TAB)?.sessionId).toBe("minted-1")

    // Whatever removes the entry (backend GC via connection_gone, the idle
    // sweep, the unmount cleanup) leaves only the recorded params behind.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    expect(h.actions!.getReconnectInfo(TAB)?.sessionId).toBe("minted-1")

    h.acpConnect.mockClear()
    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    // Reconnecting on the request AS ISSUED would pass sessionId undefined —
    // a brand-new ACP session, silently abandoning the conversation's history.
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "minted-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
  })
})

// The local entry is always released — a stranded one sends the next connect()
// down its "already connected" fast path onto a possibly-dead session — but a
// teardown that did NOT happen must not be reported as one.
describe("AcpConnectionsProvider disconnect teardown confirmation", () => {
  async function connectOwner() {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
  }

  it("counts an already-gone connection as a real teardown", async () => {
    await connectOwner()
    h.acpDisconnect.mockRejectedValue(new Error("connection not found: abc"))

    let confirmed: boolean | undefined
    await act(async () => {
      confirmed = await h.actions!.disconnect(TAB)
    })

    // Nothing is left running, so there is nothing to warn about.
    expect(confirmed).toBe(true)
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("reports an unconfirmed teardown, and reapplyConfig stops claiming success", async () => {
    await connectOwner()
    // reapplyConfig resumes off the LIVE entry's session, which the backend
    // only supplies here.
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "session_started",
      session_id: "sess-1",
    })
    // A transport blip, not a missing connection: the agent process may still
    // be alive and still holding the OLD config.
    h.acpDisconnect.mockRejectedValue(new Error("request timed out"))
    h.acpConnect.mockResolvedValue("respawned-conn")

    let applied: boolean | undefined
    await act(async () => {
      applied = await h.actions!.reapplyConfig(TAB)
    })

    // Still reconnected — the user is not left stranded...
    expect(h.acpConnect).toHaveBeenLastCalledWith(
      "claude_code",
      "/tmp/x",
      "sess-1",
      undefined,
      {},
      42,
      undefined,
      null
    )
    // ...but the caller must not show an "applied" confirmation for a restart
    // that may have landed right back on the process it meant to replace.
    expect(applied).toBe(false)
  })

  it("confirms an ordinary teardown", async () => {
    await connectOwner()

    let confirmed: boolean | undefined
    await act(async () => {
      confirmed = await h.actions!.disconnect(TAB)
    })

    expect(confirmed).toBe(true)
    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "spawned-conn",
      expect.objectContaining({ origin: "explicit_user" })
    )
  })
})

// The backend dedups connections by (agent, cwd, session), so a connect can
// hand back a connection this client already holds under another contextKey.
describe("AcpConnectionsProvider abandoned connect tears down only what it created", () => {
  const OTHER_TAB = "conv-1-claude_code-99"

  async function connectFirstOwner() {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("live-conn")
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    h.acpDisconnect.mockClear()
  }

  it("spares a REUSED connection the client already owns elsewhere", async () => {
    await connectFirstOwner()
    // A second surface resolves to the SAME backend connection (dedup), and is
    // abandoned before the connect settles — killing it would end the first
    // tab's running turn.
    let resolveConnect: (v: string) => void = () => {}
    h.acpConnect.mockImplementation(
      () =>
        new Promise<string>((res) => {
          resolveConnect = res
        })
    )
    let connectPromise: Promise<void> | undefined
    await act(async () => {
      connectPromise = h.actions!.connect(
        OTHER_TAB,
        "claude_code",
        "/tmp/x",
        "sess-1"
      )
    })
    await act(async () => {
      await h.actions!.disconnect(OTHER_TAB)
    })
    await act(async () => {
      resolveConnect("live-conn")
      await connectPromise
    })

    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("live-conn")
  })

  it("still tears down a connection the abandoned connect spawned itself", async () => {
    await connectFirstOwner()
    let resolveConnect: (v: string) => void = () => {}
    h.acpConnect.mockImplementation(
      () =>
        new Promise<string>((res) => {
          resolveConnect = res
        })
    )
    let connectPromise: Promise<void> | undefined
    await act(async () => {
      connectPromise = h.actions!.connect(
        OTHER_TAB,
        "claude_code",
        "/tmp/other",
        "sess-2"
      )
    })
    await act(async () => {
      await h.actions!.disconnect(OTHER_TAB)
    })
    await act(async () => {
      resolveConnect("fresh-conn")
      await connectPromise
    })

    expect(h.acpDisconnect).toHaveBeenCalledWith(
      "fresh-conn",
      expect.objectContaining({ origin: "abandoned_connect" })
    )
  })
})

describe("AcpConnectionsProvider permission request details", () => {
  it("hydrates a permission request from an existing live tool call input", async () => {
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    const handlers = latestAttachHandlers()
    const rawInput = JSON.stringify({ command: "pnpm test", cwd: "/tmp/x" })

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "call_1",
      title: "Bash",
      kind: "execute",
      status: "pending",
      content: null,
      raw_input: rawInput,
      raw_output: null,
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "permission_request",
      request_id: "req-1",
      tool_call: {
        kind: "execute",
        status: "pending",
        toolCallId: "call_1",
      },
      options: [],
    })

    const permission = h.store!.getConnection(TAB)!.pendingPermission
    expect(parsePermissionToolCall(permission?.tool_call).title).toBe("Bash")
    expect(parsePermissionToolCall(permission?.tool_call).command).toBe(
      "pnpm test"
    )
    expect(parsePermissionToolCall(permission?.tool_call).cwd).toBe("/tmp/x")
  })

  it("backfills an already-open permission request when tool input arrives later", async () => {
    const originalRaf = globalThis.requestAnimationFrame
    const originalCancelRaf = globalThis.cancelAnimationFrame
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0)
      return 1
    })
    vi.stubGlobal("cancelAnimationFrame", () => {})

    try {
      await mountProvider()

      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
      })

      const handlers = latestAttachHandlers()

      emitAcpEvent(handlers, {
        seq: 1,
        connection_id: "spawned-conn",
        type: "permission_request",
        request_id: "req-2",
        tool_call: {
          kind: "execute",
          status: "pending",
          toolCallId: "call_2",
        },
        options: [],
      })

      expect(
        parsePermissionToolCall(
          h.store!.getConnection(TAB)!.pendingPermission?.tool_call
        ).command
      ).toBeNull()

      emitAcpEvent(handlers, {
        seq: 2,
        connection_id: "spawned-conn",
        type: "tool_call_update",
        tool_call_id: "call_2",
        title: "Bash",
        status: "pending",
        content: null,
        raw_input: JSON.stringify({ command: "pnpm build" }),
        raw_output: null,
      })

      expect(
        parsePermissionToolCall(
          h.store!.getConnection(TAB)!.pendingPermission?.tool_call
        ).command
      ).toBe("pnpm build")
    } finally {
      vi.stubGlobal("requestAnimationFrame", originalRaf)
      vi.stubGlobal("cancelAnimationFrame", originalCancelRaf)
    }
  })

  it("hydrates snapshot permission details from active tool call input", async () => {
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })
    const handlers = latestAttachHandlers()
    h.denormalizeSnapshot.mockReturnValue({
      connectionId: "spawned-conn",
      status: "connected",
      sessionId: "sess-1",
      modes: null,
      configOptions: null,
      availableCommands: [],
      usage: null,
      liveMessage: {
        id: "live-1",
        role: "assistant",
        startedAt: 0,
        content: [
          {
            type: "tool_call",
            info: {
              tool_call_id: "call_snapshot",
              title: "Bash",
              kind: "execute",
              status: "pending",
              content: null,
              raw_input: JSON.stringify({
                command: "pnpm test -- --runInBand",
                cwd: "/tmp/x",
              }),
              raw_output_chunks: [],
              raw_output_total_bytes: 0,
              locations: null,
              meta: null,
              images: [],
            },
          },
        ],
      },
      pendingPermission: {
        request_id: "req-snapshot",
        tool_call: {
          kind: "execute",
          status: "pending",
          toolCallId: "call_snapshot",
        },
        options: [],
      },
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: true,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      lastError: null,
      eventSeq: 5,
      activeDelegations: [],
      toolWatchdogProjections: {},
    })
    hydrateSnapshot(handlers, {
      connection_id: "spawned-conn",
      conversation_id: null,
      folder_id: null,
      status: "connected",
      external_id: "sess-1",
      live_message: {
        id: "live-1",
        role: "assistant",
        started_at: new Date(0).toISOString(),
        content: [{ kind: "tool_call_ref", tool_call_id: "call_snapshot" }],
      },
      active_tool_calls: [
        {
          id: "call_snapshot",
          kind: "execute",
          label: "Bash",
          status: "pending",
          input: { command: "pnpm test -- --runInBand", cwd: "/tmp/x" },
          output: null,
          content: null,
          locations: null,
          meta: null,
        },
      ],
      pending_permission: {
        request_id: "req-snapshot",
        tool_call_id: "call_snapshot",
        tool_call: {
          kind: "execute",
          status: "pending",
          toolCallId: "call_snapshot",
        },
        options: [],
        created_at: new Date(0).toISOString(),
      },
      pending_question: null,
      pending_user_message: null,
      active_delegations: [],
      feedback: [],
      feedback_tool_available: false,
      modes: null,
      current_mode: null,
      config_options: null,
      prompt_capabilities: null,
      usage: null,
      fork_supported: false,
      available_commands: [],
      selectors_ready: true,
      config_stale: false,
      config_stale_kind: null,
      event_seq: 5,
    })

    const permission = h.store!.getConnection(TAB)!.pendingPermission
    const parsed = parsePermissionToolCall(permission?.tool_call)
    expect(parsed.title).toBe("Bash")
    expect(parsed.command).toBe("pnpm test -- --runInBand")
    expect(parsed.cwd).toBe("/tmp/x")
  })

  it("clears a pending permission when the turn completes", async () => {
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "permission_request",
      request_id: "req-cancelled",
      tool_call: {
        kind: "execute",
        status: "pending",
        toolCallId: "call-cancelled",
      },
      options: [],
    })
    expect(h.store!.getConnection(TAB)!.pendingPermission).not.toBeNull()

    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sess-1",
      stop_reason: "cancelled",
      mark_awaiting_reply: false,
    })

    expect(h.store!.getConnection(TAB)!.pendingPermission).toBeNull()
  })
})

describe("AcpConnectionsProvider session load failures", () => {
  it("localizes legacy Codex CLI sessions and preserves the recovery code", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "session_load_failed",
      session_id: "sess-1",
      message:
        "This Codex session was created by the legacy CLI runtime and cannot be resumed.",
      code: "legacy_cli_session",
    })

    const connection = h.store!.getConnection(TAB)
    expect(connection?.loadError).toMatch(
      /^backendErrors\.sessionLoadLegacyCliSession/
    )
    expect(connection?.loadErrorCode).toBe("legacy_cli_session")
  })
})

describe("AcpConnectionsProvider route override + conflict", () => {
  it("sends conversationId and route override to acpConnect in exact parameter order", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "codex", "/repo", undefined, 7, "native")
    })
    // Exact order: agentType, workingDir, sessionId, preferredModeId,
    // preferredConfigValues, conversationId, delegationRouteOverride,
    // ownerOperationId (null for main/non-detached).
    expect(h.acpConnect).toHaveBeenCalledWith(
      "codex",
      "/repo",
      undefined,
      undefined,
      {},
      7,
      "native",
      null
    )
    const conn = h.store!.getConnection(TAB)
    expect(conn?.conversationId).toBe(7)
    expect(conn?.delegationRouteOverride).toBe("native")
  })

  it("queued connect retry forwards ownerOperationId", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    let resolveFirst: ((id: string) => void) | undefined
    h.acpConnect.mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          resolveFirst = resolve
        })
    )
    h.acpConnect.mockResolvedValue("spawned-conn-2")
    await mountProvider()

    // First connect holds the in-flight slot.
    let firstDone: Promise<void> | undefined
    await act(async () => {
      firstDone = h.actions!.connect(
        TAB,
        "codex",
        "/repo",
        "sess-a",
        9,
        "native",
        null
      )
    })
    // Superseding detached request is queued while first is connecting.
    await act(async () => {
      void h.actions!.connect(
        TAB,
        "codex",
        "/repo",
        "sess-b",
        9,
        "native",
        "op-detached"
      )
    })
    await act(async () => {
      resolveFirst?.("spawned-conn-1")
      await firstDone
      // Flush queueMicrotask retry.
      await Promise.resolve()
      await Promise.resolve()
    })
    // Allow the queued retry to finish.
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.acpConnect).toHaveBeenCalledTimes(2)
    expect(h.acpConnect).toHaveBeenLastCalledWith(
      "codex",
      "/repo",
      "sess-b",
      undefined,
      {},
      9,
      "native",
      "op-detached"
    )
  })

  it("reapplyConfig disconnects then reconnects with stored boundConversationId + boundRouteOverride", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "codex", "/repo", undefined, 42, "native")
    })
    const conn = h.store!.getConnection(TAB)
    expect(conn?.conversationId).toBe(42)
    expect(conn?.delegationRouteOverride).toBe("native")
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    h.acpConnect.mockClear()
    h.acpDisconnect.mockClear()

    let reapplied = false
    await act(async () => {
      reapplied = await h.actions!.reapplyConfig(TAB)
    })
    expect(reapplied).toBe(true)
    // Explicit disconnect of the live owner process first…
    expect(h.acpDisconnect).toHaveBeenCalledWith("spawned-conn", {
      origin: "config_reapply",
    })
    // …then reconnect reuses the stored conversation id + route override exactly
    // (sessionId is whatever the connection last held — typically from snapshot).
    expect(h.acpConnect).toHaveBeenCalledWith(
      "codex",
      "/repo",
      conn?.sessionId ?? undefined,
      undefined,
      {},
      42,
      "native",
      null
    )
  })

  it("attaches session_route_conflict detail as viewer without disconnect", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockRejectedValue({
      code: "session_route_conflict",
      message: "Session route conflict",
      detail: "existing-conn",
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "codex", "/repo", "sess-1", 42, "codeg")
    })
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.attach).toHaveBeenCalledWith(
      "existing-conn",
      { sinceSeq: undefined },
      expect.anything()
    )
    const conn = h.store!.getConnection(TAB)
    expect(conn?.isViewer).toBe(true)
    expect(conn?.connectionId).toBe("existing-conn")
  })
})

describe("AcpConnectionsProvider structured shell connect errors", () => {
  async function connectAndCatch() {
    await mountProvider()
    await act(async () => {
      // No conversationId → owner spawn via acpConnect.
      try {
        await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
      } catch {
        // connect rethrows after alerting; swallow for assertions.
      }
    })
  }

  it("localizes terminal_shell_unavailable from structured i18n_key", async () => {
    h.acpConnect.mockRejectedValue({
      code: "terminal_shell_unavailable",
      message: "selected terminal shell is unavailable",
      detail: "C:\\missing\\pwsh.exe",
      i18n_key: "backendErrors.terminalShellUnavailable",
      i18n_params: { shell: "PowerShell 7" },
    })

    await connectAndCatch()

    expect(h.pushAlert).toHaveBeenCalled()
    const call = h.pushAlert.mock.calls.find(
      (c) =>
        typeof c[2] === "string" &&
        (c[2] as string).includes("backendErrors.terminalShellUnavailable")
    )
    expect(call).toBeTruthy()
    expect(call![2]).toContain("shell=PowerShell 7")
    // Must not fall back to English message substring matching.
    expect(call![2]).not.toMatch(/selected terminal shell is unavailable/i)
    // Not the SDK-missing branch (no Open Agents settings action payload).
    expect(call![0]).toBe("error")
    expect(String(call![1])).toMatch(/connectFailedTitle/)
  })

  it("localizes terminal_shell_unsupported from structured i18n_key", async () => {
    h.acpConnect.mockRejectedValue({
      code: "terminal_shell_unsupported",
      message: "selected terminal shell is unsupported",
      detail: "C:\\tools\\mystery.exe",
      i18n_key: "backendErrors.terminalShellUnsupported",
      i18n_params: { shell: "mystery.exe" },
    })

    await connectAndCatch()

    expect(h.pushAlert).toHaveBeenCalled()
    const call = h.pushAlert.mock.calls.find(
      (c) =>
        typeof c[2] === "string" &&
        (c[2] as string).includes("backendErrors.terminalShellUnsupported")
    )
    expect(call).toBeTruthy()
    expect(call![2]).toContain("shell=mystery.exe")
    expect(call![2]).not.toMatch(/selected terminal shell is unsupported/i)
    expect(String(call![1])).toMatch(/connectFailedTitle/)
  })

  it("still surfaces SDK-missing alert for legacy install string", async () => {
    h.acpConnect.mockRejectedValue(
      "Codex is not installed. Please install it in Agent Settings."
    )

    await connectAndCatch()

    const call = h.pushAlert.mock.calls.find((c) => {
      const title = typeof c[1] === "string" ? c[1] : ""
      const detail = typeof c[2] === "string" ? c[2] : ""
      return (
        title.includes("blocked.sdkMissing") ||
        title.includes("blocked.adapterMissing") ||
        detail.includes("is not installed") ||
        detail.includes("agentsSetupHint")
      )
    })
    // Debug leftover if this still fails: dump calls.
    expect(call).toBeTruthy()
    expect(String(call![2])).toMatch(/agentsSetupHint/)
    // Open Agent Settings action is attached as 4th arg.
    expect(call![3]).toBeTruthy()
    expect(Array.isArray(call![3])).toBe(true)
    expect((call![3] as unknown[]).length).toBeGreaterThan(0)
  })
})

describe("AcpConnectionsProvider terminal shell config stale", () => {
  it("applies session_config_stale terminal_shell into connection state", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "session_config_stale",
      stale: true,
      kind: "terminal_shell",
    })

    const connection = h.store!.getConnection(TAB)
    expect(connection?.configStale).toBe(true)
    expect(connection?.configStaleKind).toBe("terminal_shell")
    expect(connection?.configStaleDismissed).toBe(false)
  })
})

describe("AcpConnectionsProvider continuation waiting projection", () => {
  const waiting = {
    conversation_id: 42,
    state: "waiting" as const,
    generation: 2,
    armed_at: "2026-01-01T00:00:00.000Z",
    wake_at: "2026-01-01T00:04:00.000Z",
  }

  it("hydrates waitingForSubagents from snapshot without changing status", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    h.denormalizeSnapshot.mockReturnValue({
      connectionId: "spawned-conn",
      status: "connected",
      sessionId: "s1",
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      selectorsReady: true,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      backgroundOutstanding: 0,
      eventSeq: 5,
      activeDelegations: [],
      toolWatchdogProjections: {},
      delegationRoute: null,
      waitingForSubagents: waiting,
    })

    hydrateSnapshot(latestAttachHandlers(), {
      connection_id: "spawned-conn",
      conversation_id: 42,
      folder_id: 1,
      status: "connected",
      external_id: "s1",
      live_message: null,
      active_tool_calls: [],
      pending_permission: null,
      modes: null,
      current_mode: null,
      config_options: null,
      prompt_capabilities: null,
      usage: null,
      fork_supported: false,
      available_commands: [],
      selectors_ready: true,
      event_seq: 5,
      waiting_for_subagents: waiting,
    })

    const connection = h.store!.getConnection(TAB)
    expect(connection?.waitingForSubagents).toEqual(waiting)
    expect(connection?.status).toBe("connected")
  })

  it("applies continuation_waiting_changed live events independently of status", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    // Ensure connected first.
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })

    emitAcpEvent(latestAttachHandlers(), {
      seq: 2,
      connection_id: "spawned-conn",
      type: "continuation_waiting_changed",
      conversation_id: 42,
      waiting,
    })

    const connection = h.store!.getConnection(TAB)
    expect(connection?.waitingForSubagents).toEqual(waiting)
    expect(connection?.status).toBe("connected")

    emitAcpEvent(latestAttachHandlers(), {
      seq: 3,
      connection_id: "spawned-conn",
      type: "continuation_waiting_changed",
      conversation_id: 42,
      waiting: null,
    })
    expect(h.store!.getConnection(TAB)?.waitingForSubagents).toBeNull()
  })

  it("localizes live parent-loss, drain-timeout, and generic continuation errors", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })
    const handlers = latestAttachHandlers()

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "error",
      message: "raw parent lost",
      agent_type: "claude_code",
      code: "parent_connection_lost",
      terminal: false,
    })
    expect(h.pushAlert).toHaveBeenCalled()
    let call = h.pushAlert.mock.calls.at(-1)!
    expect(String(call[2])).toContain("backendErrors.parentConnectionLost")
    expect(String(call[2])).not.toMatch(/raw parent lost/i)

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "error",
      message: "raw drain",
      agent_type: "claude_code",
      code: "suspend_drain_timeout",
      terminal: false,
    })
    call = h.pushAlert.mock.calls.at(-1)!
    expect(String(call[2])).toContain("backendErrors.suspendDrainTimeout")

    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "error",
      message: "raw arm failed",
      agent_type: "claude_code",
      code: "arm_failed",
      terminal: false,
    })
    call = h.pushAlert.mock.calls.at(-1)!
    expect(String(call[2])).toContain("backendErrors.continuationFailed")
  })
})

describe("AcpConnectionsProvider liveMessage sink (mirror out of React)", () => {
  async function connectOwner(): Promise<AttachHandlers> {
    await mountProvider()
    await act(async () => {
      // No conversationId → skip discovery → owner spawn (acpConnect).
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })
    return latestAttachHandlers()
  }

  it("fires with isLive=true and a fresh non-null liveMessage when a turn starts", async () => {
    const handlers = await connectOwner()
    const calls: Array<{ content: unknown; isLive: boolean }> = []
    h.actions!.registerLiveMessageSink(TAB, (lm, isLive) =>
      calls.push({ content: lm.content, isLive })
    )

    // status → prompting resets liveMessage to a fresh empty assistant message.
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })

    expect(calls).toHaveLength(1)
    expect(calls[0]!.isLive).toBe(true)
    expect(calls[0]!.content).toEqual([])
    expect(h.recordFrontendTurnTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        phase: "prompting_frame",
        contextKey: TAB,
        connectionId: "spawned-conn",
        eventSeq: 1,
        receivedAtMs: expect.any(Number),
        elapsedMs: expect.any(Number),
        sinkRegistered: true,
      })
    )
    expect(h.recordFrontendTurnTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        phase: "live_published",
        contextKey: TAB,
        connectionId: "spawned-conn",
        eventSeq: 1,
        canonicalAccepted: true,
        transcriptPublished: false,
      })
    )
  })

  it("relays a subsequent liveMessage change (tool call appended) to the sink", async () => {
    const handlers = await connectOwner()
    const calls: Array<{ len: number; isLive: boolean }> = []
    h.actions!.registerLiveMessageSink(TAB, (lm, isLive) =>
      calls.push({ len: lm.content.length, isLive })
    )

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "call_1",
      title: "Bash",
      kind: "execute",
      status: "pending",
      content: null,
      raw_input: "{}",
      raw_output: null,
    })

    expect(calls.length).toBeGreaterThanOrEqual(2)
    const last = calls[calls.length - 1]!
    expect(last.isLive).toBe(true)
    expect(last.len).toBe(1) // the appended tool_call block
    expect(h.recordFrontendTurnTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        phase: "first_content",
        contextKey: TAB,
        connectionId: "spawned-conn",
        eventSeq: 2,
        receivedAtMs: expect.any(Number),
        elapsedMs: expect.any(Number),
      })
    )
  })

  it("records a plan update as first content", async () => {
    const handlers = await connectOwner()
    h.actions!.registerLiveMessageSink(TAB, () => undefined)

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "plan_update",
      entries: [{ content: "Inspect logs", status: "in_progress" }],
    })

    expect(h.recordFrontendTurnTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        phase: "first_content",
        contextKey: TAB,
        connectionId: "spawned-conn",
        eventSeq: 2,
      })
    )
  })

  it("stops firing after the returned unregister runs", async () => {
    const handlers = await connectOwner()
    let count = 0
    const unregister = h.actions!.registerLiveMessageSink(TAB, () => {
      count += 1
    })

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    expect(count).toBe(1)

    unregister()
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    expect(count).toBe(1) // no further fire
  })

  it("does not fire when a transition leaves liveMessage unchanged", async () => {
    const handlers = await connectOwner()
    let count = 0
    h.actions!.registerLiveMessageSink(TAB, () => {
      count += 1
    })

    // connecting → connected never touches liveMessage (stays null).
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })
    expect(count).toBe(0)
  })

  it("replays the current liveMessage immediately when registering over a live connection", async () => {
    const handlers = await connectOwner()
    // Drive a live message with NO sink registered (e.g. before the panel's
    // registration effect, or a connection reused across a remount).
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "call_1",
      title: "Bash",
      kind: "execute",
      status: "pending",
      content: null,
      raw_input: "{}",
      raw_output: null,
    })

    // Registering now must replay the existing liveMessage once, immediately —
    // otherwise a paused stream (no further delta) would leave the message list
    // blank until the next change.
    const calls: Array<{ len: number; isLive: boolean }> = []
    h.actions!.registerLiveMessageSink(TAB, (lm, isLive) =>
      calls.push({ len: lm.content.length, isLive })
    )
    expect(calls).toHaveLength(1)
    expect(calls[0]!.isLive).toBe(true) // still prompting
    expect(calls[0]!.len).toBe(1) // the tool_call block already present
  })

  it("does not replay a retained completion over another runtime live turn", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "retained reply A",
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sess-1",
      stop_reason: "end_turn",
      mark_awaiting_reply: false,
    })

    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const replyB: LiveMessage = {
      id: "reply-b",
      role: "assistant",
      content: [{ type: "text", text: "live reply B" }],
      startedAt: 1_700_000_000_001,
    }
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.setLiveMessage(42, replyB, true)
    let calls = 0

    try {
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical: (message, isLive) => {
          calls += 1
          runtimeActions.setLiveMessage(42, message, isLive)
          return true
        },
      })

      expect(calls).toBe(0)
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage
      ).toBe(replyB)
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("mirrors to the sink BEFORE notifying connection key subscribers", async () => {
    const handlers = await connectOwner()
    const order: string[] = []
    h.actions!.registerLiveMessageSink(TAB, () => order.push("sink"))
    const unsub = h.store!.subscribeKey(TAB, () => order.push("notify"))

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    unsub()

    // The runtime sink runs before the connection's key subscribers are notified
    // for the liveMessage-changing dispatch. (A benign follow-up dispatch that
    // leaves liveMessage unchanged may append another "notify" without re-firing
    // the sink — assert the ordering + single sink, not the total notify count.)
    expect(order[0]).toBe("sink")
    expect(order.filter((x) => x === "sink")).toHaveLength(1)
    expect(order.indexOf("sink")).toBeLessThan(order.indexOf("notify"))
  })

  it.each([
    { label: "ordinary", terminationSource: null },
    { label: "user-stop", terminationSource: "user_stop" as const },
  ])(
    "keeps settled $label replay content behind the stale reconnect guard",
    async ({ terminationSource }) => {
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
      })
      const handlers = latestAttachHandlers()
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")
      const settled: LiveMessage = {
        id: "settled",
        role: "assistant",
        content: [{ type: "text", text: "already persisted" }],
        startedAt: 1_700_000_000_000,
      }
      runtimeActions.setLiveMessage(42, settled, true)
      runtimeActions.completeTurn(42, settled)
      const originalAssistantId = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
        ?.localTurns.find((turn) => turn.role === "assistant")?.id
      expect(originalAssistantId).toEqual(expect.any(String))
      const { createLiveTranscriptStore, createLiveTranscriptFrameSink } =
        await import("@/stores/live-transcript-store")
      const transcriptStore = createLiveTranscriptStore()
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(
          42,
          "spawned-conn",
          transcriptStore
        ),
      })

      try {
        act(() => {
          handlers.onReplay(
            [
              {
                connection_id: "spawned-conn",
                seq: 1,
                type: "status_changed",
                status: "prompting",
              },
              {
                connection_id: "spawned-conn",
                seq: 2,
                type: "content_delta",
                text: "already persisted",
              },
              {
                connection_id: "spawned-conn",
                seq: 3,
                type: "turn_complete",
                session_id: "sess-1",
                stop_reason: terminationSource ? "cancelled" : "end_turn",
                mark_awaiting_reply: false,
                ...(terminationSource
                  ? {
                      termination_source: terminationSource,
                      provider_turn_id: null,
                    }
                  : {}),
              },
            ],
            3
          )
        })

        expect(
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage
        ).toBeNull()
        expect(
          useConversationRuntimeStore
            .getState()
            .byConversationId.get(42)
            ?.localTurns.filter((turn) => turn.role === "assistant")
            .map((turn) => ({ id: turn.id, blocks: turn.blocks }))
        ).toEqual([
          {
            id: originalAssistantId,
            blocks: [{ type: "text", text: "already persisted" }],
          },
        ])
        expect(
          useConversationRuntimeStore
            .getState()
            .byConversationId.get(42)
            ?.localTurns.find((turn) => turn.role === "assistant")?.outcome
        ).toBeUndefined()
        expect(transcriptStore.getConversation(42)).toBeNull()
        expect(
          h.store!.getConnection(TAB)?.acceptedCompletionMessageId
        ).toBeNull()
        expect(
          h.store!.getConnection(TAB)?.acceptedCompletionRuntimeConversationIds
        ).toBeNull()
      } finally {
        resetConversationRuntimeStore()
      }
    }
  )

  it("keeps a rejected replay settled when turn-complete arrives in a later frame", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    const handlers = latestAttachHandlers()
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    const settled: LiveMessage = {
      id: "settled-split-replay",
      role: "assistant",
      content: [{ type: "text", text: "already persisted" }],
      startedAt: 1_700_000_000_000,
    }
    runtimeActions.setLiveMessage(42, settled, true)
    runtimeActions.completeTurn(42, settled)
    const originalAssistantId = useConversationRuntimeStore
      .getState()
      .byConversationId.get(42)
      ?.localTurns.find((turn) => turn.role === "assistant")?.id
    const { createLiveTranscriptStore, createLiveTranscriptFrameSink } =
      await import("@/stores/live-transcript-store")
    const transcriptStore = createLiveTranscriptStore()
    h.actions!.registerLiveSinks(TAB, {
      runtimeConversationId: 42,
      canonical: (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      },
      transcript: createLiveTranscriptFrameSink(
        42,
        "spawned-conn",
        transcriptStore
      ),
    })

    try {
      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "spawned-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            {
              connection_id: "spawned-conn",
              seq: 2,
              type: "content_delta",
              text: "already persisted",
            },
            {
              connection_id: "spawned-conn",
              seq: 3,
              type: "status_changed",
              status: "connected",
            },
          ],
          3
        )
      })
      expect(transcriptStore.getConversation(42)).toBeNull()

      act(() => {
        handlers.onReplay(
          [
            {
              connection_id: "spawned-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          4
        )
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.liveMessage).toBeNull()
      expect(
        runtime?.localTurns
          .filter((turn) => turn.role === "assistant")
          .map((turn) => ({ id: turn.id, blocks: turn.blocks }))
      ).toEqual([
        {
          id: originalAssistantId,
          blocks: [{ type: "text", text: "already persisted" }],
        },
      ])
      expect(transcriptStore.getConversation(42)).toBeNull()
    } finally {
      resetConversationRuntimeStore()
    }
  })
})

describe("out-of-turn wire guard + background activity", () => {
  async function mountOwnerConnection() {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    return latestAttachHandlers()
  }

  it("drops streaming deltas while the connection is not prompting (Bug-A guard)", async () => {
    const handlers = await mountOwnerConnection()

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })
    // Out-of-turn delta (the backend idle loop forwards these between turns):
    // must NOT graft onto a liveMessage. The next status_changed flushes the
    // streaming queue BEFORE the status dispatch, so the drop is exercised
    // deterministically with the pre-flip status still "connected".
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "out-of-turn garbage",
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    // Prompting resets liveMessage to an empty shell; the dropped delta must
    // not appear in it.
    const afterPrompting = h.store!.getConnection(TAB)
    expect(afterPrompting?.liveMessage?.content ?? []).toEqual([])

    // In-turn delta flows normally (flushed by the next non-streaming event).
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "real reply",
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
    })
    const conn = h.store!.getConnection(TAB)
    expect(conn?.liveMessage?.content).toEqual([
      { type: "text", text: "real reply" },
    ])
  })

  it("routes parented deltas into separate blocks and drops orphans (claude-agent-acp ≥0.63)", async () => {
    const handlers = await mountOwnerConnection()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    // The launching Agent tool call precedes its subagent's chunks on the
    // seq-ordered wire — required by the reducer's parent-presence gate.
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "toolu_parent",
      title: "Agent",
      kind: "other",
      status: "in_progress",
      content: null,
      raw_input: null,
      raw_output: null,
    })
    // main → sub → main within ONE flush window: the queue pre-coalescing
    // must not concatenate across attributions, and the reducer must produce
    // three separate text blocks.
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "main ",
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "sub report",
      parent_tool_use_id: "toolu_parent",
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "main tail",
    })
    // Orphan: no such tool call in liveMessage → dropped entirely.
    emitAcpEvent(handlers, {
      seq: 6,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "orphan noise",
      parent_tool_use_id: "toolu_unknown",
    })
    // Parented thinking lands as its own attributed block.
    emitAcpEvent(handlers, {
      seq: 7,
      connection_id: "spawned-conn",
      type: "thinking",
      text: "sub reasoning",
      parent_tool_use_id: "toolu_parent",
    })
    // Non-streaming event flushes the queue deterministically.
    emitAcpEvent(handlers, {
      seq: 8,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
    })

    const conn = h.store!.getConnection(TAB)
    const content = conn?.liveMessage?.content ?? []
    const rendered = content.map((b) =>
      b.type === "text" || b.type === "thinking"
        ? { type: b.type, text: b.text, parent: b.parentToolUseId ?? null }
        : { type: b.type }
    )
    expect(rendered).toEqual([
      { type: "tool_call" },
      { type: "text", text: "main ", parent: null },
      { type: "text", text: "sub report", parent: "toolu_parent" },
      { type: "text", text: "main tail", parent: null },
      { type: "thinking", text: "sub reasoning", parent: "toolu_parent" },
    ])
  })

  it("merges consecutive same-parent deltas into one growing block", async () => {
    const handlers = await mountOwnerConnection()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_call",
      tool_call_id: "toolu_parent",
      title: "Agent",
      kind: "other",
      status: "in_progress",
      content: null,
      raw_input: null,
      raw_output: null,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "part one, ",
      parent_tool_use_id: "toolu_parent",
    })
    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "part two",
      parent_tool_use_id: "toolu_parent",
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
    })
    const content = h.store!.getConnection(TAB)?.liveMessage?.content ?? []
    const texts = content.filter((b) => b.type === "text")
    expect(texts).toHaveLength(1)
    expect(texts[0]).toMatchObject({
      text: "part one, part two",
      parentToolUseId: "toolu_parent",
    })
  })

  it("background_activity mirrors outstanding, applies overlay turns, and notifies settled tasks", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const { sendSystemNotification } = await import("@/lib/notification")
    const notify = vi.mocked(sendSystemNotification)
    notify.mockClear()
    const { getFolderConversation } = await import("@/lib/api")
    vi.mocked(getFolderConversation).mockClear()
    resetConversationRuntimeStore()
    // Bind the agent session id to a runtime conversation so the overlay
    // bridge can resolve it. Model the draft-started shape (the common QA
    // flow): the runtime session key is a virtual NEGATIVE id and the real
    // DB row id (42) is bound separately — the settle refetch must fetch
    // with 42, not the virtual key (which the backend would reject,
    // silently leaving the launch card frozen on its ack).
    const VIRTUAL = -9
    useConversationRuntimeStore
      .getState()
      .actions.setExternalId(VIRTUAL, "sess-1")
    useConversationRuntimeStore
      .getState()
      .actions.setDbConversationId(VIRTUAL, 42)

    const handlers = await mountOwnerConnection()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-1",
      turns: [
        {
          id: "bg-100-0",
          role: "assistant",
          blocks: [{ type: "text", text: "build finished cleanly" }],
          timestamp: "2026-07-07T03:47:08.000Z",
        },
      ],
      outstanding: 2,
      settled: [
        {
          task_id: "agent1",
          status: "completed",
          summary: 'Agent "Run pnpm build" finished',
          tool_use_id: "toolu_01",
          result: "Build succeeded (exit code 0).",
        },
      ],
      watermark: 4096,
    })

    // 1. outstanding mirrored onto the connection (sweep exemption + chip);
    //    the settlement arms the "syncing results" bridge state (the agent's
    //    reaction turn is being generated).
    expect(h.store!.getConnection(TAB)?.backgroundOutstanding).toBe(2)
    expect(h.store!.getConnection(TAB)?.backgroundSettleSyncingSince).toEqual(
      expect.any(Number)
    )

    // 2. overlay turn upserted into the runtime session — under the RUNTIME
    //    key (that's the session the panel renders).
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(VIRTUAL)
    expect(session?.backgroundTurns).toHaveLength(1)
    expect(session?.backgroundTurns[0]).toMatchObject({
      watermark: 4096,
      turn: { id: "bg-100-0" },
    })

    // 3. one OS notification per settled task, carrying its summary.
    expect(notify).toHaveBeenCalledTimes(1)
    expect(notify.mock.calls[0][0]).toBe("x - DrawCode")
    expect(notify.mock.calls[0][1]).toContain('Agent "Run pnpm build" finished')

    // 4. the settlement flips the launch card IN-MEMORY (no detail refetch):
    //    with no promoted card yet (it's mid-stream), it's queued under the
    //    runtime key by `tool_use_id` for COMPLETE_TURN to apply.
    expect(vi.mocked(getFolderConversation)).not.toHaveBeenCalled()
    expect(session?.pendingBackgroundSettlements).toEqual([
      {
        toolUseId: "toolu_01",
        taskId: "agent1",
        status: "completed",
        summary: 'Agent "Run pnpm build" finished',
        result: "Build succeeded (exit code 0).",
      },
    ])

    // Accounting-only follow-up (work settles to zero): mirror updates, no
    // duplicate overlay entries, no extra notification.
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-1",
      outstanding: 0,
      watermark: 4200,
    })
    expect(h.store!.getConnection(TAB)?.backgroundOutstanding).toBe(0)
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(VIRTUAL)
        ?.backgroundTurns
    ).toHaveLength(1)
    expect(notify).toHaveBeenCalledTimes(1)
    // Accounting-only events keep the syncing bridge armed — the reaction
    // turn hasn't surfaced yet.
    expect(h.store!.getConnection(TAB)?.backgroundSettleSyncingSince).toEqual(
      expect.any(Number)
    )

    // The reaction turn arriving (turns-only event) disarms the bridge.
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-1",
      turns: [
        {
          id: "bg-100-1",
          role: "assistant",
          blocks: [{ type: "text", text: "here is what the build produced" }],
          timestamp: "2026-07-07T03:47:12.000Z",
        },
      ],
      outstanding: 0,
      watermark: 4400,
    })
    expect(h.store!.getConnection(TAB)?.backgroundSettleSyncingSince).toBeNull()

    resetConversationRuntimeStore()
  })

  it("terminal background_activity requests a detail refetch with preserveLive when idle", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()

    const VIRTUAL = -10
    const actions = useConversationRuntimeStore.getState().actions
    actions.setExternalId(VIRTUAL, "sess-terminal")
    actions.setDbConversationId(VIRTUAL, 43)
    const refetchDetail = vi
      .spyOn(actions, "refetchDetail")
      .mockImplementation(() => {})

    const handlers = await mountOwnerConnection()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-terminal",
      turns: [
        {
          id: "grok-autonomous:terminal:assistant:0",
          role: "assistant",
          blocks: [{ type: "text", text: "terminal persisted reply" }],
          timestamp: "2026-08-19T00:00:00.000Z",
        },
      ],
      outstanding: 0,
      watermark: 512,
      detail_refetch: true,
    })

    expect(refetchDetail).toHaveBeenCalledWith(VIRTUAL, {
      preserveLive: true,
    })
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(VIRTUAL)
        ?.backgroundTurns
    ).toMatchObject([
      {
        watermark: 512,
        turn: { id: "grok-autonomous:terminal:assistant:0" },
      },
    ])

    refetchDetail.mockRestore()
    resetConversationRuntimeStore()
  })

  it("hard-cap overlay eviction bypasses an earlier fold-refetch throttle", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()

    const VIRTUAL = -12
    const actions = useConversationRuntimeStore.getState().actions
    actions.setExternalId(VIRTUAL, "sess-overlay-eviction")
    actions.setDbConversationId(VIRTUAL, 45)
    const refetchDetail = vi
      .spyOn(actions, "refetchDetail")
      .mockImplementation(() => {})
    const handlers = await mountOwnerConnection()
    const turns = (start: number, count: number) =>
      Array.from({ length: count }, (_, offset) => {
        const index = start + offset
        return {
          id: `grok-autonomous:bg-${index}:assistant:0`,
          role: "assistant" as const,
          blocks: [{ type: "text" as const, text: `reply ${index}` }],
          timestamp: "2026-08-19T00:00:00.000Z",
        }
      })

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-overlay-eviction",
      turns: turns(0, 61),
      outstanding: 0,
      watermark: 1000,
    })
    expect(refetchDetail).toHaveBeenCalledTimes(1)

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-overlay-eviction",
      turns: turns(61, 240),
      outstanding: 0,
      watermark: 2000,
    })

    const entries = useConversationRuntimeStore
      .getState()
      .byConversationId.get(VIRTUAL)?.backgroundTurns
    expect(entries).toHaveLength(300)
    expect(entries?.[0]?.turn.id).toBe("grok-autonomous:bg-1:assistant:0")
    expect(refetchDetail).toHaveBeenCalledTimes(2)

    refetchDetail.mockRestore()
    resetConversationRuntimeStore()
  })

  it("hard-cap overlay eviction supersedes an in-flight detail refetch", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()

    const VIRTUAL = -14
    const actions = useConversationRuntimeStore.getState().actions
    actions.setExternalId(VIRTUAL, "sess-overlay-in-flight")
    actions.setDbConversationId(VIRTUAL, 47)
    const turns = (start: number, count: number) =>
      Array.from({ length: count }, (_, offset) => {
        const index = start + offset
        return {
          id: `grok-autonomous:in-flight-${index}:assistant:0`,
          role: "assistant" as const,
          blocks: [{ type: "text" as const, text: `reply ${index}` }],
          timestamp: "2026-08-19T00:00:00.000Z",
        }
      })
    actions.applyBackgroundActivity(VIRTUAL, turns(0, 300), 1000)
    useConversationRuntimeStore.setState((state) => {
      const current = state.byConversationId.get(VIRTUAL)!
      const byConversationId = new Map(state.byConversationId)
      byConversationId.set(VIRTUAL, { ...current, detailLoading: true })
      return { byConversationId }
    })
    const refetchDetail = vi
      .spyOn(actions, "refetchDetail")
      .mockImplementation(() => {})
    const handlers = await mountOwnerConnection()

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-overlay-in-flight",
      turns: turns(300, 1),
      outstanding: 0,
      watermark: 2000,
    })

    const entries = useConversationRuntimeStore
      .getState()
      .byConversationId.get(VIRTUAL)?.backgroundTurns
    expect(entries).toHaveLength(300)
    expect(entries?.[0]?.turn.id).toBe(
      "grok-autonomous:in-flight-1:assistant:0"
    )
    expect(refetchDetail).toHaveBeenCalledTimes(1)

    refetchDetail.mockRestore()
    resetConversationRuntimeStore()
  })

  it("transcript reset clears overlays even when background_activity has no turns", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()

    const VIRTUAL = -11
    const actions = useConversationRuntimeStore.getState().actions
    actions.setExternalId(VIRTUAL, "sess-reset")
    actions.setDbConversationId(VIRTUAL, 44)
    actions.applyBackgroundActivity(
      VIRTUAL,
      [
        {
          id: "grok-autonomous:old:assistant:0",
          role: "assistant",
          blocks: [{ type: "text", text: "old generation" }],
          timestamp: "2026-08-19T00:00:00.000Z",
        },
      ],
      4096
    )
    const refetchDetail = vi
      .spyOn(actions, "refetchDetail")
      .mockImplementation(() => {})

    const handlers = await mountOwnerConnection()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-reset",
      outstanding: 0,
      watermark: 32,
      detail_refetch: true,
      transcript_reset: true,
    })

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(VIRTUAL)
        ?.backgroundTurns
    ).toEqual([])
    expect(refetchDetail).toHaveBeenCalledWith(VIRTUAL, {
      preserveLive: true,
    })

    refetchDetail.mockRestore()
    resetConversationRuntimeStore()
  })

  it("snapshot recovery clears an old transcript generation and refetches detail", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()

    const VIRTUAL = -13
    const actions = useConversationRuntimeStore.getState().actions
    actions.setExternalId(VIRTUAL, "sess-snapshot-recovery")
    actions.setDbConversationId(VIRTUAL, 46)
    actions.applyBackgroundActivity(
      VIRTUAL,
      [
        {
          id: "grok-autonomous:old-generation:assistant:0",
          role: "assistant",
          blocks: [{ type: "text", text: "old generation" }],
          timestamp: "2026-08-19T00:00:00.000Z",
        },
      ],
      4096
    )
    const refetchDetail = vi
      .spyOn(actions, "refetchDetail")
      .mockImplementation(() => {})
    const handlers = await mountOwnerConnection()
    h.denormalizeSnapshot.mockReturnValueOnce({
      ...h.denormalizeSnapshot(),
      connectionId: "spawned-conn",
      sessionId: "sess-snapshot-recovery",
      eventSeq: 5,
      backgroundDetailRevision: 1,
      backgroundTranscriptGeneration: 1,
    })

    hydrateSnapshot(handlers, {
      event_seq: 5,
    } as unknown as LiveSessionSnapshot)

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(VIRTUAL)
        ?.backgroundTurns
    ).toEqual([])
    expect(refetchDetail).toHaveBeenCalledWith(VIRTUAL, {
      preserveLive: true,
    })

    refetchDetail.mockRestore()
    resetConversationRuntimeStore()
  })

  it("does NOT arm the syncing-results hint for a wire-visible (#870-held) settle", async () => {
    const { resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const handlers = await mountOwnerConnection()

    // #870: the launching turn is held OPEN and the sub-agent's reply streams
    // live as the tail of that held turn — the backend marks the settle
    // `wire_visible: true`. There is no "results not yet visible" gap, so the
    // hint must stay hidden (not strand on "Syncing background results…" until
    // the 30s cap). Gated on the backend flag, NOT the connection status, so it
    // holds even if this event is delivered after the turn returns to connected.
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "background_activity",
      session_id: "sess-1",
      outstanding: 0,
      settled: [
        {
          task_id: "agent1",
          status: "completed",
          tool_use_id: "toolu_01",
          result: "done",
          wire_visible: true,
        },
      ],
      watermark: 100,
    })

    expect(h.store!.getConnection(TAB)?.backgroundOutstanding).toBe(0)
    expect(h.store!.getConnection(TAB)?.backgroundSettleSyncingSince).toBeNull()

    resetConversationRuntimeStore()
  })
})

describe("AcpConnectionsProvider Grok cross-agent-type model switch", () => {
  function grokModelOptions(current: string): SessionConfigOptionInfo[] {
    return [
      {
        id: "model",
        name: "Model",
        category: "model",
        kind: {
          type: "select",
          current_value: current,
          options: [
            { value: "grok-4.5", name: "Grok 4.5" },
            { value: "grok-composer-2.5-fast", name: "Composer 2.5" },
          ],
          groups: [],
        },
      },
    ]
  }

  async function connectGrokOwner(): Promise<AttachHandlers> {
    h.acpGetAgentStatus.mockResolvedValue({
      agent_type: "grok",
      enabled: true,
      available: true,
      installed_version: "0.2.103",
      host_tools_agent_mode: false,
      is_acp_adapter: false,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "grok", "/tmp/x", "sess-1")
    })
    return latestAttachHandlers()
  }

  it("reverts the optimistic pick, surfaces the localized error, and keeps the attempted preference", async () => {
    const handlers = await connectGrokOwner()

    // Composer selector arrives with grok-4.5 active.
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "session_config_options",
      config_options: grokModelOptions("grok-4.5"),
    })
    expect(
      h.store!.getConnection(TAB)!.configOptions?.[0]?.kind.current_value
    ).toBe("grok-4.5")

    // User optimistically switches to the cross-agent-type Composer model.
    vi.mocked(saveConfigPreference).mockClear()
    await act(async () => {
      await h.actions!.setConfigOption(TAB, "model", "grok-composer-2.5-fast")
    })
    // Optimistic: the selector shows the pick and the preference is persisted.
    expect(
      h.store!.getConnection(TAB)!.configOptions?.[0]?.kind.current_value
    ).toBe("grok-composer-2.5-fast")
    expect(saveConfigPreference).toHaveBeenCalledTimes(1)
    expect(saveConfigPreference).toHaveBeenCalledWith(
      "grok",
      "model",
      "grok-composer-2.5-fast"
    )

    // Backend rejects the switch mid-conversation: it re-emits the authoritative
    // options (revert) followed by the coded, recoverable error.
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "session_config_options",
      config_options: grokModelOptions("grok-4.5"),
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "error",
      message: "Cannot switch to that model in an existing conversation.",
      agent_type: "grok",
      code: "grok_model_switch_incompatible_agent",
      terminal: false,
    })

    const conn = h.store!.getConnection(TAB)!
    // The selector snapped back to the model actually in effect.
    expect(conn.configOptions?.[0]?.kind.current_value).toBe("grok-4.5")
    // The coded error is localized (the useTranslations mock echoes the key) —
    // NOT the raw fallback message.
    expect(conn.error).toMatch(
      /^backendErrors\.grokModelSwitchIncompatibleAgent/
    )
    // The attempted model stays the saved preference (no revert of the persisted
    // choice), so a fresh session lands on Composer where the switch succeeds.
    expect(saveConfigPreference).toHaveBeenCalledTimes(1)
  })
})

describe("HYDRATE_FROM_SNAPSHOT last_error recovery", () => {
  function snapshotPatch(overrides: {
    eventSeq: number
    lastError: string | null
    connectionId?: string
  }) {
    return {
      connectionId: "spawned-conn",
      conversationId: null as number | null,
      status: "connected" as const,
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      backgroundOutstanding: 0,
      activeDelegations: [],
      toolWatchdogProjections: {},
      delegationRoute: null,
      waitingForSubagents: null,
      ...overrides,
    }
  }

  async function connectOwner(): Promise<AttachHandlers> {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpGetAgentStatus.mockResolvedValue({
      agent_type: "claude_code",
      enabled: true,
      available: true,
      installed_version: "1.0.0",
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    return latestAttachHandlers()
  }

  it("recovers last_error from a fresh snapshot", async () => {
    const handlers = await connectOwner()
    h.denormalizeSnapshot.mockReturnValue(
      snapshotPatch({ eventSeq: 5, lastError: "boom from snapshot" })
    )
    hydrateSnapshot(handlers, {
      event_seq: 5,
    } as unknown as LiveSessionSnapshot)
    expect(h.store!.getConnection(TAB)!.error).toBe("boom from snapshot")
  })

  it("does not resurrect a cleared error from a stale snapshot", async () => {
    const handlers = await connectOwner()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "error",
      message: "boom",
      agent_type: "claude_code",
      code: "runtime_failure",
      terminal: false,
    })
    expect(h.store!.getConnection(TAB)!.error).toBe("boom")
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    expect(h.store!.getConnection(TAB)!.error).toBeNull()

    h.denormalizeSnapshot.mockReturnValue(
      snapshotPatch({ eventSeq: 1, lastError: "boom" })
    )
    hydrateSnapshot(handlers, {
      event_seq: 1,
    } as unknown as LiveSessionSnapshot)
    expect(h.store!.getConnection(TAB)!.error).toBeNull()
  })
})

// ── Task 7: one store transaction per browser frame ──

function batch(
  batch_id: number,
  events: EventEnvelope[]
): DesktopAcpEventBatch {
  return { batch_id, events }
}

function content(
  connectionId: string,
  seq: number,
  text: string
): EventEnvelope {
  return {
    connection_id: connectionId,
    seq,
    type: "content_delta",
    text,
  }
}

function thinking(
  connectionId: string,
  seq: number,
  text: string
): EventEnvelope {
  return {
    connection_id: connectionId,
    seq,
    type: "thinking",
    text,
  }
}

describe("AcpConnectionsProvider frame transactions (raw order)", () => {
  it("publishes one store transaction and one live sink for a 200-event frame", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")

    render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
        ])
      )
      h.runAnimationFrame()
    })

    const sink = vi.fn()
    h.actions!.registerLiveMessageSink(TAB, sink)
    sink.mockClear()
    const notify = vi.fn()
    const unsubscribe = h.store!.subscribeKey(TAB, notify)
    __resetPublishedConnectionMapsCount()

    act(() => {
      h.emitDesktopBatch(
        batch(
          2,
          Array.from({ length: 200 }, (_, index) =>
            content("owner-conn", index + 2, "x")
          )
        )
      )
      h.runAnimationFrame()
    })

    expect(h.publishedConnectionMaps()).toBe(1)
    expect(sink).toHaveBeenCalledTimes(1)
    expect(notify).toHaveBeenCalledTimes(1)
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(201)
    expect(h.store!.getConnection(TAB)?.liveMessage?.content[0]).toMatchObject({
      type: "text",
      text: "x".repeat(200),
    })
    unsubscribe()
  })

  it("maps a retry rollback envelope before applying replacement content", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")

    render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "grok", "/tmp/x", "sess-1")
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
          content("owner-conn", 2, "old"),
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "turn_attempt_rollback",
            attempt: 1,
          },
          content("owner-conn", 4, "accepted"),
        ])
      )
      h.runAnimationFrame()
    })

    expect(h.store!.getConnection(TAB)?.liveMessage?.content).toEqual([
      { type: "text", text: "accepted" },
    ])
  })

  it("publishes turn_complete-only frames and marks transcript completing", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")

    const { createLiveTranscriptStore, createLiveTranscriptFrameSink } =
      await import("@/stores/live-transcript-store")
    const transcriptStore = createLiveTranscriptStore()
    const baseSink = createLiveTranscriptFrameSink(
      42,
      "owner-conn",
      transcriptStore
    )
    const publish = vi.fn(
      (
        frame: Parameters<typeof baseSink.publish>[0],
        canonical: Parameters<typeof baseSink.publish>[1]
      ) => baseSink.publish(frame, canonical)
    )

    render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    h.actions!.registerLiveSinks(TAB, {
      canonical: vi.fn(),
      transcript: {
        rebuild: baseSink.rebuild,
        publish,
        markCompleting: baseSink.markCompleting,
        clear: baseSink.clear,
      },
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
          content("owner-conn", 2, "hello"),
        ])
      )
      h.runAnimationFrame()
    })

    const liveMessage = h.store!.getConnection(TAB)?.liveMessage
    expect(liveMessage).toBeTruthy()
    publish.mockClear()

    // turn_complete alone leaves liveMessage reference unchanged; transcript
    // publish must still run so status can flip to completing.
    act(() => {
      h.emitDesktopBatch(
        batch(2, [
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "turn_complete",
            session_id: "sess-1",
            stop_reason: "end_turn",
            mark_awaiting_reply: false,
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(publish).toHaveBeenCalledTimes(1)
    const publishedFrame = publish.mock.calls[0]![0]
    expect(
      publishedFrame.applyEvents.map((e: { type: string }) => e.type)
    ).toEqual(["turn_complete"])
    expect(h.store!.getConnection(TAB)?.liveMessage).toBe(liveMessage)
    expect(transcriptStore.getConversation(42)?.status).toBe("completing")
  })

  it("accepts a terminal when the runtime holds the same message id in an older object", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-older-ref",
        role: "user",
        blocks: [{ type: "text", text: "finish this" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "user-older-ref"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      const sinks = {
        runtimeConversationId: 42,
        canonical: (message: LiveMessage, isLive: boolean) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
      }
      h.actions!.registerLiveSinks(TAB, sinks)

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "complete reply"),
          ])
        )
        h.runAnimationFrame()
      })

      const canonical = h.store!.getConnection(TAB)?.liveMessage
      expect(canonical).toBeTruthy()
      const olderRuntimeObject: LiveMessage = {
        ...canonical!,
        content: [...canonical!.content],
      }
      expect(olderRuntimeObject).not.toBe(canonical)
      runtimeActions.setLiveMessage(42, olderRuntimeObject, true)

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(h.store!.getConnection(TAB)?.status).toBe("connected")
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.localTurns.at(-1)).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "complete reply" }],
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("removes pending-cleanup runtime state only after an admitted terminal", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-cleanup",
        role: "user",
        blocks: [{ type: "text", text: "close tab" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "user-cleanup"
    )
    runtimeActions.setPendingCleanup(42, true)

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      const sinks = {
        runtimeConversationId: 42,
        canonical: (message: LiveMessage, isLive: boolean) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
      }
      h.actions!.registerLiveSinks(TAB, sinks)

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "closed-tab reply"),
          ])
        )
        h.runAnimationFrame()
      })

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "other-session",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })
      expect(
        useConversationRuntimeStore.getState().byConversationId.has(42)
      ).toBe(true)
      expect(h.store!.getConnection(TAB)?.status).toBe("prompting")

      act(() => {
        h.emitDesktopBatch(
          batch(3, [
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.has(42)
      ).toBe(false)
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("adopts an accepted completion while a cold reopen detail is loading", async () => {
    const { getFolderConversation } = await import("@/lib/api")
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-reopen",
        role: "user",
        blocks: [{ type: "text", text: "close and reopen" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "user-reopen"
    )
    runtimeActions.setPendingCleanup(42, true)

    let resolveDetail!: (detail: DbConversationDetail) => void
    vi.mocked(getFolderConversation).mockImplementationOnce(
      () =>
        new Promise<DbConversationDetail>((resolve) => {
          resolveDetail = resolve
        })
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "retained final reply"),
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.has(42)
      ).toBe(false)
      const completedConnection = h.store!.getConnection(TAB)
      expect(completedConnection?.acceptedCompletionMessageId).toBe(
        completedConnection?.liveMessage?.id
      )
      expect(
        completedConnection?.acceptedCompletionRuntimeConversationIds
      ).toContain(42)

      runtimeActions.fetchDetail(42)
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.detailLoading
      ).toBe(true)
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
      })

      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.at(-1)
      ).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "retained final reply" }],
      })

      await act(async () => {
        resolveDetail({
          summary: {
            id: 42,
            folder_id: 1,
            agent_type: "claude_code",
            title: "reopened",
            title_locked: false,
            auto_title_finalized: false,
            status: "in_progress",
            awaiting_reply_token: null,
            kind: "regular",
            model: null,
            git_branch: null,
            external_id: "sess-1",
            message_count: 1,
            child_count: 0,
            created_at: "2026-08-25T07:31:49.000Z",
            updated_at: "2026-08-25T07:31:49.000Z",
            pinned_at: null,
          },
          turns: [
            {
              id: "user-reopen",
              role: "user",
              blocks: [{ type: "text", text: "close and reopen" }],
              timestamp: "2026-08-25T07:31:49.000Z",
            },
          ],
          session_stats: null,
        })
        await Promise.resolve()
      })

      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.at(-1)
      ).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "retained final reply" }],
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("adopts an accepted completion after a cold reopen rekeys the owner", async () => {
    const orphanKey = "new-reopen-owner"
    const { getFolderConversation } = await import("@/lib/api")
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-rekey-reopen",
        role: "user",
        blocks: [{ type: "text", text: "close before completion" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "user-rekey-reopen"
    )
    runtimeActions.setPendingCleanup(42, true)
    let resolveDetail!: (detail: DbConversationDetail) => void
    vi.mocked(getFolderConversation).mockImplementationOnce(
      () =>
        new Promise<DbConversationDetail>((resolve) => {
          resolveDetail = resolve
        })
    )

    try {
      await mountDesktopOwner("owner-conn", orphanKey, "sess-1", 42)
      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "session_started",
              session_id: "sess-1",
            },
            {
              connection_id: "owner-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 3, "rekeyed retained reply"),
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.has(42)
      ).toBe(false)
      runtimeActions.fetchDetail(42)
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.detailLoading
      ).toBe(true)

      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
      })
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.at(-1)
      ).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "rekeyed retained reply" }],
      })
      await act(async () => {
        resolveDetail({
          summary: {
            id: 42,
            folder_id: 1,
            agent_type: "claude_code",
            title: "reopened after draft",
            title_locked: false,
            auto_title_finalized: false,
            status: "in_progress",
            awaiting_reply_token: null,
            kind: "regular",
            model: null,
            git_branch: null,
            external_id: "sess-1",
            message_count: 1,
            child_count: 0,
            created_at: "2026-08-25T07:31:49.000Z",
            updated_at: "2026-08-25T07:31:49.000Z",
            pinned_at: null,
          },
          turns: [
            {
              id: "user-rekey-reopen",
              role: "user",
              blocks: [{ type: "text", text: "close before completion" }],
              timestamp: "2026-08-25T07:31:49.000Z",
            },
          ],
          session_stats: null,
        })
        await Promise.resolve()
      })
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.at(-1)
      ).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "rekeyed retained reply" }],
      })
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
      })

      expect(h.store!.getConnection(orphanKey)).toBeUndefined()
      expect(h.store!.getConnection(TAB)?.acceptedCompletionMessageId).toBe(
        h.store!.getConnection(TAB)?.liveMessage?.id
      )
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
          ?.localTurns.at(-1)
      ).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "rekeyed retained reply" }],
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("publishes final content as live when turn_complete shares its RAF frame", async () => {
    await mountDesktopOwner()
    const sink = vi.fn()
    h.actions!.registerLiveMessageSink(TAB, sink)

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
        ])
      )
      h.runAnimationFrame()
    })
    sink.mockClear()

    act(() => {
      h.emitDesktopBatch(
        batch(2, [
          content("owner-conn", 2, "final answer"),
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "turn_complete",
            session_id: "sess-1",
            stop_reason: "end_turn",
            mark_awaiting_reply: false,
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(sink).toHaveBeenCalledTimes(1)
    expect(sink.mock.calls[0]![0].content).toEqual([
      { type: "text", text: "final answer" },
    ])
    expect(sink.mock.calls[0]![1]).toBe(true)
  })

  it("promotes a coalesced completed turn through its virtual runtime alias", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const runtimeConversationId = -42
    runtimeActions.setExternalId(runtimeConversationId, "sess-1")
    runtimeActions.setDbConversationId(runtimeConversationId, 42)
    runtimeActions.appendOptimisticTurn(
      runtimeConversationId,
      {
        id: "user-1",
        role: "user",
        blocks: [{ type: "text", text: "fix it" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-1"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "user_message",
              message_id: "user-1",
              blocks: [{ type: "text", text: "fix it" }],
            },
            {
              connection_id: "owner-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 3, "final answer"),
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(runtimeConversationId)
      expect(runtime?.syncState).toBe("idle")
      expect(runtime?.optimisticTurns).toEqual([])
      expect(
        runtime?.localTurns.map((turn) => ({
          role: turn.role,
          blocks: turn.blocks,
        }))
      ).toEqual([
        { role: "user", blocks: [{ type: "text", text: "fix it" }] },
        {
          role: "assistant",
          blocks: [{ type: "text", text: "final answer" }],
        },
      ])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("does not retain a rejected durable alias for cold completion adoption", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const virtualConversationId = -42
    runtimeActions.setExternalId(virtualConversationId, "sess-1")
    runtimeActions.setDbConversationId(virtualConversationId, 42)
    runtimeActions.appendOptimisticTurn(
      virtualConversationId,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "owner prompt A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "user-a"
    )
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-b",
        role: "user",
        blocks: [{ type: "text", text: "other prompt B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "user-b"
    )
    const otherLive: LiveMessage = {
      id: "reply-b",
      role: "assistant",
      content: [{ type: "text", text: "other reply B" }],
      startedAt: 1_700_000_000_000,
    }
    runtimeActions.setLiveMessage(42, otherLive, true)

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      const unregister = h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: virtualConversationId,
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(virtualConversationId, message, isLive)
          return (
            useConversationRuntimeStore
              .getState()
              .byConversationId.get(virtualConversationId)?.liveMessage ===
            message
          )
        },
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "user_message",
              message_id: "user-a",
              blocks: [{ type: "text", text: "owner prompt A" }],
            },
            {
              connection_id: "owner-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 3, "owner reply A"),
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const completedConnection = h.store!.getConnection(TAB)
      expect(
        completedConnection?.acceptedCompletionRuntimeConversationIds
      ).toEqual([virtualConversationId])
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage
      ).toBe(otherLive)

      unregister()
      runtimeActions.removeConversation(42)
      runtimeActions.setExternalId(42, "sess-1")
      const canonical = vi.fn((message: LiveMessage, isLive: boolean) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return true
      })
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical,
      })

      expect(canonical).not.toHaveBeenCalled()
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.localTurns
      ).toEqual([])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("orders a coalesced viewer user message before its completed reply", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")

    function ViewerRuntimeProbe() {
      const acpActions = useAcpActions()
      useEffect(
        () =>
          acpActions.registerLiveMessageSink(
            TAB,
            (message, isLive, deliveryIds) => {
              runtimeActions.setLiveMessage(42, message, isLive, deliveryIds)
            }
          ),
        [acpActions]
      )
      useAcpEvent((event) => {
        if (event.type !== "user_message") return
        const turn: import("@/lib/types").MessageTurn = {
          id: event.message_id,
          role: "user",
          blocks: event.blocks.map((block) =>
            block.type === "image"
              ? {
                  type: "image",
                  data: block.data,
                  mime_type: block.mime_type,
                  uri: null,
                }
              : { type: "text", text: block.text }
          ),
          timestamp: "2026-08-25T07:31:49.000Z",
        }
        runtimeActions.appendViewerUserTurn(42, turn)
      })
      return null
    }

    h.eventStreamValue = null
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "viewer-conn",
      event_seq: 0,
    })
    render(
      <AcpConnectionsProvider>
        <Probe />
        <ViewerRuntimeProbe />
      </AcpConnectionsProvider>
    )

    try {
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
      })
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
      })
      expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "viewer-conn",
              seq: 1,
              type: "user_message",
              message_id: "viewer-user-1",
              blocks: [{ type: "text", text: "check this" }],
            },
            {
              connection_id: "viewer-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("viewer-conn", 3, "viewer final answer"),
            {
              connection_id: "viewer-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns).toEqual([])
      expect(
        runtime?.localTurns.map((turn) => ({
          id: turn.id,
          role: turn.role,
          blocks: turn.blocks,
        }))
      ).toEqual([
        {
          id: "viewer-user-1",
          role: "user",
          blocks: [{ type: "text", text: "check this" }],
        },
        {
          id: expect.any(String),
          role: "assistant",
          blocks: [{ type: "text", text: "viewer final answer" }],
        },
      ])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("orders a coalesced viewer user message before its user-stop reply", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    noteUserStopTurnOwnership(42)

    function ViewerRuntimeProbe() {
      const acpActions = useAcpActions()
      useEffect(
        () =>
          acpActions.registerLiveSinks(TAB, {
            runtimeConversationId: 42,
            canonical: (message, isLive, deliveryIds) => {
              runtimeActions.setLiveMessage(42, message, isLive, deliveryIds)
              return (
                useConversationRuntimeStore.getState().byConversationId.get(42)
                  ?.liveMessage === message
              )
            },
          }),
        [acpActions]
      )
      useAcpEvent((event) => {
        if (event.type !== "user_message") return
        runtimeActions.appendViewerUserTurn(42, {
          id: event.message_id,
          role: "user",
          blocks: event.blocks.map((block) =>
            block.type === "image"
              ? {
                  type: "image",
                  data: block.data,
                  mime_type: block.mime_type,
                  uri: null,
                }
              : { type: "text", text: block.text }
          ),
          timestamp: "2026-08-25T07:31:49.000Z",
        })
      })
      return null
    }

    h.eventStreamValue = null
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "viewer-conn",
      event_seq: 0,
    })
    render(
      <AcpConnectionsProvider>
        <Probe />
        <ViewerRuntimeProbe />
      </AcpConnectionsProvider>
    )

    try {
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
      })
      await act(async () => {
        await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "viewer-conn",
              seq: 1,
              type: "user_message",
              message_id: "viewer-user-stop",
              blocks: [{ type: "text", text: "cancel this" }],
            },
            {
              connection_id: "viewer-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("viewer-conn", 3, "partial answer"),
            {
              connection_id: "viewer-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: null,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(
        runtime?.localTurns.map((turn) => ({
          id: turn.id,
          role: turn.role,
          blocks: turn.blocks,
          outcome: turn.outcome,
        }))
      ).toEqual([
        {
          id: "viewer-user-stop",
          role: "user",
          blocks: [{ type: "text", text: "cancel this" }],
          outcome: undefined,
        },
        {
          id: expect.any(String),
          role: "assistant",
          blocks: [{ type: "text", text: "partial answer" }],
          outcome: {
            status: "interrupted",
            stop_reason: "cancelled",
            source: "user_stop",
            provider_turn_id: null,
          },
        },
      ])
      expect(runtime?.optimisticTurns).toEqual([])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("promotes an earlier completion without pairing it with a later turn", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-a"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "answer A"),
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 5, "answer B in progress"),
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(
        runtime?.localTurns.map((turn) => ({
          role: turn.role,
          blocks: turn.blocks,
        }))
      ).toEqual([
        { role: "user", blocks: [{ type: "text", text: "turn A" }] },
        { role: "assistant", blocks: [{ type: "text", text: "answer A" }] },
      ])
      expect(runtime?.optimisticTurns).toEqual([])
      expect(runtime?.syncState).toBe("idle")
      expect(h.store!.getConnection(TAB)?.liveMessage?.content).toEqual([
        { type: "text", text: "answer B in progress" },
      ])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("uses the last of two terminal completions in one frame", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-a"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
      })
      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "answer A"),
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })
      runtimeActions.appendOptimisticTurn(
        42,
        {
          id: "user-b",
          role: "user",
          blocks: [{ type: "text", text: "turn B" }],
          timestamp: "2026-08-25T07:31:50.000Z",
        },
        "turn-b"
      )

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
            {
              connection_id: "owner-conn",
              seq: 5,
              type: "user_message",
              message_id: "user-b",
              blocks: [{ type: "text", text: "turn B" }],
            },
            {
              connection_id: "owner-conn",
              seq: 6,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 7, "answer B"),
            {
              connection_id: "owner-conn",
              seq: 8,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(
        runtime?.localTurns.map((turn) => ({
          role: turn.role,
          blocks: turn.blocks,
        }))
      ).toEqual([
        { role: "user", blocks: [{ type: "text", text: "turn A" }] },
        { role: "assistant", blocks: [{ type: "text", text: "answer A" }] },
        { role: "user", blocks: [{ type: "text", text: "turn B" }] },
        { role: "assistant", blocks: [{ type: "text", text: "answer B" }] },
      ])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("promotes two previously unprocessed turn boundaries in one frame", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-a"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "answer A"),
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "user_message",
              message_id: "user-b",
              blocks: [{ type: "text", text: "turn B" }],
            },
            {
              connection_id: "owner-conn",
              seq: 5,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 6, "answer B"),
            {
              connection_id: "owner-conn",
              seq: 7,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(
        runtime?.localTurns.map((turn) => ({
          role: turn.role,
          blocks: turn.blocks,
        }))
      ).toEqual([
        { role: "user", blocks: [{ type: "text", text: "turn A" }] },
        { role: "assistant", blocks: [{ type: "text", text: "answer A" }] },
        { role: "user", blocks: [{ type: "text", text: "turn B" }] },
        { role: "assistant", blocks: [{ type: "text", text: "answer B" }] },
      ])
      expect(runtime?.optimisticTurns).toEqual([])
      expect(runtime?.liveMessage).toBeNull()
      const completedConnection = h.store!.getConnection(TAB)
      expect(completedConnection?.liveMessage).toEqual(
        expect.objectContaining({ id: expect.any(String) })
      )
      expect(completedConnection?.acceptedCompletionMessageId).toBe(
        completedConnection?.liveMessage?.id
      )
      expect(
        completedConnection?.acceptedCompletionRuntimeConversationIds
      ).toEqual([42])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("does not authorize retained completion without connection session identity", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const rawSeqs: number[] = []
    function RawProbe() {
      useAcpEvent((event) => rawSeqs.push(event.seq))
      return null
    }
    resetConversationRuntimeStore()
    useAppWorkspaceStore
      .getState()
      .applyConversationUpsert(
        makeSummary({ id: 42, external_id: "expected-session" })
      )

    try {
      h.eventStreamValue = null
      h.acpConnect.mockResolvedValue("owner-conn")
      await mountProvider(<RawProbe />)
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
        await h.actions!.connect(TAB, "claude_code", "/work", undefined, 42)
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "reply with unknown session"),
          ])
        )
        h.runAnimationFrame()
      })
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        sessionId: null,
      })

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "other-session",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        lastAppliedSeq: 3,
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
      expect(rawSeqs).toContain(3)

      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "expected-session")
      h.actions!.registerLiveSinks(TAB, {
        runtimeConversationId: 42,
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return true
        },
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.localTurns).toEqual([])
      expect(runtime?.liveMessage?.content).toEqual([
        { type: "text", text: "reply with unknown session" },
      ])
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("does not complete a runtime from another session", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const { createLiveTranscriptFrameSink, liveTranscriptStore } =
      await import("@/stores/live-transcript-store")
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const rawSeqs: number[] = []
    function RawProbe() {
      useAcpEvent((event) => rawSeqs.push(event.seq))
      return null
    }
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-1",
        role: "user",
        blocks: [{ type: "text", text: "keep this in flight" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-1"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42, <RawProbe />)
      h.actions!.registerLiveSinks(TAB, {
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(42, "owner-conn"),
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "session_started",
              session_id: "sess-1",
            },
            {
              connection_id: "owner-conn",
              seq: 2,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 3, "wrong-session reply"),
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "turn_complete",
              session_id: "other-session",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.localTurns).toEqual([])
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "user-1",
      ])
      expect(runtime?.liveMessage?.content).toEqual([
        { type: "text", text: "wrong-session reply" },
      ])
      expect(runtime?.syncState).toBe("awaiting_persist")
      expect(liveTranscriptStore.getConversation(42)?.status).toBe("streaming")
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        lastAppliedSeq: 4,
        acceptedCompletionMessageId: null,
        acceptedCompletionRuntimeConversationIds: null,
      })
      expect(rawSeqs).toContain(4)

      act(() => {
        h.emitDesktopBatch(
          batch(2, [content("owner-conn", 5, " + still live")])
        )
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage?.content
      ).toEqual([{ type: "text", text: "wrong-session reply + still live" }])
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        lastAppliedSeq: 5,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("rejects a terminal when an exact-live runtime owns another session", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const rawSeqs: number[] = []
    function RawProbe() {
      useAcpEvent((event) => rawSeqs.push(event.seq))
      return null
    }
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "owned-session")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-1",
        role: "user",
        blocks: [{ type: "text", text: "keep this in flight" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-1"
    )

    try {
      await mountDesktopOwner(
        "owner-conn",
        TAB,
        "requested-session",
        42,
        <RawProbe />
      )
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "owned reply"),
          ])
        )
        h.runAnimationFrame()
      })

      const connectionLive = h.store!.getConnection(TAB)?.liveMessage
      expect(h.store!.getConnection(TAB)?.sessionId).toBeNull()
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage
      ).toBe(connectionLive)

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "other-session",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ])
        )
        h.runAnimationFrame()
      })

      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        lastAppliedSeq: 3,
      })
      expect(rawSeqs).toContain(3)

      act(() => {
        h.emitDesktopBatch(
          batch(3, [content("owner-conn", 4, " + still live")])
        )
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage?.content
      ).toEqual([{ type: "text", text: "owned reply + still live" }])
      expect(h.store!.getConnection(TAB)?.status).toBe("prompting")
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("does not let a delayed user-stop completion consume the next turn", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    const { createLiveTranscriptFrameSink, liveTranscriptStore } =
      await import("@/stores/live-transcript-store")
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const rawSeqs: number[] = []
    function RawProbe() {
      useAcpEvent((event) => rawSeqs.push(event.seq))
      return null
    }
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-a"
    )
    const replyA: LiveMessage = {
      id: "reply-a",
      role: "assistant",
      content: [{ type: "text", text: "partial A" }],
      startedAt: 1_700_000_000_000,
    }
    runtimeActions.setLiveMessage(42, replyA, true)
    noteUserStopTurnOwnership(42)
    runtimeActions.completeTurn(42, replyA)
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-b",
        role: "user",
        blocks: [{ type: "text", text: "turn B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "turn-b"
    )

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42, <RawProbe />)
      h.actions!.registerLiveSinks(TAB, {
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(42, "owner-conn"),
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "reply B in progress"),
          ])
        )
        h.runAnimationFrame()
      })
      const liveB = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)?.liveMessage
      expect(liveB?.content).toEqual([
        { type: "text", text: "reply B in progress" },
      ])
      expect(liveTranscriptStore.getConversation(42)?.status).toBe("streaming")

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: "provider-a",
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.activeTurnToken).toBe("turn-b")
      expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "user-b",
      ])
      expect(runtime?.liveMessage).toBe(liveB)
      expect(
        runtime?.localTurns
          .filter((turn) => turn.role === "assistant")
          .map((turn) => turn.blocks)
      ).toEqual([[{ type: "text", text: "partial A" }]])
      expect(liveTranscriptStore.getConversation(42)?.status).toBe("streaming")
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        lastAppliedSeq: 3,
      })
      expect(rawSeqs).toContain(3)

      act(() => {
        h.emitDesktopBatch(batch(3, [content("owner-conn", 4, " + tail")]))
        h.runAnimationFrame()
      })

      expect(
        useConversationRuntimeStore.getState().byConversationId.get(42)
          ?.liveMessage?.content
      ).toEqual([{ type: "text", text: "reply B in progress + tail" }])
      expect(h.store!.getConnection(TAB)).toMatchObject({
        status: "prompting",
        lastAppliedSeq: 4,
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("promotes an accepted user-stop completion through its ownership fence", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    const { createLiveTranscriptFrameSink, liveTranscriptStore } =
      await import("@/stores/live-transcript-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "cancel turn A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-a"
    )
    noteUserStopTurnOwnership(42)

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      h.actions!.registerLiveSinks(TAB, {
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(42, "owner-conn"),
      })
      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "cancelled final content"),
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: null,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(runtime?.optimisticTurns).toEqual([])
      expect(runtime?.liveMessage).toBeNull()
      expect(runtime?.localTurns.at(-1)).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "cancelled final content" }],
        outcome: { source: "user_stop", stop_reason: "cancelled" },
      })
      expect(liveTranscriptStore.getConversation(42)).toBeNull()
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("does not apply user-stop completion to a same-session runtime with another live turn", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    for (const [conversationId, userId, text, token] of [
      [42, "user-owner", "owner prompt", "turn-owner"],
      [43, "user-other", "other prompt", "turn-other"],
    ] as const) {
      runtimeActions.setExternalId(conversationId, "sess-1")
      runtimeActions.appendOptimisticTurn(
        conversationId,
        {
          id: userId,
          role: "user",
          blocks: [{ type: "text", text }],
          timestamp: "2026-08-25T07:31:49.000Z",
        },
        token
      )
    }
    const otherLive: LiveMessage = {
      id: "reply-other",
      role: "assistant",
      content: [{ type: "text", text: "other reply" }],
      startedAt: 1_700_000_000_000,
    }
    runtimeActions.setLiveMessage(43, otherLive, true)

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "owner reply"),
          ])
        )
        h.runAnimationFrame()
      })
      noteUserStopTurnOwnership(42)
      noteUserStopTurnOwnership(43)

      act(() => {
        h.emitDesktopBatch(
          batch(2, [
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: null,
            },
          ])
        )
        h.runAnimationFrame()
      })

      const owner = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      const other = useConversationRuntimeStore
        .getState()
        .byConversationId.get(43)
      expect(owner?.localTurns.at(-1)).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "owner reply" }],
        outcome: { source: "user_stop", stop_reason: "cancelled" },
      })
      expect(other?.localTurns).toEqual([])
      expect(other?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "user-other",
      ])
      expect(other?.liveMessage).toBe(otherLive)
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("completes a user-stop step before mirroring the next turn in the same frame", async () => {
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    const { createLiveTranscriptFrameSink, liveTranscriptStore } =
      await import("@/stores/live-transcript-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-1")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "cancel A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "turn-a"
    )
    noteUserStopTurnOwnership(42)

    try {
      await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
      h.actions!.registerLiveSinks(TAB, {
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(42, "owner-conn"),
      })

      act(() => {
        h.emitDesktopBatch(
          batch(1, [
            {
              connection_id: "owner-conn",
              seq: 1,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 2, "cancelled reply A"),
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: null,
            },
            {
              connection_id: "owner-conn",
              seq: 4,
              type: "status_changed",
              status: "prompting",
            },
            content("owner-conn", 5, "reply B in progress"),
          ])
        )
        h.runAnimationFrame()
      })

      const runtime = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      expect(
        runtime?.localTurns.map((turn) => ({
          role: turn.role,
          blocks: turn.blocks,
          outcome: turn.outcome,
        }))
      ).toEqual([
        {
          role: "user",
          blocks: [{ type: "text", text: "cancel A" }],
          outcome: undefined,
        },
        {
          role: "assistant",
          blocks: [{ type: "text", text: "cancelled reply A" }],
          outcome: {
            status: "interrupted",
            stop_reason: "cancelled",
            source: "user_stop",
            provider_turn_id: null,
          },
        },
      ])
      expect(runtime?.liveMessage?.content).toEqual([
        { type: "text", text: "reply B in progress" },
      ])
      expect(h.store!.getConnection(TAB)?.status).toBe("prompting")
      expect(liveTranscriptStore.getConversation(42)).toMatchObject({
        status: "streaming",
      })
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("raw subscribers run after commit in original envelope order", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")

    const seen: Array<{ seq: number; cursor: number }> = []
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")

    function RawProbe() {
      useAcpEvent((event) => {
        const conn = h.store?.getConnection(TAB)
        seen.push({
          seq: event.seq,
          cursor: conn?.lastAppliedSeq ?? -1,
        })
      })
      return null
    }

    render(
      <AcpConnectionsProvider>
        <Probe />
        <RawProbe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
        ])
      )
      h.runAnimationFrame()
    })
    seen.length = 0

    act(() => {
      h.emitDesktopBatch(
        batch(9, [
          content("owner-conn", 2, "a"),
          thinking("owner-conn", 3, "b"),
        ])
      )
      h.runAnimationFrame()
    })

    // After commit, cursor is highest applied seq (3); both raw callbacks see it.
    expect(seen).toEqual([
      { seq: 2, cursor: 3 },
      { seq: 3, cursor: 3 },
    ])
  })

  it("raw useAcpEvent subscriber receives error terminal:true unchanged", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")

    const seen: Array<{ type: string; terminal?: boolean }> = []
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")

    function RawProbe() {
      useAcpEvent((event) => {
        if (event.type === "error") {
          seen.push({ type: event.type, terminal: event.terminal })
        } else {
          seen.push({ type: event.type })
        }
      })
      return null
    }

    render(
      <AcpConnectionsProvider>
        <Probe />
        <RawProbe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "error",
            message: "agent exited",
            agent_type: "codex",
            code: "process_exited",
            terminal: true,
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(seen).toEqual([{ type: "error", terminal: true }])
  })

  it("unknown event advances cursor, reaches raw subscribers, logs only type", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")
    const seen: number[] = []
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})

    function RawProbe() {
      useAcpEvent((event) => {
        seen.push(event.seq)
      })
      return null
    }

    render(
      <AcpConnectionsProvider>
        <Probe />
        <RawProbe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            // @ts-expect-error intentional unknown wire type
            type: "future_extension_event",
            secret_payload: "must-not-log",
          } as EventEnvelope,
        ])
      )
      h.runAnimationFrame()
    })

    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(1)
    expect(seen).toEqual([1])
    expect(warn).toHaveBeenCalledWith("[acp-context] unknown ACP event type", {
      type: "future_extension_event",
    })
    const logged = JSON.stringify(warn.mock.calls)
    expect(logged).not.toContain("secret_payload")
    expect(logged).not.toContain("must-not-log")
    warn.mockRestore()
  })

  it("delegation operational no-store events fan out without unknown warnings", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")
    const seen: string[] = []
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    const { emptyRuntimeStats } = await import("@/lib/types")

    function RawProbe() {
      useAcpEvent((event) => {
        seen.push(event.type)
      })
      return null
    }

    render(
      <AcpConnectionsProvider>
        <Probe />
        <RawProbe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    const startedAt = "2026-07-19T00:00:00.000Z"
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "delegation_observation_changed",
            parent_tool_use_id: "pt-1",
            task_id: "task-1",
            observation: "active",
            last_agent_activity_at: startedAt,
          },
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "delegation_runtime_stats_changed",
            parent_tool_use_id: "pt-1",
            task_id: "task-1",
            runtime_stats: emptyRuntimeStats(startedAt),
          },
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "delegation_attention_changed",
            parent_tool_use_id: "pt-1",
            task_id: "task-1",
            attention_request: null,
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(seen).toEqual([
      "delegation_observation_changed",
      "delegation_runtime_stats_changed",
      "delegation_attention_changed",
    ])
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(3)
    // Must not warn as unknown — raw subscribers still receive them.
    expect(warn).not.toHaveBeenCalledWith(
      "[acp-context] unknown ACP event type",
      expect.anything()
    )
    // availability_changed stays a store-mutating path (not part of this set).
    const logged = JSON.stringify(warn.mock.calls)
    expect(logged).not.toContain("delegation_observation_changed")
    expect(logged).not.toContain("delegation_runtime_stats_changed")
    expect(logged).not.toContain("delegation_attention_changed")
    warn.mockRestore()
  })

  it("one raw subscriber throwing does not stop later subscribers", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")
    const second = vi.fn()
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {})

    function RawProbes() {
      useAcpEvent(() => {
        throw new Error("boom")
      })
      useAcpEvent(second)
      return null
    }

    render(
      <AcpConnectionsProvider>
        <Probe />
        <RawProbes />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "connected",
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(second).toHaveBeenCalled()
    errSpy.mockRestore()
  })

  it("runtime failure never starts the legacy listener", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")
    const { subscribeDesktopAcpEvents } =
      await import("@/lib/transport/desktop-acp-events")

    render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    const callsBefore = vi.mocked(subscribeDesktopAcpEvents).mock.calls.length

    act(() => {
      h.emitDesktopFailure({
        generation: 1,
        reason: "batch_emit_failed",
        affected: [{ connection_id: "owner-conn", first_seq: 1, last_seq: 3 }],
      })
    })

    // Failure must not re-subscribe (no hot-switch / no legacy acp://event).
    expect(vi.mocked(subscribeDesktopAcpEvents).mock.calls.length).toBe(
      callsBefore
    )
    expect(h.pushAlert).toHaveBeenCalled()
  })

  async function mountDesktopOwner(
    connectionId = "owner-conn",
    contextKey = TAB,
    sessionId = "sess-1",
    conversationId?: number,
    children?: ReactNode
  ) {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue(connectionId)
    render(
      <AcpConnectionsProvider>
        <Probe />
        {children}
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(
        contextKey,
        "claude_code",
        "/tmp/x",
        sessionId,
        conversationId
      )
    })
  }

  it("attributes first content to the event that creates it in a mixed frame", async () => {
    await mountDesktopOwner()
    h.actions!.registerLiveMessageSink(TAB, () => undefined)

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "plan_update",
            entries: [],
          },
          content("owner-conn", 3, "first output"),
        ])
      )
      h.runAnimationFrame()
    })

    expect(h.recordFrontendTurnTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        phase: "first_content",
        eventSeq: 3,
      })
    )
  })

  it("applies control-event order in one batch after one RAF", async () => {
    const seenTypes: string[] = []
    const { useAcpEvent } = await import("@/contexts/acp-connections-context")

    function RawProbe() {
      useAcpEvent((event) => {
        seenTypes.push(event.type)
      })
      return null
    }

    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")
    render(
      <AcpConnectionsProvider>
        <Probe />
        <RawProbe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "permission_request",
            request_id: "req-1",
            tool_call: {
              kind: "execute",
              status: "pending",
              toolCallId: "call-1",
            },
            options: [],
          },
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "permission_resolved",
            request_id: "req-1",
          },
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "question_request",
            question_id: "q-1",
            questions: [
              {
                id: "q1",
                question: "Pick one?",
                header: "Q",
                multi_select: false,
                options: [{ label: "A", description: "" }],
              },
            ],
          },
          {
            connection_id: "owner-conn",
            seq: 4,
            type: "question_resolved",
            question_id: "q-1",
          },
          {
            connection_id: "owner-conn",
            seq: 5,
            type: "error",
            message: "turn blew up",
            agent_type: "claude_code",
            code: null,
            terminal: false,
          },
          {
            connection_id: "owner-conn",
            seq: 6,
            type: "status_changed",
            status: "prompting",
          },
          {
            connection_id: "owner-conn",
            seq: 7,
            type: "turn_complete",
            session_id: "sess-1",
            stop_reason: "end_turn",
            mark_awaiting_reply: false,
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(seenTypes).toEqual([
      "permission_request",
      "permission_resolved",
      "question_request",
      "question_resolved",
      "error",
      "status_changed",
      "turn_complete",
    ])
    const conn = h.store!.getConnection(TAB)!
    // Final transitions after ordered apply in one frame:
    // request→resolved cleared permission/question; status_changed(prompting)
    // clears the error set by the prior error event; turn_complete → connected.
    expect(conn.pendingPermission).toBeNull()
    expect(conn.pendingAskQuestion).toBeNull()
    expect(conn.error).toBeNull()
    expect(conn.status).toBe("connected")
    expect(conn.lastAppliedSeq).toBe(7)
    // Error afterCommit still fired (before status cleared the field).
    expect(h.pushAlert).toHaveBeenCalled()
    expect(h.pushAlert.mock.calls[0]?.slice(0, 3)).toEqual([
      "error",
      "§eventErrorTitle",
      "turn blew up",
    ])
  })

  it("applies plan approval and retrying events through the frame ingestor", async () => {
    await mountDesktopOwner()

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "session_started",
            session_id: "sess-1",
          },
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "plan_approval_request",
            approval_id: "approval-1",
            tool_call_id: "tool-1",
            plan_markdown: "## Plan\n\nShip it.",
          },
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "turn_retrying",
            message: "rate limited",
            error_status: 429,
          },
        ])
      )
      h.runAnimationFrame()
    })

    const active = h.store!.getConnection(TAB)!
    expect(active.pendingPlanApproval).toMatchObject({
      approval_id: "approval-1",
      tool_call_id: "tool-1",
      plan_markdown: "## Plan\n\nShip it.",
    })
    expect(active.claudeApiRetry).toEqual({
      sessionId: "sess-1",
      attempt: null,
      maxRetries: null,
      error: "rate limited",
      errorStatus: 429,
      retryDelayMs: null,
    })
    expect(active.lastAppliedSeq).toBe(3)

    act(() => {
      h.emitDesktopBatch(
        batch(2, [
          {
            connection_id: "owner-conn",
            seq: 4,
            type: "plan_approval_resolved",
            approval_id: "approval-1",
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(h.store!.getConnection(TAB)?.pendingPlanApproval).toBeNull()
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(4)
  })

  it("concatenates raw_output_append chunks in order after one frame", async () => {
    await mountDesktopOwner()
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "tool_call",
            tool_call_id: "tool-1",
            title: "Bash",
            kind: "execute",
            status: "in_progress",
            content: null,
            raw_input: null,
            raw_output: null,
          },
          {
            connection_id: "owner-conn",
            seq: 3,
            type: "tool_call_update",
            tool_call_id: "tool-1",
            title: null,
            status: null,
            content: null,
            raw_input: null,
            raw_output: "hello ",
            raw_output_append: true,
          },
          {
            connection_id: "owner-conn",
            seq: 4,
            type: "tool_call_update",
            tool_call_id: "tool-1",
            title: null,
            status: null,
            content: null,
            raw_input: null,
            raw_output: "world",
            raw_output_append: true,
          },
          {
            connection_id: "owner-conn",
            seq: 5,
            type: "tool_call_update",
            tool_call_id: "tool-1",
            title: null,
            status: "completed",
            content: null,
            raw_input: null,
            raw_output: "!",
            raw_output_append: true,
          },
        ])
      )
      h.runAnimationFrame()
    })

    const tool = h
      .store!.getConnection(TAB)!
      .liveMessage?.content.find((b) => b.type === "tool_call")
    expect(tool?.type).toBe("tool_call")
    if (tool?.type !== "tool_call") throw new Error("expected tool_call")
    expect(tool.info.raw_output_chunks).toEqual(["hello ", "world", "!"])
    expect(tool.info.raw_output_total_bytes).toBe("hello world!".length)
    expect(tool.info.status).toBe("completed")
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(5)
  })

  it("rekeys between receipt and commit and applies once under the new key", async () => {
    const ORPHAN = "new-orphan-tab"
    await mountDesktopOwner("owner-conn", ORPHAN, "sess-shared")

    // Orphan rescue matches on sessionId — seed it before the rekey race.
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "session_started",
            session_id: "sess-shared",
          },
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "status_changed",
            status: "prompting",
          },
        ])
      )
      h.runAnimationFrame()
    })
    expect(h.store!.getConnection(ORPHAN)?.sessionId).toBe("sess-shared")

    act(() => {
      h.emitDesktopBatch(
        batch(2, [content("owner-conn", 3, "a"), content("owner-conn", 4, "b")])
      )
    })
    // Frame is scheduled but not run — rekey via orphan rescue first.
    expect(h.rafQueue.length).toBeGreaterThan(0)

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-shared")
    })

    expect(h.store!.getConnection(ORPHAN)).toBeUndefined()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("owner-conn")

    act(() => {
      h.runAnimationFrame()
    })

    expect(h.store!.getConnection(ORPHAN)).toBeUndefined()
    const conn = h.store!.getConnection(TAB)!
    expect(conn.lastAppliedSeq).toBe(4)
    expect(conn.liveMessage?.content[0]).toMatchObject({
      type: "text",
      text: "ab",
    })
  })

  it("buffers unmapped events, hydrates, then drains without duplicates", async () => {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue("owner-conn")
    let resolveSnapshot: (value: unknown) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSnapshot = resolve
        })
    )
    h.denormalizeSnapshot.mockImplementation(
      (snap: { connection_id: string; event_seq: number }) => ({
        connectionId: snap.connection_id,
        eventSeq: snap.event_seq,
        activeDelegations: [],
        toolWatchdogProjections: {},
        status: "prompting",
        sessionId: "sess-1",
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: {
          id: "snap-lm",
          role: "assistant",
          content: [{ type: "text", text: "from-snapshot" }],
          startedAt: 1,
        },
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        backgroundOutstanding: 0,
      })
    )

    render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    let connectPromise: Promise<void> | undefined
    await act(async () => {
      connectPromise = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sess-1"
      )
    })
    // Wait until CONNECTION_CREATED (acpConnect resolved) but reverseMap is
    // still unset while snapshot is in flight.
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("owner-conn")

    const publish = vi.fn()
    h.actions!.registerLiveSinks(TAB, {
      canonical: () => true,
      transcript: {
        rebuild: vi.fn(),
        publish,
        markCompleting: vi.fn(),
        clear: vi.fn(),
      },
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          content("owner-conn", 1, "dup-a"),
          content("owner-conn", 2, "dup-b"),
          content("owner-conn", 3, "only-live"),
        ])
      )
      h.runAnimationFrame()
    })
    // Unmapped: cursor must not advance from the firehose yet.
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(0)

    await act(async () => {
      resolveSnapshot({ connection_id: "owner-conn", event_seq: 2 })
      await connectPromise
    })
    // Drain may flush immediately; run any residual frame.
    act(() => {
      h.runAnimationFrame()
    })

    const conn = h.store!.getConnection(TAB)!
    expect(conn.lastAppliedSeq).toBe(3)
    // Snapshot text + only the post-cursor live delta (no duplicate 1/2).
    const texts = (conn.liveMessage?.content ?? [])
      .filter((b): b is { type: "text"; text: string } => b.type === "text")
      .map((b) => b.text)
    expect(texts.join("")).toContain("from-snapshot")
    expect(texts.join("")).toContain("only-live")
    expect(texts.join("")).not.toContain("dup-a")
    expect(texts.join("")).not.toContain("dup-b")
    const drainedFrame = publish.mock.calls.at(-1)?.[0]
    expect(drainedFrame?.deliverySource).toBe("desktop")
    expect(drainedFrame?.eventDeliverySourceBySeq?.get(3)).toBe("desktop")
  })

  it("snapshot race mid-queue drops old seq and applies contiguous suffix", async () => {
    await mountDesktopOwner()
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
          content("owner-conn", 2, "w"),
          content("owner-conn", 3, "x"),
        ])
      )
      h.runAnimationFrame()
    })
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(3)

    h.denormalizeSnapshot.mockReturnValue({
      connectionId: "owner-conn",
      eventSeq: 5,
      activeDelegations: [],
      toolWatchdogProjections: {},
      status: "prompting",
      sessionId: "sess-1",
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: {
        id: "lm-race",
        role: "assistant",
        content: [{ type: "text", text: "snap-5" }],
        startedAt: 1,
      },
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      backgroundOutstanding: 0,
    })
    h.acpGetSessionSnapshot.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 5,
    })

    // Gap at 5 (missing 4) pauses the connection; 5-7 stay buffered.
    act(() => {
      h.emitDesktopBatch(
        batch(2, [
          content("owner-conn", 5, "old"),
          content("owner-conn", 6, "g"),
          content("owner-conn", 7, "h"),
        ])
      )
      h.runAnimationFrame()
    })
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(3)

    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    act(() => {
      h.runAnimationFrame()
    })

    const conn = h.store!.getConnection(TAB)!
    // Hydrate cursor 5 drops seq 5; contiguous 6-7 apply.
    expect(conn.lastAppliedSeq).toBe(7)
    const text = (conn.liveMessage?.content ?? [])
      .filter((b): b is { type: "text"; text: string } => b.type === "text")
      .map((b) => b.text)
      .join("")
    expect(text).toContain("snap-5")
    expect(text).toContain("g")
    expect(text).toContain("h")
    expect(text).not.toContain("old")
  })

  it("multi-connection gap on A does not block B in the same batch", async () => {
    const TAB_A = "tab-a-claude"
    const TAB_B = "tab-b-claude"
    h.eventStreamValue = null
    let connectN = 0
    h.acpConnect.mockImplementation(async () => {
      connectN += 1
      return connectN === 1 ? "conn-a" : "conn-b"
    })
    // Connect path: no snapshot. Gap recovery: hydrate A to seq 5.
    h.acpGetSessionSnapshot.mockResolvedValue(null)
    h.denormalizeSnapshot.mockImplementation(
      (snap: { connection_id: string; event_seq: number }) => ({
        connectionId: snap.connection_id,
        eventSeq: snap.event_seq,
        activeDelegations: [],
        toolWatchdogProjections: {},
        status: "prompting",
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: {
          id: "lm",
          role: "assistant",
          content: [],
          startedAt: 1,
        },
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        backgroundOutstanding: 0,
      })
    )

    render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      await h.actions!.connect(TAB_A, "claude_code", "/tmp/x", "sess-a")
    })
    await act(async () => {
      await h.actions!.connect(TAB_B, "claude_code", "/tmp/x", "sess-b")
    })

    // Seed A cursor to 3 so a jump to 5 is a gap (missing 4).
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "conn-a",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
          content("conn-a", 2, "a"),
          content("conn-a", 3, "b"),
          {
            connection_id: "conn-b",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
        ])
      )
      h.runAnimationFrame()
    })
    expect(h.store!.getConnection(TAB_A)?.lastAppliedSeq).toBe(3)
    expect(h.store!.getConnection(TAB_B)?.lastAppliedSeq).toBe(1)

    // From here, gap recovery for A returns a snapshot at seq 5.
    h.acpGetSessionSnapshot.mockImplementation(async (id: string) => {
      if (id === "conn-a") {
        return { connection_id: "conn-a", event_seq: 5 }
      }
      return null
    })

    act(() => {
      h.emitDesktopBatch(
        batch(2, [
          content("conn-a", 5, "gap"), // missing 4
          content("conn-b", 2, "x"),
          content("conn-b", 3, "y"),
        ])
      )
      h.runAnimationFrame()
    })

    // B commits contiguous work immediately; A stays at 3 pending recovery.
    expect(h.store!.getConnection(TAB_B)?.lastAppliedSeq).toBe(3)
    expect(
      h.store!.getConnection(TAB_B)?.liveMessage?.content[0]
    ).toMatchObject({
      type: "text",
      text: "xy",
    })
    expect(h.store!.getConnection(TAB_A)?.lastAppliedSeq).toBe(3)

    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    act(() => {
      h.runAnimationFrame()
    })

    // A recovered via snapshot to event_seq 5 (gap event dropped as duplicate).
    expect(h.store!.getConnection(TAB_A)?.lastAppliedSeq).toBe(5)
    // B remains healthy and was never starved.
    expect(h.store!.getConnection(TAB_B)?.lastAppliedSeq).toBe(3)
  })

  it("cursor-only frame skips live sink and key notify", async () => {
    await mountDesktopOwner()
    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            connection_id: "owner-conn",
            seq: 1,
            type: "status_changed",
            status: "prompting",
          },
        ])
      )
      h.runAnimationFrame()
    })

    const sink = vi.fn()
    h.actions!.registerLiveMessageSink(TAB, sink)
    sink.mockClear()
    const notify = vi.fn()
    const unsubscribe = h.store!.subscribeKey(TAB, notify)
    const liveBefore = h.store!.getConnection(TAB)?.liveMessage

    act(() => {
      // user_prompt_sent is notification-only — cursor advance only.
      h.emitDesktopBatch(
        batch(2, [
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "user_prompt_sent",
            text_preview: "hi",
          },
        ])
      )
      h.runAnimationFrame()
    })

    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(2)
    expect(h.store!.getConnection(TAB)?.liveMessage).toBe(liveBefore)
    expect(sink).not.toHaveBeenCalled()
    expect(notify).not.toHaveBeenCalled()
    unsubscribe()
  })
})

describe("APPLY_EVENT_FRAME reducer parity", () => {
  /**
   * Closed set of FrameAction types produced by `prepareMappedEnvelope` on the
   * frame commit path. Map-level actions and direct-dispatch-only actions
   * (HYDRATE_FROM_SNAPSHOT, STREAM_BATCH, BATCH_TOOL_CALL_UPDATES,
   * DISMISS_CONFIG_STALE, CONFIG_OPTION_CHANGED, CLEAR_ACP_LOAD_ERROR,
   * EVENT_APPLIED, CLEAR_PENDING_QUESTION) are intentionally excluded — they
   * are never listed in PreparedConnectionFrame.actions.
   */
  function baseConn(
    overrides: Partial<
      import("@/contexts/acp-connections-context").ConnectionState
    > = {}
  ): import("@/contexts/acp-connections-context").ConnectionState {
    return {
      connectionId: "c1",
      contextKey: "k1",
      agentType: "claude_code",
      workingDir: "/tmp",
      status: "prompting",
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: "s1",
      modes: {
        current_mode_id: "default",
        available_modes: [
          { id: "default", name: "Default", description: null },
        ],
      },
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: {
        id: "lm",
        role: "assistant",
        content: [],
        startedAt: 1,
      },
      pendingPermission: {
        request_id: "req-0",
        tool_call: { toolCallId: "t0" },
        options: [],
      },
      pendingUserMessage: null,
      pendingQuestion: {
        tool_call_id: "tq",
        question: "old?",
      },
      pendingAskQuestion: {
        question_id: "ask-0",
        questions: [],
        created_at: "2020-01-01T00:00:00.000Z",
      },
      pendingPlanApproval: {
        approval_id: "approval-0",
        tool_call_id: "plan-tool-0",
        plan_markdown: "old plan",
        created_at: "2020-01-01T00:00:00.000Z",
      },
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      sessionFailures: [],
      ...overrides,
    }
  }

  it("rolls live content back through the final tool boundary", () => {
    const toolInfo = {
      tool_call_id: "t1",
      title: "Read",
      kind: "read",
      status: "completed",
      content: null,
      raw_input: "{}",
      raw_output_chunks: [],
      raw_output_total_bytes: 0,
      locations: null,
      meta: null,
      images: [],
    }
    const conn = baseConn({
      liveMessage: {
        id: "lm",
        role: "assistant",
        content: [
          { type: "text", text: "accepted prefix" },
          { type: "tool_call", info: toolInfo },
          { type: "thinking", text: "stale thought" },
          { type: "text", text: "stale answer" },
          {
            type: "plan",
            entries: [
              { content: "stale plan", status: "pending", priority: "high" },
            ],
          },
        ],
        startedAt: 1,
      },
    })

    const rolled = __connectionsReducerForTests(new Map([["k1", conn]]), {
      type: "TURN_ATTEMPT_ROLLBACK",
      contextKey: "k1",
    }).get("k1")!

    expect(rolled.liveMessage?.content).toEqual([
      { type: "text", text: "accepted prefix" },
      { type: "tool_call", info: toolInfo },
    ])

    const withoutTool = baseConn({
      liveMessage: {
        id: "lm-no-tool",
        role: "assistant",
        content: [
          { type: "text", text: "stale" },
          { type: "thinking", text: "stale thought" },
        ],
        startedAt: 1,
      },
    })
    const cleared = __connectionsReducerForTests(
      new Map([["k1", withoutTool]]),
      { type: "TURN_ATTEMPT_ROLLBACK", contextKey: "k1" }
    ).get("k1")!
    expect(cleared.liveMessage?.content).toEqual([])
  })

  const framePathFixtures: Array<{
    name: string
    action: import("@/contexts/acp-connections-context").__FrameActionForTests
    conn?: Partial<import("@/contexts/acp-connections-context").ConnectionState>
  }> = [
    {
      name: "CONTENT_DELTA",
      action: {
        type: "CONTENT_DELTA",
        contextKey: "k1",
        text: "hi",
        receivedAt: 1,
      },
    },
    {
      name: "THINKING",
      action: {
        type: "THINKING",
        contextKey: "k1",
        text: "hmm",
        receivedAt: 1,
      },
    },
    {
      name: "STATUS_CHANGED",
      action: {
        type: "STATUS_CHANGED",
        contextKey: "k1",
        status: "connected",
      },
    },
    {
      name: "CONTINUATION_WAITING_CHANGED",
      action: {
        type: "CONTINUATION_WAITING_CHANGED",
        contextKey: "k1",
        waiting: {
          conversation_id: 9,
          state: "waiting",
          generation: 1,
          armed_at: "2026-01-01T00:00:00.000Z",
          wake_at: "2026-01-01T00:04:00.000Z",
        },
      },
    },
    {
      name: "ERROR",
      action: { type: "ERROR", contextKey: "k1", message: "boom" },
    },
    {
      name: "USAGE_UPDATE",
      action: {
        type: "USAGE_UPDATE",
        contextKey: "k1",
        usage: { used: 1, size: 10 },
        boundaryAt: 1,
      },
    },
    {
      name: "SESSION_STARTED",
      action: {
        type: "SESSION_STARTED",
        contextKey: "k1",
        sessionId: "new-sess",
      },
    },
    {
      name: "SESSION_MODES",
      action: {
        type: "SESSION_MODES",
        contextKey: "k1",
        modes: {
          current_mode_id: "plan",
          available_modes: [{ id: "plan", name: "Plan", description: null }],
        },
      },
    },
    {
      name: "SESSION_CONFIG_OPTIONS",
      action: {
        type: "SESSION_CONFIG_OPTIONS",
        contextKey: "k1",
        configOptions: [
          {
            id: "model",
            name: "Model",
            description: null,
            category: null,
            kind: {
              type: "select",
              current_value: "m1",
              options: [{ value: "m1", name: "M1" }],
              groups: [],
            },
          },
        ],
      },
    },
    {
      name: "CONFIG_STALE_CHANGED",
      action: {
        type: "CONFIG_STALE_CHANGED",
        contextKey: "k1",
        stale: true,
        kind: "agent_config",
      },
    },
    {
      name: "SELECTORS_READY",
      action: { type: "SELECTORS_READY", contextKey: "k1" },
    },
    {
      name: "PROMPT_CAPABILITIES",
      action: {
        type: "PROMPT_CAPABILITIES",
        contextKey: "k1",
        promptCapabilities: {
          image: true,
          audio: false,
          embedded_context: true,
        },
      },
    },
    {
      name: "FORK_SUPPORTED",
      action: {
        type: "FORK_SUPPORTED",
        contextKey: "k1",
        supported: true,
      },
    },
    {
      name: "MODE_CHANGED",
      action: {
        type: "MODE_CHANGED",
        contextKey: "k1",
        modeId: "plan",
      },
      conn: {
        modes: {
          current_mode_id: "default",
          available_modes: [
            { id: "default", name: "Default", description: null },
            { id: "plan", name: "Plan", description: null },
          ],
        },
      },
    },
    {
      name: "PLAN_UPDATE",
      action: {
        type: "PLAN_UPDATE",
        contextKey: "k1",
        entries: [{ content: "a", status: "pending", priority: "medium" }],
        receivedAt: 1,
      },
    },
    {
      name: "TURN_ATTEMPT_ROLLBACK",
      action: { type: "TURN_ATTEMPT_ROLLBACK", contextKey: "k1" },
    },
    {
      name: "CLAUDE_API_RETRY",
      action: {
        type: "CLAUDE_API_RETRY",
        contextKey: "k1",
        retry: {
          sessionId: "s1",
          attempt: 1,
          maxRetries: 3,
          error: "rate limit",
          errorStatus: 429,
          retryDelayMs: 1000,
        },
      },
    },
    {
      name: "TOOL_CALL",
      action: {
        type: "TOOL_CALL",
        contextKey: "k1",
        tool_call_id: "t1",
        title: "Bash",
        kind: "execute",
        status: "pending",
        content: null,
        raw_input: "{}",
        raw_output: null,
        locations: null,
        meta: null,
        images: null,
        receivedAt: 1,
      },
    },
    {
      name: "TOOL_CALL_UPDATE",
      action: {
        type: "TOOL_CALL_UPDATE",
        contextKey: "k1",
        tool_call_id: "t1",
        title: "Bash",
        fallback_title: "tool",
        fallback_kind: "tool",
        status: "in_progress",
        content: null,
        raw_input: null,
        raw_output: "out",
        raw_output_append: true,
        locations: null,
        meta: null,
        images: null,
        receivedAt: 1,
      },
    },
    {
      name: "PERMISSION_REQUEST",
      action: {
        type: "PERMISSION_REQUEST",
        contextKey: "k1",
        request_id: "req-1",
        tool_call: { toolCallId: "t1" },
        fallback_title: "tool",
        fallback_kind: "tool",
        options: [],
      },
    },
    {
      name: "PERMISSION_CLEARED",
      action: {
        type: "PERMISSION_CLEARED",
        contextKey: "k1",
        requestId: "req-0",
      },
    },
    {
      name: "SET_ASK_QUESTION",
      action: {
        type: "SET_ASK_QUESTION",
        contextKey: "k1",
        pendingAskQuestion: {
          question_id: "q1",
          questions: [
            {
              id: "q1",
              question: "?",
              header: "H",
              multi_select: false,
              options: [],
            },
          ],
          created_at: "2020-01-01T00:00:00.000Z",
        },
      },
    },
    {
      name: "CLEAR_ASK_QUESTION",
      action: {
        type: "CLEAR_ASK_QUESTION",
        contextKey: "k1",
        questionId: "ask-0",
      },
    },
    {
      name: "SET_PLAN_APPROVAL",
      action: {
        type: "SET_PLAN_APPROVAL",
        contextKey: "k1",
        pendingPlanApproval: {
          approval_id: "approval-1",
          tool_call_id: "plan-tool-1",
          plan_markdown: "new plan",
          created_at: "2020-01-02T00:00:00.000Z",
        },
      },
    },
    {
      name: "CLEAR_PLAN_APPROVAL",
      action: {
        type: "CLEAR_PLAN_APPROVAL",
        contextKey: "k1",
        approvalId: "approval-0",
      },
    },
    {
      name: "SET_PENDING_QUESTION",
      action: {
        type: "SET_PENDING_QUESTION",
        contextKey: "k1",
        pendingQuestion: {
          tool_call_id: "tq2",
          question: "continue?",
        },
      },
    },
    {
      name: "SET_BACKGROUND_OUTSTANDING",
      action: {
        type: "SET_BACKGROUND_OUTSTANDING",
        contextKey: "k1",
        outstanding: 2,
        settledCount: 0,
        turnsCount: 0,
      },
    },
    {
      name: "AVAILABLE_COMMANDS",
      action: {
        type: "AVAILABLE_COMMANDS",
        contextKey: "k1",
        commands: [{ name: "help", description: "Help" }],
      },
    },
    {
      name: "ACP_LOAD_ERROR",
      action: {
        type: "ACP_LOAD_ERROR",
        contextKey: "k1",
        message: "gone",
        code: "resource_not_found",
      },
    },
  ]

  it("documents the closed frame-path FrameAction set", () => {
    const types = framePathFixtures.map((f) => f.action.type).sort()
    expect(types).toEqual(
      [
        "ACP_LOAD_ERROR",
        "AVAILABLE_COMMANDS",
        "CLAUDE_API_RETRY",
        "CLEAR_ASK_QUESTION",
        "CLEAR_PLAN_APPROVAL",
        "CONFIG_STALE_CHANGED",
        "CONTENT_DELTA",
        "CONTINUATION_WAITING_CHANGED",
        "ERROR",
        "FORK_SUPPORTED",
        "MODE_CHANGED",
        "PERMISSION_CLEARED",
        "PERMISSION_REQUEST",
        "PLAN_UPDATE",
        "PROMPT_CAPABILITIES",
        "SELECTORS_READY",
        "SESSION_CONFIG_OPTIONS",
        "SESSION_MODES",
        "SESSION_STARTED",
        "SET_ASK_QUESTION",
        "SET_BACKGROUND_OUTSTANDING",
        "SET_PLAN_APPROVAL",
        "SET_PENDING_QUESTION",
        "STATUS_CHANGED",
        "THINKING",
        "TURN_ATTEMPT_ROLLBACK",
        "TOOL_CALL",
        "TOOL_CALL_UPDATE",
        "USAGE_UPDATE",
      ].sort()
    )
  })

  it.each(framePathFixtures)(
    "single-action and one-item frame match for $name",
    ({ action, conn }) => {
      const state = new Map([["k1", baseConn(conn)]])
      __resetWritableConnectionsCloneCount()
      const single = __connectionsReducerForTests(state, action)
      __resetWritableConnectionsCloneCount()
      const framed = __connectionsReducerForTests(state, {
        type: "APPLY_EVENT_FRAME",
        frames: [
          {
            contextKey: "k1",
            deliveryIds: [1],
            actions: [action],
            highestSeq: 0,
          },
        ],
      })
      // Frame path clones the outer map exactly once.
      expect(__getWritableConnectionsCloneCount()).toBe(1)
      expect(framed.get("k1")).toEqual(single.get("k1"))
    }
  )
})

describe("request usage generation clock", () => {
  async function connectPromptingOwner() {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    return handlers
  }

  it("keeps tool-call-only usage by starting the clock on top-level tool output", async () => {
    const handlers = await connectPromptingOwner()

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      received_at: 1_000,
      type: "tool_call",
      tool_call_id: "tool-1",
      title: "Read",
      kind: "read",
      status: "in_progress",
      content: null,
      raw_input: null,
      raw_output: null,
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      received_at: 1_750,
      type: "request_usage",
      output_tokens: 25,
      duration_ms: null,
    })

    expect(h.store!.getConnection(TAB)?.requestUsage).toMatchObject({
      outputTokens: 25,
      generationMs: 750,
      sampleCount: 1,
    })
  })

  it("does not start the clock for a status-only tool call update", async () => {
    const handlers = await connectPromptingOwner()

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      received_at: 1_000,
      type: "tool_call_update",
      tool_call_id: "tool-1",
      title: null,
      status: "completed",
      content: null,
      raw_input: null,
      raw_output: null,
    })

    expect(h.store!.getConnection(TAB)?.generationClockStartedAt).toBeNull()
  })
})

describe("send_prompt_forwards_prompt_context_to_api", () => {
  it("forwards promptContext as the required sixth argument to acpPrompt", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    await act(async () => {
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }], {
        folderId: 1,
        conversationId: 2,
        clientMessageId: "m1",
        promptContext: {
          visibleText: "README.md task",
          locale: "zh_cn",
        },
      })
    })

    expect(acpPromptMock).toHaveBeenCalledWith(
      "spawned-conn",
      [{ type: "text", text: "wire" }],
      1,
      2,
      "m1",
      {
        visibleText: "README.md task",
        locale: "zh_cn",
      }
    )
  })

  it("supplies null context when an older direct caller omits promptContext", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    await act(async () => {
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }], {
        folderId: 1,
        conversationId: 2,
        clientMessageId: "m1",
      })
    })

    expect(acpPromptMock).toHaveBeenCalledWith(
      "spawned-conn",
      [{ type: "text", text: "wire" }],
      1,
      2,
      "m1",
      {
        visibleText: null,
        locale: null,
      }
    )
  })
})

describe("root_conversation_activity_at_acp_dispatch_boundaries", () => {
  it("begins root activity immediately before acpPrompt and keeps it on success", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      false
    )

    await act(async () => {
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }])
    })

    expect(acpPromptMock).toHaveBeenCalledTimes(1)
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      true
    )
  })

  it("rolls back the exact prompt token when acpPrompt rejects", async () => {
    acpPromptMock.mockRejectedValueOnce(new Error("send failed"))
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })

    await expect(
      h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }])
    ).rejects.toThrow("send failed")
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      false
    )
  })

  it("rolls back exact overlay and rethrows TurnBusyError for busy/TurnInProgress", async () => {
    const { TurnBusyError } = await import("@/lib/turn-busy")
    acpPromptMock.mockRejectedValueOnce(new TurnBusyError())
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })

    // Activity begins immediately before the wire call; busy rejection must
    // roll the exact overlay back and propagate the same TurnBusyError so the
    // lifecycle/requeue path can catch it.
    await expect(
      h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }])
    ).rejects.toBeInstanceOf(TurnBusyError)
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      false
    )
    expect(acpPromptMock).toHaveBeenCalledTimes(1)
  })

  it("uses explicit opts.conversationId over the bound connection id", async () => {
    useAppWorkspaceStore
      .getState()
      .applyConversationUpsert(makeSummary({ id: 3 }))
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })

    await act(async () => {
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "wire" }], {
        conversationId: 3,
      })
    })

    const optimistic = useAppWorkspaceStore.getState().optimisticActivityById
    expect(optimistic.has(3)).toBe(true)
    expect(optimistic.has(2)).toBe(false)
  })

  it("rejects a connection-not-found error for an unknown connection context", async () => {
    await mountProvider()

    await expect(
      h.actions!.sendPrompt("missing-key", [{ type: "text", text: "wire" }])
    ).rejects.toMatchObject({
      code: "connection_not_found",
      message: expect.stringContaining("missing-key"),
    })

    expect(acpPromptMock).not.toHaveBeenCalled()
    expect(useAppWorkspaceStore.getState().optimisticActivityById.size).toBe(0)
  })

  it("begins viewer root activity through the connection-bound id", async () => {
    h.acpFindConnectionForConversation.mockResolvedValueOnce({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })

    await act(async () => {
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "viewer send" }])
    })

    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      true
    )
  })

  it("begins root activity immediately before acpAnswerQuestion and keeps it on success", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      false
    )

    await act(async () => {
      await h.actions!.answerQuestion(TAB, "q-1", {
        answers: [{ questionId: "choice", labels: ["A"] }],
        declined: false,
      })
    })

    expect(acpAnswerQuestionMock).toHaveBeenCalledTimes(1)
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      true
    )
  })

  it("rolls back the exact answer-question token when acpAnswerQuestion rejects", async () => {
    acpAnswerQuestionMock.mockRejectedValueOnce(new Error("answer failed"))
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })

    await expect(
      h.actions!.answerQuestion(TAB, "q-1", {
        answers: [{ questionId: "choice", labels: ["A"] }],
        declined: false,
      })
    ).rejects.toThrow("answer failed")
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(2)).toBe(
      false
    )
  })

  it("does not begin root activity for delegation-child answerQuestion", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 2)
    })

    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "child-1",
        parentConnectionId: "spawned-conn",
        parentToolUseId: "tool-1",
        agentType: "codex",
      })
    })

    await act(async () => {
      await h.actions!.answerQuestion("child-1", "q-child", {
        answers: [{ questionId: "choice", labels: ["A"] }],
        declined: false,
      })
    })

    expect(acpAnswerQuestionMock).toHaveBeenCalledTimes(1)
    expect(useAppWorkspaceStore.getState().optimisticActivityById.size).toBe(0)
  })
})

describe("AcpConnectionsProvider pop-out ownership bridge", () => {
  it("releaseConnectionWithoutDisconnect removes main owner without acpDisconnect", async () => {
    const {
      releaseConnectionWithoutDisconnect,
      __resetTransferFencesForTests,
    } = await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("spawned-conn")
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(false)

    h.acpDisconnect.mockClear()
    await act(async () => {
      await releaseConnectionWithoutDisconnect(42, "op-release")
    })

    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("claimConnectionOwnership attaches as owner without spawning a second agent", async () => {
    const { claimConnectionOwnership, __resetTransferFencesForTests } =
      await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    await mountProvider()
    h.acpConnect.mockClear()
    h.attach.mockClear()

    const detachedKey = "conversation-99-claude_code"
    await act(async () => {
      const result = await claimConnectionOwnership({
        conversationId: 99,
        connectionId: "live-rebind-conn",
        agentType: "claude_code",
        workingDir: "/tmp/repo",
        operationId: "op-claim",
        contextKey: detachedKey,
        ownershipGeneration: 2,
        ownerWindowLabel: "conversation-99",
      })
      expect(result.connectionId).toBe("live-rebind-conn")
      expect(result.ownershipGeneration).toBe(2)
    })

    expect(h.acpConnect).not.toHaveBeenCalled()
    const claimed = h.store!.getConnection(detachedKey)
    expect(claimed).toBeTruthy()
    expect(claimed!.connectionId).toBe("live-rebind-conn")
    expect(claimed!.isViewer).toBe(false)
    expect(claimed!.conversationId).toBe(99)
    expect(claimed!.ownershipGeneration).toBe(2)
    expect(claimed!.ownerOperationId).toBe("op-claim")
    // Web/attach path: ownership claim attaches the existing connection.
    expect(h.attach).toHaveBeenCalledWith(
      "live-rebind-conn",
      expect.anything(),
      expect.anything()
    )
  })

  it("two sequential reverse reclaims adopt post-reverse lease so tab close disconnects", async () => {
    const {
      releaseConnectionWithoutDisconnect,
      reclaimAfterAbort,
      __resetTransferFencesForTests,
    } = await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    // Reclaim requires liveness via snapshot before CONNECTION_CREATED.
    h.acpGetSessionSnapshot.mockResolvedValue({
      connection_id: "spawned-conn",
      status: "connected",
    })
    h.denormalizeSnapshot.mockReturnValue({
      connectionId: "spawned-conn",
      status: "connected",
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      lastError: null,
      eventSeq: 0,
      activeDelegations: [],
      toolWatchdogProjections: {},
      delegationRoute: null,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    h.acpDisconnect.mockClear()

    // First failed live handoff: reverse stamps main/op-A/gen=2.
    await act(async () => {
      await releaseConnectionWithoutDisconnect(42, "op-A")
    })
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    await act(async () => {
      await reclaimAfterAbort(42, "op-A", {
        ownershipGeneration: 2,
        ownerWindowLabel: "main",
      })
    })
    let conn = h.store!.getConnection(TAB)
    expect(conn).toBeTruthy()
    expect(conn!.ownerOperationId).toBe("op-A")
    expect(conn!.ownershipGeneration).toBe(2)
    expect(conn!.ownerWindowLabel).toBe("main")
    expect(h.acpConnect).toHaveBeenCalledTimes(1)

    // Second failed live handoff: reverse stamps main/op-B/gen=4.
    // Reclaim must NOT restore the pre-transfer snapshot (op-A/gen=2).
    await act(async () => {
      await releaseConnectionWithoutDisconnect(42, "op-B")
    })
    await act(async () => {
      await reclaimAfterAbort(42, "op-B", {
        ownershipGeneration: 4,
        ownerWindowLabel: "main",
      })
    })
    conn = h.store!.getConnection(TAB)
    expect(conn).toBeTruthy()
    expect(conn!.ownerOperationId).toBe("op-B")
    expect(conn!.ownershipGeneration).toBe(4)
    expect(conn!.ownerWindowLabel).toBe("main")
    expect(h.acpConnect).toHaveBeenCalledTimes(1)

    // Closing the restored main tab must disconnect with the live lease.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    expect(h.acpDisconnect).toHaveBeenCalledWith("spawned-conn", {
      expectedOwnerWindow: "main",
      expectedOperationId: "op-B",
      expectedOwnershipGeneration: 4,
      origin: "explicit_user",
    })
  })

  it("reclaimAfterAbort throws connection_gone when snapshot is null (no dead owner)", async () => {
    const {
      releaseConnectionWithoutDisconnect,
      reclaimAfterAbort,
      __resetTransferFencesForTests,
    } = await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    await act(async () => {
      await releaseConnectionWithoutDisconnect(42, "op-gone")
    })
    expect(h.store!.getConnection(TAB)).toBeUndefined()

    // Reverse succeeded but agent exited before reclaim proves liveness.
    h.acpGetSessionSnapshot.mockResolvedValue(null)

    await expect(
      act(async () => {
        await reclaimAfterAbort(42, "op-gone", {
          ownershipGeneration: 3,
          ownerWindowLabel: "main",
        })
      })
    ).rejects.toThrow(/connection_gone/i)

    // Must not invent a connecting/dead owner that blocks later connect.
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("pre-ready reverse refreshes lease in place without inventing CONNECTION_CREATED", async () => {
    const { reclaimAfterAbort, __resetTransferFencesForTests } =
      await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    const before = h.store!.getConnection(TAB)
    expect(before).toBeTruthy()
    // Stale lease from a prior incarnation (op-A/gen-2) while main still holds.
    // Simulate by reclaiming once with that lease first via release+reclaim,
    // then refresh without a second release (pre-ready path).
    await act(async () => {
      await reclaimAfterAbort(42, "op-B", {
        ownershipGeneration: 4,
        ownerWindowLabel: "main",
      })
    })
    const conn = h.store!.getConnection(TAB)
    expect(conn).toBeTruthy()
    expect(conn!.connectionId).toBe("spawned-conn")
    expect(conn!.ownerOperationId).toBe("op-B")
    expect(conn!.ownershipGeneration).toBe(4)
    expect(conn!.ownerWindowLabel).toBe("main")
    // No second spawn / invent — still the live connection.
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    expect(h.store!.getConnection(TAB)?.status).not.toBeUndefined()
  })

  it("fenced disconnect snapshots for reclaim; late reverse restores main owner", async () => {
    // R7 Critical barrier: null-closed wait + source-tab unmount drops local
    // entry but snapshots into releasedForReclaim → late Reversed full reclaim
    // restores main owner (not in-place no-op on empty map).
    const {
      markTransferringOut,
      getTransferFence,
      reclaimAfterAbort,
      __resetTransferFencesForTests,
    } = await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpGetSessionSnapshot.mockResolvedValue({
      connection_id: "spawned-conn",
      status: "connected",
    })
    h.denormalizeSnapshot.mockReturnValue({
      connectionId: "spawned-conn",
      status: "connected",
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      lastError: null,
      eventSeq: 0,
      activeDelegations: [],
      toolWatchdogProjections: {},
      delegationRoute: null,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("spawned-conn")

    markTransferringOut(42, "op-fenced-teardown")
    h.acpDisconnect.mockClear()

    // Source main tab unmount while transfer fence is set (reverse pending).
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })

    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    expect(getTransferFence(42)?.mainReleased).toBe(true)
    expect(getTransferFence(42)?.operationId).toBe("op-fenced-teardown")

    // Late reverse: full reclaim from releasedForReclaim snapshot.
    await act(async () => {
      await reclaimAfterAbort(42, "op-fenced-teardown", {
        ownershipGeneration: 11,
        ownerWindowLabel: "main",
      })
    })

    const restored = h.store!.getConnection(TAB)
    expect(restored).toBeTruthy()
    expect(restored!.connectionId).toBe("spawned-conn")
    expect(restored!.ownerOperationId).toBe("op-fenced-teardown")
    expect(restored!.ownershipGeneration).toBe(11)
    expect(restored!.ownerWindowLabel).toBe("main")
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
  })

  it("reclaimAfterAbort fails closed when map empty and no released snapshot", async () => {
    const { reclaimAfterAbort, __resetTransferFencesForTests } =
      await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    await mountProvider()
    // No connect / no release snapshot — reverse lease cannot be adopted.
    await expect(
      act(async () => {
        await reclaimAfterAbort(42, "op-orphan", {
          ownershipGeneration: 9,
          ownerWindowLabel: "main",
        })
      })
    ).rejects.toThrow(/reclaim_failed|releasedForReclaim/i)
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("suppressed owner disconnect never acpDisconnects (post-ack detached lifetime)", async () => {
    const {
      setSuppressFrontendDisconnect,
      isFrontendDisconnectSuppressed,
      __resetTransferFencesForTests,
    } = await import("@/lib/conversation-popout-acp-bridge")
    const {
      resolveDetachedConnectGate,
      shouldClearSuppressOnDetachedCommitAck,
    } = await import("@/lib/conversation-popout-detached-bootstrap")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("spawned-conn")
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(false)

    // Detached bootstrap: suppress for full window lifetime (pre-ack).
    setSuppressFrontendDisconnect(42, true)
    expect(isFrontendDisconnectSuppressed(42)).toBe(true)

    // Spec 17 / applyAck: commit-ack must not clear suppress. Page only sets
    // commitAcked; gate still reports suppress true; bridge flag stays set.
    expect(shouldClearSuppressOnDetachedCommitAck()).toBe(false)
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: true,
        commitAcked: true,
      }).suppressFrontendDisconnect
    ).toBe(true)
    // Simulated applyAck never calls setSuppress(..., false).
    expect(isFrontendDisconnectSuppressed(42)).toBe(true)

    h.acpDisconnect.mockClear()

    // Wire through the real disconnect path used by useConnectionLifecycle
    // unmount (shouldDisconnectOnUnmount → connDisconnect → provider.disconnect).
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })

    expect(h.acpDisconnect).toHaveBeenCalledTimes(0)
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    // Suppress flag survives disconnect (dies only with JS context / explicit clear).
    expect(isFrontendDisconnectSuppressed(42)).toBe(true)
  })

  it("suppressed owner with pending_permission never acpDisconnects", async () => {
    const {
      setSuppressFrontendDisconnect,
      isFrontendDisconnectSuppressed,
      __resetTransferFencesForTests,
    } = await import("@/lib/conversation-popout-acp-bridge")
    __resetTransferFencesForTests()

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })

    // Kill vector: status=connected + real pending_permission + 0 background.
    // shouldDisconnectOnUnmount does NOT check pendingPermission, so unmount
    // would call disconnect() — suppress must short-circuit before acpDisconnect.
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "connected",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "permission_request",
      request_id: "req-detached-perm",
      tool_call: {
        kind: "execute",
        status: "pending",
        toolCallId: "call-detached-perm",
      },
      options: [],
    })

    const conn = h.store!.getConnection(TAB)
    expect(conn?.conversationId).toBe(42)
    expect(conn?.isViewer).toBe(false)
    expect(conn?.status).toBe("connected")
    expect(conn?.backgroundOutstanding).toBe(0)
    expect(conn?.pendingPermission).not.toBeNull()
    expect(conn?.pendingPermission?.request_id).toBe("req-detached-perm")

    setSuppressFrontendDisconnect(42, true)
    expect(isFrontendDisconnectSuppressed(42)).toBe(true)
    h.acpDisconnect.mockClear()

    // Unmount/teardown path with a live permission prompt still open.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })

    expect(h.acpDisconnect).toHaveBeenCalledTimes(0)
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    expect(isFrontendDisconnectSuppressed(42)).toBe(true)
  })
})

describe("tool_watchdog_changed reduction and desktop notification", () => {
  beforeEach(async () => {
    const { __resetToolWatchdogNotifyDedupeForTests } =
      await import("@/contexts/acp-connections-context")
    __resetToolWatchdogNotifyDedupeForTests()
    h.isDesktop = true
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => true,
    })
  })

  function projection(
    overrides: Partial<import("@/lib/types").ToolWatchdogProjection> = {}
  ): import("@/lib/types").ToolWatchdogProjection {
    return {
      lease_id: "lease-w1",
      version: 2,
      tool_title: "terminal",
      phase: "grace",
      last_progress_at: "2026-07-23T00:00:00.000Z",
      transition_at: "2026-07-23T00:00:00.000Z",
      grace_deadline: "2026-07-23T00:10:00.000Z",
      cancellation_scope: null,
      error_code: null,
      ...overrides,
    }
  }

  it("reduces live tool_watchdog_changed into the connection map", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    const handlers = latestAttachHandlers()

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ phase: "grace", version: 2 }),
    })

    const conn = h.store!.getConnection(TAB)
    expect(conn?.toolWatchdogProjections?.["lease-w1"]?.version).toBe(2)
    expect(conn?.toolWatchdogProjections?.["lease-w1"]?.phase).toBe("grace")
  })

  it("progress clear and timed_out remove the map entry without inventing terminal status", async () => {
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    const handlers = latestAttachHandlers()

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ version: 2, phase: "grace" }),
    })
    expect(
      h.store!.getConnection(TAB)?.toolWatchdogProjections?.["lease-w1"]
    ).toBeTruthy()

    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ version: 3, phase: "cleared" }),
    })
    expect(
      h.store!.getConnection(TAB)?.toolWatchdogProjections?.["lease-w1"]
    ).toBeUndefined()
    // Connection status is not locally forced to a terminal tool outcome.
    expect(h.store!.getConnection(TAB)?.status).toBe("prompting")

    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ version: 4, phase: "grace" }),
    })
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({
        version: 5,
        phase: "timed_out",
        error_code: "tool_stalled_timeout",
      }),
    })
    expect(
      h.store!.getConnection(TAB)?.toolWatchdogProjections?.["lease-w1"]
    ).toBeUndefined()
  })

  it("two-window winner/loser converge on the higher version", () => {
    const minimal = {
      connectionId: "c1",
      contextKey: "k1",
      agentType: "claude_code" as const,
      workingDir: "/tmp",
      status: "prompting" as const,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: "s1",
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingUserMessage: null,
      pendingQuestion: null,
      pendingAskQuestion: null,
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      toolWatchdogProjections: {
        "lease-w1": projection({ version: 2, phase: "grace" }),
      },
    }
    const winnerEvent = {
      type: "TOOL_WATCHDOG_CHANGED" as const,
      contextKey: "k1",
      projection: projection({ version: 3, phase: "cancelling" }),
    }
    const a = __connectionsReducerForTests(
      new Map([["k1", minimal]]),
      winnerEvent
    ).get("k1")!
    const b = __connectionsReducerForTests(
      new Map([["k1", { ...minimal }]]),
      winnerEvent
    ).get("k1")!
    expect(a.toolWatchdogProjections?.["lease-w1"]?.version).toBe(3)
    expect(b.toolWatchdogProjections?.["lease-w1"]?.version).toBe(3)
    expect(a.toolWatchdogProjections?.["lease-w1"]?.phase).toBe("cancelling")
    expect(b.toolWatchdogProjections?.["lease-w1"]?.phase).toBe("cancelling")
  })

  it("hydrates toolWatchdogProjections from snapshot", () => {
    const map = {
      "lease-a": projection({ lease_id: "lease-a", version: 7 }),
    }
    const before = {
      connectionId: "spawned-conn",
      contextKey: "k1",
      agentType: "claude_code" as const,
      workingDir: "/tmp",
      status: "connected" as const,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingUserMessage: null,
      pendingQuestion: null,
      pendingAskQuestion: null,
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      toolWatchdogProjections: {},
    }
    const next = __connectionsReducerForTests(new Map([["k1", before]]), {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "k1",
      patch: {
        connectionId: "spawned-conn",
        conversationId: null,
        status: "connected",
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: null,
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        lastError: null,
        eventSeq: 5,
        activeDelegations: [],
        toolWatchdogProjections: map,
        lastToolWatchdogDiagnostic: null,
        delegationRoute: null,
        waitingForSubagents: null,
        backgroundOutstanding: 0,
      },
    }).get("k1")!
    expect(next.toolWatchdogProjections?.["lease-a"]?.version).toBe(7)
  })

  it("hydrates lastToolWatchdogDiagnostic after timed_out (empty actionable map)", () => {
    const timedOut = projection({
      lease_id: "lease-dead",
      version: 3,
      phase: "timed_out",
      transition_at: "2026-07-23T00:20:00.000Z",
      error_code: "tool_stalled_timeout",
      grace_deadline: null,
    })
    const before = {
      connectionId: "spawned-conn",
      contextKey: "k1",
      agentType: "claude_code" as const,
      workingDir: "/tmp",
      status: "connected" as const,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingUserMessage: null,
      pendingQuestion: null,
      pendingAskQuestion: null,
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      toolWatchdogProjections: {},
      lastToolWatchdogDiagnostic: null,
    }
    const next = __connectionsReducerForTests(new Map([["k1", before]]), {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "k1",
      patch: {
        connectionId: "spawned-conn",
        conversationId: null,
        status: "connected",
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: null,
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        lastError: null,
        eventSeq: 9,
        activeDelegations: [],
        toolWatchdogProjections: {},
        lastToolWatchdogDiagnostic: timedOut,
        delegationRoute: null,
        waitingForSubagents: null,
        backgroundOutstanding: 0,
      },
    }).get("k1")!
    expect(next.toolWatchdogProjections).toEqual({})
    expect(next.lastToolWatchdogDiagnostic?.phase).toBe("timed_out")
    expect(next.lastToolWatchdogDiagnostic?.error_code).toBe(
      "tool_stalled_timeout"
    )
    expect(next.lastToolWatchdogDiagnostic?.transition_at).toBe(
      "2026-07-23T00:20:00.000Z"
    )
  })

  it("cold multi-lease hydrate rejects late lower-version cancelling for A", () => {
    // I1 R3: A TimedOut(v3), B is sole last diagnostic; snapshot carries A's
    // floor via toolWatchdogMaxVersions; delayed A Cancelling(v2) must not
    // resurrect A's banner after cold attach.
    const before = {
      connectionId: "spawned-conn",
      contextKey: "k1",
      agentType: "claude_code" as const,
      workingDir: "/tmp",
      status: "connected" as const,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingUserMessage: null,
      pendingQuestion: null,
      pendingAskQuestion: null,
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      toolWatchdogProjections: {},
      lastToolWatchdogDiagnostic: null,
    }
    const leaseB = projection({
      lease_id: "lease-b",
      version: 2,
      phase: "warning",
      transition_at: "2026-07-23T00:30:00.000Z",
    })
    const hydrated = __connectionsReducerForTests(new Map([["k1", before]]), {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "k1",
      patch: {
        connectionId: "spawned-conn",
        conversationId: null,
        status: "connected",
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: null,
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        lastError: null,
        eventSeq: 11,
        activeDelegations: [],
        toolWatchdogProjections: { "lease-b": leaseB },
        toolWatchdogMaxVersions: { "lease-a": 3, "lease-b": 2 },
        lastToolWatchdogDiagnostic: leaseB,
        delegationRoute: null,
        waitingForSubagents: null,
        backgroundOutstanding: 0,
      },
    })
    expect(hydrated.get("k1")?.toolWatchdogMaxVersions?.["lease-a"]).toBe(3)

    const afterLate = __connectionsReducerForTests(hydrated, {
      type: "TOOL_WATCHDOG_CHANGED",
      contextKey: "k1",
      projection: projection({
        lease_id: "lease-a",
        version: 2,
        phase: "cancelling",
      }),
    }).get("k1")!
    expect(afterLate.toolWatchdogProjections?.["lease-a"]).toBeUndefined()
    expect(afterLate.toolWatchdogProjections?.["lease-b"]?.phase).toBe(
      "warning"
    )
  })

  it("picks concurrent live diagnostics by transition_at not version on hydrate", () => {
    const olderHighVersion = projection({
      lease_id: "lease-old",
      version: 9,
      phase: "grace",
      transition_at: "2026-07-23T00:05:00.000Z",
    })
    const newerLowVersion = projection({
      lease_id: "lease-new",
      version: 1,
      phase: "warning",
      transition_at: "2026-07-23T00:15:00.000Z",
    })
    const before = {
      connectionId: "spawned-conn",
      contextKey: "k1",
      agentType: "claude_code" as const,
      workingDir: "/tmp",
      status: "connected" as const,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingUserMessage: null,
      pendingQuestion: null,
      pendingAskQuestion: null,
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      toolWatchdogProjections: {},
      lastToolWatchdogDiagnostic: null,
    }
    const next = __connectionsReducerForTests(new Map([["k1", before]]), {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "k1",
      patch: {
        connectionId: "spawned-conn",
        conversationId: null,
        status: "connected",
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: null,
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        lastError: null,
        eventSeq: 3,
        activeDelegations: [],
        toolWatchdogProjections: {
          "lease-old": olderHighVersion,
          "lease-new": newerLowVersion,
        },
        lastToolWatchdogDiagnostic: null,
        delegationRoute: null,
        waitingForSubagents: null,
        backgroundOutstanding: 0,
      },
    }).get("k1")!
    expect(next.lastToolWatchdogDiagnostic?.lease_id).toBe("lease-new")
    expect(next.lastToolWatchdogDiagnostic?.phase).toBe("warning")
  })

  it("hydrate retains higher-seq diagnostic over equal-millis later map key", () => {
    // Server applied lease-a last (seq 12). Live map also has lease-z with the
    // same wall millis but lower seq; BTreeMap/Object key order ends on lease-z.
    const retained = projection({
      lease_id: "lease-a",
      version: 2,
      phase: "warning",
      transition_at: "2026-07-23T00:00:00.000Z",
      transition_seq: 12,
    })
    const liveZ = projection({
      lease_id: "lease-z",
      version: 1,
      phase: "grace",
      transition_at: "2026-07-23T00:00:00.000Z",
      transition_seq: 11,
    })
    const liveA = projection({
      lease_id: "lease-a",
      version: 2,
      phase: "warning",
      transition_at: "2026-07-23T00:00:00.000Z",
      transition_seq: 12,
    })
    const before = {
      connectionId: "spawned-conn",
      contextKey: "k1",
      agentType: "claude_code" as const,
      workingDir: "/tmp",
      status: "connected" as const,
      promptCapabilities: {
        image: false,
        audio: false,
        embedded_context: false,
      },
      supportsFork: false,
      selectorsReady: false,
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingUserMessage: null,
      pendingQuestion: null,
      pendingAskQuestion: null,
      claudeApiRetry: null,
      error: null,
      loadError: null,
      loadErrorCode: null,
      lastAppliedSeq: 0,
      isDelegationChild: false,
      parentToolUseId: null,
      parentConnectionId: null,
      isViewer: false,
      configStale: false,
      configStaleKind: null,
      configStaleDismissed: false,
      backgroundOutstanding: 0,
      backgroundSettleSyncingSince: null,
      outOfTurnToolCalls: null,
      waitingForSubagents: null,
      toolWatchdogProjections: {},
      lastToolWatchdogDiagnostic: null,
    }
    const next = __connectionsReducerForTests(new Map([["k1", before]]), {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "k1",
      patch: {
        connectionId: "spawned-conn",
        conversationId: null,
        status: "connected",
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        liveMessage: null,
        pendingPermission: null,
        pendingAskQuestion: null,
        pendingUserMessage: null,
        promptCapabilities: null,
        selectorsReady: false,
        supportsFork: false,
        configStale: false,
        configStaleKind: null,
        lastError: null,
        eventSeq: 4,
        activeDelegations: [],
        toolWatchdogProjections: {
          "lease-a": liveA,
          "lease-z": liveZ,
        },
        lastToolWatchdogDiagnostic: retained,
        delegationRoute: null,
        waitingForSubagents: null,
        backgroundOutstanding: 0,
      },
    }).get("k1")!
    expect(next.lastToolWatchdogDiagnostic?.lease_id).toBe("lease-a")
    expect(next.lastToolWatchdogDiagnostic?.transition_seq).toBe(12)
    expect(next.lastToolWatchdogDiagnostic?.phase).toBe("warning")
  })

  it("hidden desktop path notifies once per (lease_id, version) with conversation target", async () => {
    const { sendSystemNotification } = await import("@/lib/notification")
    const notify = vi.mocked(sendSystemNotification)
    notify.mockClear()

    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    const handlers = latestAttachHandlers()
    const p = projection({ version: 2, phase: "grace" })

    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: p,
    })
    // Duplicate same version must not re-notify.
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: p,
    })

    expect(notify).toHaveBeenCalledTimes(1)
    const [title, body, target, options] = notify.mock.calls[0]!
    expect(String(title)).toMatch(/DrawCode/)
    expect(String(body)).toMatch(/stalled/i)
    expect(String(body)).not.toMatch(/rm |sudo |password|raw_input/i)
    expect(target).toEqual({ kind: "conversation", conversationId: 42 })
    // Host multi-window once-per-(lease, version) gate.
    expect(options).toEqual({ dedupeKey: "lease-w1:2" })
  })

  it("conversation_linked updates null conversationId so later watchdog notify has target", async () => {
    // Fresh tab auto-connects before the conversation row exists, so connect
    // starts with conversationId: null. Backend later emits conversation_linked;
    // watchdog notifications must include the linked conversation target.
    const { sendSystemNotification } = await import("@/lib/notification")
    const notify = vi.mocked(sendSystemNotification)
    notify.mockClear()

    await mountProvider()
    await act(async () => {
      // No persisted conversationId at connect time.
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })
    expect(h.store!.getConnection(TAB)?.conversationId ?? null).toBeNull()

    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "conversation_linked",
      conversation_id: 99,
      folder_id: 1,
    })
    expect(h.store!.getConnection(TAB)?.conversationId).toBe(99)

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ version: 1, phase: "warning" }),
    })

    expect(notify).toHaveBeenCalledTimes(1)
    const target = notify.mock.calls[0]![2]
    expect(target).toEqual({ kind: "conversation", conversationId: 99 })
  })

  it("does not notify when document is visible", async () => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => false,
    })
    const { sendSystemNotification } = await import("@/lib/notification")
    const notify = vi.mocked(sendSystemNotification)
    notify.mockClear()

    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ phase: "warning", version: 1 }),
    })
    expect(notify).not.toHaveBeenCalled()
  })

  it("server/web (non-desktop) never dispatches watchdog system notification", async () => {
    h.isDesktop = false
    const { sendSystemNotification } = await import("@/lib/notification")
    const notify = vi.mocked(sendSystemNotification)
    notify.mockClear()

    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "spawned-conn",
      type: "tool_watchdog_changed",
      projection: projection({ phase: "grace", version: 2 }),
    })
    // Banner map still reduces; notification path is skipped.
    expect(
      h.store!.getConnection(TAB)?.toolWatchdogProjections?.["lease-w1"]?.phase
    ).toBe("grace")
    expect(notify).not.toHaveBeenCalled()
  })

  it("notification-navigate selects/focuses the affected conversation", async () => {
    const openTab = vi.fn(async () => true)
    const tabMod = await import("@/stores/tab-store")
    const origGetState = tabMod.useTabStore.getState.bind(tabMod.useTabStore)
    tabMod.useTabStore.getState = (() => ({
      ...origGetState(),
      openTab,
    })) as typeof tabMod.useTabStore.getState

    try {
      useAppWorkspaceStore.getState().applyConversationUpsert(
        makeSummary({
          id: 77,
          folder_id: 1,
          agent_type: "claude_code",
          title: "Stalled session",
        })
      )
      // Ensure folder exists so addFolder path is skipped.
      const folders = useAppWorkspaceStore.getState().folders
      if (!folders.some((f) => f.id === 1)) {
        useAppWorkspaceStore.setState({
          folders: [
            ...folders,
            {
              id: 1,
              name: "x",
              path: "/tmp/x",
              created_at: "",
              updated_at: "",
            } as never,
          ],
          foldersHydrated: true,
        })
      } else {
        useAppWorkspaceStore.setState({ foldersHydrated: true })
      }

      await mountProvider()
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
      })

      const handler = h.subscribeHandlers.get("notification-navigate")
      expect(handler).toBeTruthy()

      await act(async () => {
        handler?.({ kind: "conversation", conversationId: 77 })
        await Promise.resolve()
        await Promise.resolve()
        await Promise.resolve()
      })

      await waitFor(
        () => {
          expect(openTab).toHaveBeenCalledWith(
            1,
            77,
            "claude_code",
            true,
            "Stalled session"
          )
        },
        { timeout: 2000 }
      )
    } finally {
      tabMod.useTabStore.getState = origGetState
    }
  })
})

describe("AcpConnectionsProvider canonical observer aliases", () => {
  it("publishes one canonical state to the tab alias", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    expect(h.store!.getConnection(TAB)).toBe(
      h.store!.getConnection("broker-child")
    )
    expect(h.store!.getConnection(TAB)?.contextKey).toBe("broker-child")
    expect(h.attach).toHaveBeenCalledTimes(1)
  })

  it("snapshots user-stop ownership for every runtime alias", async () => {
    const TAB2 = "conv-2-claude_code-99"
    const {
      __getUserStopOwnershipForTests,
      resetConversationRuntimeStore,
      useConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const live: LiveMessage = {
      id: "shared-cancel-live",
      role: "assistant",
      content: [{ type: "text", text: "partial shared reply" }],
      startedAt: 1_700_000_000_000,
    }
    for (const conversationId of [42, 99]) {
      runtimeActions.setExternalId(conversationId, "sess-shared")
      runtimeActions.appendOptimisticTurn(
        conversationId,
        {
          id: `user-${conversationId}`,
          role: "user",
          blocks: [{ type: "text", text: "cancel shared prompt" }],
          timestamp: "2026-08-25T07:31:49.000Z",
        },
        `turn-${conversationId}`
      )
      runtimeActions.setLiveMessage(conversationId, live, true)
    }

    try {
      h.acpFindConnectionForConversation.mockResolvedValue({
        connection_id: "broker-child",
        event_seq: 0,
      })
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          42
        )
      })
      emitAcpEvent(latestAttachHandlers(), {
        seq: 1,
        connection_id: "broker-child",
        type: "session_started",
        session_id: "sess-shared",
      })
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await act(async () => {
        await h.actions!.connect(
          TAB2,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          99
        )
        await h.actions!.cancel(TAB)
      })

      expect(__getUserStopOwnershipForTests(42)).toMatchObject({
        activeTurnToken: "turn-42",
      })
      expect(__getUserStopOwnershipForTests(99)).toMatchObject({
        activeTurnToken: "turn-99",
      })
      expect(h.acpCancel).toHaveBeenCalledTimes(1)
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("dismisses canonical session failures through a viewer tab alias", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "broker-child",
      type: "session_failure",
      record: {
        id: "viewer-failure",
        revision: 1,
        category: "connection",
        severity: "error",
        title: "Viewer can close this failure",
        actions: ["new_session"],
      },
    })
    expect(h.store!.getConnection(TAB)?.sessionFailures[0]).toMatchObject({
      id: "viewer-failure",
      resolved: false,
    })

    act(() => {
      h.actions!.dismissSessionFailures(TAB, ["viewer-failure"])
    })

    expect(
      h.store!.getConnection("broker-child")?.sessionFailures[0]
    ).toMatchObject({
      id: "viewer-failure",
      resolved: true,
      dismissed: true,
    })
  })

  it("reconnects a viewer alias with the canonical live conversation id", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "broker-child",
      type: "conversation_linked",
      conversation_id: 99,
      folder_id: 1,
    })
    expect(h.store!.getConnection(TAB)?.conversationId).toBe(99)
    h.acpFindConnectionForConversation.mockClear()

    await act(async () => {
      await h.actions!.reconnect(TAB)
    })

    expect(h.acpFindConnectionForConversation).toHaveBeenLastCalledWith(
      99,
      "sid",
      "claude_code"
    )
  })

  it("fans canonical updates to alias listeners and alias live sinks", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    const notify = vi.fn()
    const sink = vi.fn()
    const off = h.store!.subscribeKey(TAB, notify)
    const offSink = h.actions!.registerLiveMessageSink(TAB, sink)
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "broker-child",
      type: "content_delta",
      text: "live child output",
    })

    expect(notify).toHaveBeenCalled()
    // prompting resets liveMessage, then content_delta appends text — both
    // must reach the alias sink registered under the tab key.
    expect(sink.mock.calls.length).toBeGreaterThanOrEqual(2)
    const lastLive = sink.mock.calls[sink.mock.calls.length - 1]![0]
    expect(lastLive.content).toContainEqual({
      type: "text",
      text: "live child output",
    })
    offSink()
    off()
  })

  it("promotes a coalesced completion in every canonical observer alias", async () => {
    const TAB2 = "conv-2-claude_code-99"
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const userTurn = {
      id: "shared-user",
      role: "user" as const,
      blocks: [{ type: "text" as const, text: "shared prompt" }],
      timestamp: "2026-08-25T07:31:49.000Z",
    }
    runtimeActions.setExternalId(42, "sess-shared")
    runtimeActions.appendOptimisticTurn(42, userTurn, userTurn.id)
    runtimeActions.setExternalId(99, "sess-shared")
    runtimeActions.appendOptimisticTurn(99, userTurn, userTurn.id)

    try {
      h.acpFindConnectionForConversation.mockResolvedValue({
        connection_id: "broker-child",
        event_seq: 0,
      })
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          42
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        seq: 1,
        connection_id: "broker-child",
        type: "session_started",
        session_id: "sess-shared",
      })
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await act(async () => {
        await h.actions!.connect(
          TAB2,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          99
        )
      })
      h.actions!.registerLiveMessageSink(TAB, (message, isLive) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      h.actions!.registerLiveMessageSink(TAB2, (message, isLive) => {
        runtimeActions.setLiveMessage(99, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(99)
            ?.liveMessage === message
        )
      })

      act(() => {
        handlers.onReplay(
          [
            {
              seq: 2,
              connection_id: "broker-child",
              type: "user_message",
              message_id: userTurn.id,
              blocks: userTurn.blocks,
            },
            {
              seq: 3,
              connection_id: "broker-child",
              type: "status_changed",
              status: "prompting",
            },
            content("broker-child", 4, "shared reply"),
            {
              seq: 5,
              connection_id: "broker-child",
              type: "turn_complete",
              session_id: "sess-shared",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          5
        )
      })

      for (const conversationId of [42, 99]) {
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(conversationId)
        expect(runtime?.optimisticTurns).toEqual([])
        expect(
          runtime?.localTurns.map((turn) => ({
            role: turn.role,
            blocks: turn.blocks,
          }))
        ).toEqual([
          { role: "user", blocks: [{ type: "text", text: "shared prompt" }] },
          {
            role: "assistant",
            blocks: [{ type: "text", text: "shared reply" }],
          },
        ])
      }
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("projects a terminal only into aliases that own its live message", async () => {
    const TAB2 = "conv-2-claude_code-99"
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-shared")
    runtimeActions.appendOptimisticTurn(
      42,
      {
        id: "user-a",
        role: "user",
        blocks: [{ type: "text", text: "turn A" }],
        timestamp: "2026-08-25T07:31:49.000Z",
      },
      "user-a"
    )
    runtimeActions.setExternalId(99, "sess-shared")
    runtimeActions.appendOptimisticTurn(
      99,
      {
        id: "user-b",
        role: "user",
        blocks: [{ type: "text", text: "turn B" }],
        timestamp: "2026-08-25T07:31:50.000Z",
      },
      "user-b"
    )

    try {
      h.acpFindConnectionForConversation.mockResolvedValue({
        connection_id: "broker-child",
        event_seq: 0,
      })
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          42
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        seq: 1,
        connection_id: "broker-child",
        type: "session_started",
        session_id: "sess-shared",
      })
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await act(async () => {
        await h.actions!.connect(
          TAB2,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          99
        )
      })

      const sink42 = vi.fn((message: LiveMessage, isLive: boolean) => {
        runtimeActions.setLiveMessage(42, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(42)
            ?.liveMessage === message
        )
      })
      const sink99 = vi.fn((message: LiveMessage, isLive: boolean) => {
        runtimeActions.setLiveMessage(99, message, isLive)
        return (
          useConversationRuntimeStore.getState().byConversationId.get(99)
            ?.liveMessage === message
        )
      })
      const sinks42 = { runtimeConversationId: 42, canonical: sink42 }
      const sinks99 = { runtimeConversationId: 99, canonical: sink99 }
      h.actions!.registerLiveSinks(TAB, sinks42)
      h.actions!.registerLiveSinks(TAB2, sinks99)

      act(() => {
        handlers.onReplay(
          [
            {
              seq: 2,
              connection_id: "broker-child",
              type: "status_changed",
              status: "prompting",
            },
            content("broker-child", 3, "reply A"),
          ],
          3
        )
      })

      const replyB: LiveMessage = {
        id: "reply-b",
        role: "assistant",
        content: [{ type: "text", text: "reply B in progress" }],
        startedAt: 1_700_000_000_001,
      }
      runtimeActions.setLiveMessage(99, replyB, true)
      sink42.mockClear()
      sink99.mockClear()

      act(() => {
        handlers.onReplay(
          [
            content("broker-child", 4, " final"),
            {
              seq: 5,
              connection_id: "broker-child",
              type: "turn_complete",
              session_id: "sess-shared",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
          ],
          5
        )
      })

      const runtime42 = useConversationRuntimeStore
        .getState()
        .byConversationId.get(42)
      const runtime99 = useConversationRuntimeStore
        .getState()
        .byConversationId.get(99)
      expect(runtime42?.liveMessage).toBeNull()
      expect(runtime42?.localTurns.at(-1)).toMatchObject({
        role: "assistant",
        blocks: [{ type: "text", text: "reply A final" }],
      })
      expect(runtime99?.liveMessage).toBe(replyB)
      expect(runtime99?.optimisticTurns.map((turn) => turn.id)).toEqual([
        "user-b",
      ])
      expect(runtime99?.localTurns).toEqual([])
      expect(sink42).toHaveBeenCalledTimes(1)
      expect(sink99).not.toHaveBeenCalled()
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("promotes an accepted user-stop completion in every canonical observer alias", async () => {
    const TAB2 = "conv-2-claude_code-99"
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    const { createLiveTranscriptFrameSink, liveTranscriptStore } =
      await import("@/stores/live-transcript-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const userTurn = {
      id: "cancelled-shared-user",
      role: "user" as const,
      blocks: [{ type: "text" as const, text: "cancel shared prompt" }],
      timestamp: "2026-08-25T07:31:49.000Z",
    }
    for (const conversationId of [42, 99]) {
      runtimeActions.setExternalId(conversationId, "sess-shared")
      runtimeActions.appendOptimisticTurn(conversationId, userTurn, userTurn.id)
      noteUserStopTurnOwnership(conversationId)
    }

    try {
      h.acpFindConnectionForConversation.mockResolvedValue({
        connection_id: "broker-child",
        event_seq: 0,
      })
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          42
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        seq: 1,
        connection_id: "broker-child",
        type: "session_started",
        session_id: "sess-shared",
      })
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await act(async () => {
        await h.actions!.connect(
          TAB2,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          99
        )
      })
      h.actions!.registerLiveSinks(TAB, {
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(42, "broker-child"),
      })
      h.actions!.registerLiveSinks(TAB2, {
        canonical: (message, isLive) => {
          runtimeActions.setLiveMessage(99, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(99)
              ?.liveMessage === message
          )
        },
        transcript: createLiveTranscriptFrameSink(99, "broker-child"),
      })

      act(() => {
        handlers.onReplay(
          [
            {
              seq: 2,
              connection_id: "broker-child",
              type: "status_changed",
              status: "prompting",
            },
            content("broker-child", 3, "cancelled shared reply"),
            {
              seq: 4,
              connection_id: "broker-child",
              type: "turn_complete",
              session_id: "sess-shared",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: null,
            },
          ],
          4,
          1
        )
      })

      for (const conversationId of [42, 99]) {
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(conversationId)
        expect(runtime?.optimisticTurns).toEqual([])
        expect(runtime?.liveMessage).toBeNull()
        expect(runtime?.localTurns.at(-1)).toMatchObject({
          role: "assistant",
          blocks: [{ type: "text", text: "cancelled shared reply" }],
        })
        expect(liveTranscriptStore.getConversation(conversationId)).toBeNull()
      }
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("keeps the next turn live in every alias after a stale user-stop completion", async () => {
    const TAB2 = "conv-2-claude_code-99"
    const {
      noteUserStopTurnOwnership,
      useConversationRuntimeStore,
      resetConversationRuntimeStore,
    } = await import("@/stores/conversation-runtime-store")
    const { createLiveTranscriptFrameSink, liveTranscriptStore } =
      await import("@/stores/live-transcript-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    const cancelledReply: LiveMessage = {
      id: "cancelled-alias-reply-a",
      role: "assistant",
      content: [{ type: "text", text: "partial A" }],
      startedAt: 1_700_000_000_000,
    }
    for (const conversationId of [42, 99]) {
      runtimeActions.setExternalId(conversationId, "sess-shared")
      runtimeActions.appendOptimisticTurn(
        conversationId,
        {
          id: `user-a-${conversationId}`,
          role: "user",
          blocks: [{ type: "text", text: "turn A" }],
          timestamp: "2026-08-25T07:31:49.000Z",
        },
        `turn-a-${conversationId}`
      )
      runtimeActions.setLiveMessage(conversationId, cancelledReply, true)
      noteUserStopTurnOwnership(conversationId)
      runtimeActions.completeTurn(conversationId, cancelledReply)
      runtimeActions.appendOptimisticTurn(
        conversationId,
        {
          id: `user-b-${conversationId}`,
          role: "user",
          blocks: [{ type: "text", text: "turn B" }],
          timestamp: "2026-08-25T07:31:50.000Z",
        },
        `turn-b-${conversationId}`
      )
    }

    try {
      h.acpFindConnectionForConversation.mockResolvedValue({
        connection_id: "broker-child",
        event_seq: 0,
      })
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          42
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        seq: 1,
        connection_id: "broker-child",
        type: "session_started",
        session_id: "sess-shared",
      })
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await act(async () => {
        await h.actions!.connect(
          TAB2,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          99
        )
      })
      for (const [tabId, conversationId] of [
        [TAB, 42],
        [TAB2, 99],
      ] as const) {
        h.actions!.registerLiveSinks(tabId, {
          canonical: (message, isLive) => {
            runtimeActions.setLiveMessage(conversationId, message, isLive)
            return (
              useConversationRuntimeStore
                .getState()
                .byConversationId.get(conversationId)?.liveMessage === message
            )
          },
          transcript: createLiveTranscriptFrameSink(
            conversationId,
            "broker-child"
          ),
        })
      }

      act(() => {
        handlers.onReplay(
          [
            {
              seq: 2,
              connection_id: "broker-child",
              type: "status_changed",
              status: "prompting",
            },
            content("broker-child", 3, "reply B in progress"),
          ],
          3
        )
      })
      const liveByConversation = new Map(
        [42, 99].map((conversationId) => [
          conversationId,
          useConversationRuntimeStore
            .getState()
            .byConversationId.get(conversationId)?.liveMessage,
        ])
      )

      act(() => {
        handlers.onReplay(
          [
            {
              seq: 4,
              connection_id: "broker-child",
              type: "turn_complete",
              session_id: "sess-shared",
              stop_reason: "cancelled",
              mark_awaiting_reply: false,
              termination_source: "user_stop",
              provider_turn_id: "provider-a",
            },
          ],
          4
        )
      })

      for (const conversationId of [42, 99]) {
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(conversationId)
        expect(runtime?.activeTurnToken).toBe(`turn-b-${conversationId}`)
        expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
          `user-b-${conversationId}`,
        ])
        expect(runtime?.liveMessage).toBe(
          liveByConversation.get(conversationId)
        )
        expect(
          liveTranscriptStore.getConversation(conversationId)?.status
        ).toBe("streaming")
      }
    } finally {
      resetConversationRuntimeStore()
    }
  })

  it("merges delegation metadata into an existing canonical viewer", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    const original = h.store!.getConnection(TAB)

    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
    })

    const merged = h.store!.getConnection(TAB)
    expect(merged?.liveMessage).toBe(original?.liveMessage)
    expect(merged).toMatchObject({
      isViewer: true,
      isDelegationChild: true,
      parentConnectionId: "parent",
      parentToolUseId: "tool-1",
    })
    expect(h.attach).toHaveBeenCalledTimes(1)
  })

  it("retains observer state after delegation detach and never disconnects it", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
      h.actions!.detachDelegationChild("broker-child")
    })

    expect(h.store!.getConnection(TAB)).toMatchObject({
      connectionId: "broker-child",
      isViewer: true,
      isDelegationChild: false,
      parentConnectionId: null,
      parentToolUseId: null,
    })
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("resolves alias to canonical connection for interactive actions", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "session_load_failed",
      session_id: "sid",
      message: "load failed for alias clear test",
      code: "legacy_cli_session",
    })
    expect(h.store!.getConnection(TAB)?.loadError).toBeTruthy()

    await act(async () => {
      await h.actions!.sendPrompt(TAB, [{ type: "text", text: "hi" }])
      await h.actions!.setMode(TAB, "default")
      await h.actions!.setConfigOption(TAB, "model", "x")
      await h.actions!.cancel(TAB)
      await h.actions!.respondPermission(TAB, "req-1", "allow")
      await h.actions!.answerQuestion(TAB, "q-1", {
        answers: [{ questionId: "q-1", selectedOptionIds: ["a"] }],
      } as never)
      h.actions!.clearAcpLoadError(TAB)
    })

    expect(acpPromptMock).toHaveBeenCalledWith(
      "broker-child",
      expect.anything(),
      null,
      null,
      null,
      expect.anything()
    )
    expect(acpSetModeMock).toHaveBeenCalledWith("broker-child", "default")
    expect(acpSetConfigOptionMock).toHaveBeenCalledWith(
      "broker-child",
      "model",
      "x"
    )
    expect(h.acpCancel).toHaveBeenCalledWith("broker-child")
    expect(acpRespondPermissionMock).toHaveBeenCalledWith(
      "broker-child",
      "req-1",
      "allow"
    )
    expect(acpAnswerQuestionMock).toHaveBeenCalledWith(
      "broker-child",
      "q-1",
      expect.anything()
    )
    expect(h.store!.getConnection(TAB)?.loadError).toBeNull()
  })

  it("never calls acpTouchConnection for observer alias activity or keepalive", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    // Bring the canonical entry to connected so keepalive would touch owners.
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "connected",
    })

    act(() => {
      h.actions!.setActiveKey(TAB)
      h.actions!.registerOpenTabKeys(new Set([TAB]))
      h.actions!.touchActivity(TAB)
    })

    await act(async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true })
      try {
        await vi.advanceTimersByTimeAsync(30_000)
      } finally {
        vi.useRealTimers()
      }
    })

    expect(acpTouchConnectionMock).not.toHaveBeenCalled()
  })

  it("connection_gone clears tab aliases so owner reconnect under tab id works", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection("broker-child")).toBeTruthy()

    const handlers = latestAttachHandlers()
    act(() => {
      handlers.onDetached("connection_gone")
    })

    // Canonical state is gone, and the tab alias must not keep resolving to it.
    expect(h.store!.getConnection("broker-child")).toBeUndefined()
    expect(h.store!.getConnection(TAB)).toBeUndefined()

    // Broker is gone — discovery returns null so connect spawns an owner under TAB.
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-after-gone")
    h.attach.mockClear()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    expect(h.acpConnect).toHaveBeenCalled()
    const reconnected = h.store!.getConnection(TAB)
    expect(reconnected).toBeTruthy()
    expect(reconnected?.connectionId).toBe("owner-after-gone")
    expect(reconnected?.isViewer).toBeFalsy()
    // Owner state is keyed by the tab id (not a stale broker alias target).
    expect(reconnected?.contextKey).toBe(TAB)
  })

  it("snapshot_budget_exceeded keeps canonical connection in a recoverable attach error", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    expect(h.store!.getConnection("broker-child")).toBeTruthy()
    expect(h.attach).toHaveBeenCalledTimes(1)

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })

    const conn = h.store!.getConnection("broker-child")
    expect(conn).toBeTruthy()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(conn?.attachError).toEqual({
      code: "snapshot_budget_exceeded",
      retryable: true,
    })
    expect(h.attach).toHaveBeenCalledTimes(1)

    act(() => {
      h.actions!.retryAttach("broker-child")
    })
    expect(h.attach).toHaveBeenCalledTimes(2)
    expect(h.attach.mock.calls.at(-1)?.[1]).toEqual({ reconnectMode: "cold" })
    expect(h.store!.getConnection("broker-child")?.attachError).toBeNull()
    expect(h.store!.getConnection("broker-child")).toBeTruthy()

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })
    expect(h.attach).toHaveBeenCalledTimes(2)

    act(() => {
      h.fireReconnect()
    })
    expect(h.attach).toHaveBeenCalledTimes(3)
    expect(h.attach.mock.calls.at(-1)?.[1]).toEqual({ reconnectMode: "cold" })

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })
    act(() => {
      h.fireReconnect()
    })
    expect(h.attach).toHaveBeenCalledTimes(3)
  })

  it("clears a parked attach retry record on disconnect so a new session can auto-retry", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })
    act(() => {
      h.fireReconnect()
    })
    expect(h.attach).toHaveBeenCalledTimes(2)

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    expect(h.attach).toHaveBeenCalledTimes(3)

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })
    act(() => {
      h.fireReconnect()
    })
    expect(h.attach).toHaveBeenCalledTimes(4)
  })

  it("retries snapshot_budget_exceeded as a cold attach instead of sinceSeq 0", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("spawned-conn")
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    expect(h.attach.mock.calls.at(-1)?.[1]).toEqual({})

    act(() => {
      latestAttachHandlers().onAttachError("snapshot_budget_exceeded", true)
    })
    act(() => {
      h.actions!.retryAttach(TAB)
    })
    expect(h.attach.mock.calls.at(-1)?.[1]).toEqual({ reconnectMode: "cold" })
  })

  it("orphan rescue does not rekey viewer off connectionId; second observer reuses state", async () => {
    const TAB2 = "conv-2-claude_code-99"
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-shared", 42)
    })
    expect(h.store!.getConnection(TAB)?.contextKey).toBe("broker-child")
    expect(h.attach).toHaveBeenCalledTimes(1)

    // Snapshot hydration stamps sessionId so a later tab with the same
    // sessionId would historically trigger orphan rescue rekey.
    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "broker-child",
      type: "session_started",
      session_id: "sess-shared",
    })
    expect(h.store!.getConnection(TAB)?.sessionId).toBe("sess-shared")

    // Second tab + same sessionId: must alias the canonical connectionId
    // entry, never rekey it onto TAB2 (that would strand connectAsViewer
    // lookups and create a second attach subscription).
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await act(async () => {
      await h.actions!.connect(TAB2, "claude_code", "/tmp/x", "sess-shared", 99)
    })

    expect(h.store!.getConnection("broker-child")?.connectionId).toBe(
      "broker-child"
    )
    expect(h.store!.getConnection("broker-child")?.contextKey).toBe(
      "broker-child"
    )
    expect(h.store!.getConnection(TAB)).toBe(
      h.store!.getConnection("broker-child")
    )
    expect(h.store!.getConnection(TAB2)).toBe(
      h.store!.getConnection("broker-child")
    )
    expect(h.store!.getConnection(TAB2)?.isViewer).toBe(true)
    expect(h.attach).toHaveBeenCalledTimes(1)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("orphan rescue still rekeys true tab-keyed owner orphans", async () => {
    const ORPHAN = "new-orphan-tab"
    const TAB2 = "conv-2-claude_code-99"
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-conn")
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(ORPHAN, "claude_code", "/tmp/x", "sess-shared")
    })
    expect(h.store!.getConnection(ORPHAN)?.connectionId).toBe("owner-conn")
    expect(h.store!.getConnection(ORPHAN)?.contextKey).toBe(ORPHAN)

    emitAcpEvent(latestAttachHandlers(), {
      seq: 1,
      connection_id: "owner-conn",
      type: "session_started",
      session_id: "sess-shared",
    })

    await act(async () => {
      await h.actions!.connect(TAB2, "claude_code", "/tmp/x", "sess-shared", 99)
    })

    expect(h.store!.getConnection(ORPHAN)).toBeUndefined()
    expect(h.store!.getConnection(TAB2)?.connectionId).toBe("owner-conn")
    expect(h.store!.getConnection(TAB2)?.contextKey).toBe(TAB2)
    // Owner orphans rekey; they do not create a second backend agent.
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
  })

  it("second tab aliases the same canonical after snapshot hydration", async () => {
    const TAB2 = "conv-2-claude_code-99"
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-shared", 42)
    })

    // Apply post-attach live state (session + content) before a second tab
    // joins — models the “after snapshot hydration” window.
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "session_started",
      session_id: "sess-shared",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "broker-child",
      type: "status_changed",
      status: "prompting",
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "broker-child",
      type: "content_delta",
      text: "hydrated",
    })
    expect(h.store!.getConnection(TAB)?.sessionId).toBe("sess-shared")
    expect(h.store!.getConnection(TAB)?.liveMessage?.content).toContainEqual({
      type: "text",
      text: "hydrated",
    })

    // Same sessionId still aliases (must not rekey the connectionId entry).
    await act(async () => {
      await h.actions!.connect(TAB2, "claude_code", "/tmp/x", "sess-shared", 99)
    })

    expect(h.store!.getConnection(TAB)).toBe(h.store!.getConnection(TAB2))
    expect(h.store!.getConnection(TAB2)?.contextKey).toBe("broker-child")
    expect(h.store!.getConnection(TAB2)?.liveMessage?.content).toContainEqual({
      type: "text",
      text: "hydrated",
    })
    expect(h.attach).toHaveBeenCalledTimes(1)
  })

  it("sequence-gap dead canonical removal clears tab aliases", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "connected",
    })
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(1)

    h.acpGetSessionSnapshot.mockResolvedValue(null)
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "broker-child",
      type: "content_delta",
      text: "gap-skip",
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.store!.getConnection("broker-child")).toBeUndefined()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("stale gap recovery resumes from the newer accepted cursor", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    h.denormalizeSnapshot.mockImplementation(
      (snapshot: { connection_id: string; event_seq: number }) =>
        estimatorSnapshotPatch({
          connectionId: snapshot.connection_id,
          conversationId: 42,
          eventSeq: snapshot.event_seq,
        })
    )
    let resolveGapSnapshot: (snapshot: LiveSessionSnapshot) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementationOnce(
      () =>
        new Promise<LiveSessionSnapshot>((resolve) => {
          resolveGapSnapshot = resolve
        })
    )
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "prompting",
    })

    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "broker-child",
      type: "content_delta",
      text: "gap-buffered",
    })
    await act(async () => {
      await Promise.resolve()
    })
    expect(h.acpGetSessionSnapshot).toHaveBeenCalledTimes(1)

    hydrateSnapshot(handlers, {
      connection_id: "broker-child",
      event_seq: 4,
    } as LiveSessionSnapshot)
    emitAcpEvent(handlers, {
      seq: 5,
      connection_id: "broker-child",
      type: "content_delta",
      text: "newer-live",
    })
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(5)

    await act(async () => {
      resolveGapSnapshot({
        connection_id: "broker-child",
        event_seq: 2,
      } as LiveSessionSnapshot)
      await Promise.resolve()
      await Promise.resolve()
    })
    emitAcpEvent(handlers, {
      seq: 6,
      connection_id: "broker-child",
      type: "content_delta",
      text: "after-stale-recovery",
    })

    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(6)
    expect(h.acpGetSessionSnapshot).toHaveBeenCalledTimes(1)
  })

  it("sequence-gap null recovery re-resolves key after interleaved owner orphan rekey", async () => {
    const ORPHAN = "new-orphan-tab"
    const TAB2 = "conv-2-claude_code-99"
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-conn")
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(ORPHAN, "claude_code", "/tmp/x", "sess-shared")
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "owner-conn",
      type: "session_started",
      session_id: "sess-shared",
    })
    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "owner-conn",
      type: "status_changed",
      status: "connected",
    })
    expect(h.store!.getConnection(ORPHAN)?.lastAppliedSeq).toBe(2)
    expect(h.store!.getConnection(ORPHAN)?.sessionId).toBe("sess-shared")

    // Hold recovery mid-await so orphan rescue can rekey first.
    let resolveSnapshot: (value: null) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSnapshot = resolve
        })
    )

    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "owner-conn",
      type: "content_delta",
      text: "gap-skip",
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    // Interleaved rekey: owner moves from ORPHAN → TAB2 while sequence-gap
    // recovery is still awaiting the null snapshot.
    await act(async () => {
      await h.actions!.connect(TAB2, "claude_code", "/tmp/x", "sess-shared", 99)
    })
    expect(h.store!.getConnection(ORPHAN)).toBeUndefined()
    expect(h.store!.getConnection(TAB2)?.connectionId).toBe("owner-conn")
    expect(h.store!.getConnection(TAB2)?.contextKey).toBe(TAB2)

    // Null snapshot must remove the *current* key (TAB2), not the stale
    // pre-await gap.contextKey (ORPHAN).
    await act(async () => {
      resolveSnapshot(null)
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.store!.getConnection(ORPHAN)).toBeUndefined()
    expect(h.store!.getConnection(TAB2)).toBeUndefined()
  })

  it("sequence-gap null recovery does not remove replacement under same tab key", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("conn-A")
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-A", 42)
    })
    const handlersA = latestAttachHandlers()
    emitAcpEvent(handlersA, {
      seq: 1,
      connection_id: "conn-A",
      type: "status_changed",
      status: "connected",
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("conn-A")
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(1)

    // Hold recovery mid-await so a replacement reconnect can land first.
    let resolveSnapshot: (value: null) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSnapshot = resolve
        })
    )

    emitAcpEvent(handlersA, {
      seq: 3,
      connection_id: "conn-A",
      type: "content_delta",
      text: "gap-skip",
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    // Replace A with B under the same tab key while recovery is in flight.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    h.acpConnect.mockResolvedValue("conn-B")
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-B", 42)
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("conn-B")

    // Null snapshot for dead A must not wipe the live replacement B.
    await act(async () => {
      resolveSnapshot(null)
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.store!.getConnection(TAB)?.connectionId).toBe("conn-B")
    expect(h.store!.getConnection(TAB)?.contextKey).toBe(TAB)
  })

  it("sequence-gap null recovery fires handoff re-entry for watched broker", async () => {
    // Task 5 r4: gap null snapshot removes the dead canonical without
    // connection_gone; handoff watchers must still re-invoke own_or_observe.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 1,
    })
    h.acpConnect.mockClear()
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await handoff
    })
    vi.useRealTimers()

    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)

    // Contiguous status so lastAppliedSeq advances; then gap with null snapshot.
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "connected",
    })
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(1)

    h.acpGetSessionSnapshot.mockResolvedValue(null)
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-after-gap-null")
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "broker-child",
      type: "content_delta",
      text: "gap-skip",
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    await waitFor(() => {
      expect(h.acpConnect).toHaveBeenCalled()
    })
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sid",
      undefined,
      {},
      42,
      null,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe(
      "owner-after-gap-null"
    )
    expect(h.store!.getConnection(TAB)?.isViewer).toBeFalsy()
  })

  it("sequence-gap rejected-snapshot recovery does not acpConnect for discovery errors", async () => {
    // Task 5 r5: snapshot throw is not confirmed-dead. Cleanup local state but
    // do not fire handoff re-entry (which would claim ownership via acpConnect)
    // while the broker may still be live.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 1,
    })
    h.acpConnect.mockClear()
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await handoff
    })
    vi.useRealTimers()

    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)

    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "connected",
    })
    expect(h.store!.getConnection(TAB)?.lastAppliedSeq).toBe(1)

    h.acpGetSessionSnapshot.mockRejectedValue(
      new Error("malformed discovery payload")
    )
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-after-gap-throw")
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "broker-child",
      type: "content_delta",
      text: "gap-skip",
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    // Allow any queued handoff microtask a chance to run if incorrectly fired.
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.acpConnect).not.toHaveBeenCalled()
    // Local dead-entry cleanup still runs; ownership is not claimed.
    expect(h.store!.getConnection("broker-child")).toBeUndefined()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("desktop delivery-failure dead snapshot clears tab aliases", async () => {
    h.eventStreamValue = null
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    h.acpGetSessionSnapshot.mockResolvedValue(null)
    await mountProvider()
    // Desktop capability subscribe settles async.
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")

    act(() => {
      h.emitDesktopFailure({
        generation: 1,
        reason: "batch_emit_failed",
        affected: [
          { connection_id: "broker-child", first_seq: 1, last_seq: 3 },
        ],
      })
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.store!.getConnection("broker-child")).toBeUndefined()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })
})

describe("isRetryableObserverDiscoveryError", () => {
  it("classifies transport timeout as retryable", () => {
    expect(
      isRetryableObserverDiscoveryError(new Error("Request timed out"))
    ).toBe(true)
  })

  it("classifies network reset as retryable", () => {
    expect(
      isRetryableObserverDiscoveryError(new Error("read ECONNRESET"))
    ).toBe(true)
  })

  it("classifies HTTP 5xx as retryable", () => {
    expect(isRetryableObserverDiscoveryError({ status: 503 })).toBe(true)
    expect(
      isRetryableObserverDiscoveryError({
        code: "http_500",
        message: "Internal Server Error",
      })
    ).toBe(true)
  })

  it("classifies temporary not-ready as retryable", () => {
    expect(
      isRetryableObserverDiscoveryError(new Error("service not ready"))
    ).toBe(true)
  })

  it("classifies auth 401/403 as non-retryable", () => {
    expect(isRetryableObserverDiscoveryError({ status: 401 })).toBe(false)
    expect(isRetryableObserverDiscoveryError({ status: 403 })).toBe(false)
    expect(
      isRetryableObserverDiscoveryError({
        code: "unauthorized",
        message: "Unauthorized",
      })
    ).toBe(false)
  })

  it("classifies permanent not-found as non-retryable", () => {
    expect(isRetryableObserverDiscoveryError({ status: 404 })).toBe(false)
    expect(
      isRetryableObserverDiscoveryError({
        code: "conversation_not_found",
        message: "Conversation not found",
      })
    ).toBe(false)
  })

  it("classifies malformed payload as non-retryable", () => {
    expect(
      isRetryableObserverDiscoveryError(new Error("malformed payload"))
    ).toBe(false)
  })

  it("classifies nested/response HTTP 5xx as retryable when status available", () => {
    expect(
      isRetryableObserverDiscoveryError({ response: { status: 502 } })
    ).toBe(true)
    expect(
      isRetryableObserverDiscoveryError({ cause: { statusCode: 503 } })
    ).toBe(true)
    expect(isRetryableObserverDiscoveryError({ code: "HTTP_504" })).toBe(true)
  })

  it("classifies nested HTTP 401 as non-retryable", () => {
    expect(
      isRetryableObserverDiscoveryError({ response: { status: 401 } })
    ).toBe(false)
  })
})

describe("ACP event metrics compatibility", () => {
  it("keeps legacy counters flat while accepting the additive broker snapshot", () => {
    const legacy: EventBusMetricsSnapshot = {
      emitted_count: 1,
      lagged_count: 2,
      ring_buffer_evict_count: 3,
      replay_count: 4,
      replay_event_total: 5,
      snapshot_fallback_count: 6,
      snapshot_cold_count: 7,
      forwarder_lagged_count: 8,
      worker_queue_full_count: 9,
      critical_lane_emit_count: 10,
      critical_lane_full_count: 11,
      desktop_raw_envelope_count: 12,
      desktop_raw_bytes: 13,
      desktop_emit_attempt_count: 14,
      desktop_serialization_failure_count: 15,
      desktop_emit_failure_count: 16,
      desktop_legacy_emit_count: 17,
      desktop_batch_count: 18,
      desktop_batch_event_count: 19,
      desktop_batch_bytes: 20,
      desktop_batch_max_events: 21,
      desktop_batch_max_bytes: 22,
      desktop_batch_latency_total_us: 23,
      desktop_batch_latency_max_us: 24,
      desktop_queue_full_count: 25,
      desktop_startup_fallback_count: 26,
      desktop_runtime_failure_count: 27,
    }
    const current: AcpEventMetricsSnapshot = {
      ...legacy,
      shared_session_broker: {
        created_total: 1,
        attached_total: 0,
        live_sessions: 1,
        active_leases: 1,
        bootstrap_ready_total: 0,
        bootstrap_failed_total: {},
        bootstrap_duration_ms_total: 0,
        bootstrap_duration_samples: 0,
        waiting_prompts: 0,
        waiting_bytes: 0,
        enqueue_total: 0,
        cancel_total: 0,
        dispatch_total: 0,
        capacity_rejected_total: 0,
        queue_item_failed_total: 0,
        interaction_winner_total: 0,
        interaction_stale_total: 0,
        stale_stop_total: 0,
        lease_expired_total: 0,
        lease_released_total: 0,
        idle_candidate_total: 0,
        idle_cas_lost_total: 0,
        idle_reclaimed_total: 0,
        cleanup_duration_ms_total: 0,
        cleanup_duration_samples: 0,
        cleanup_incomplete_total: 0,
      },
    }

    expect(legacy.emitted_count).toBe(1)
    expect(legacy.shared_session_broker).toBeUndefined()
    expect(current.emitted_count).toBe(1)
    expect(current.shared_session_broker.live_sessions).toBe(1)
  })
})

describe("isValidConversationConnectionInfo", () => {
  it("accepts well-formed discovery payloads", () => {
    expect(
      isValidConversationConnectionInfo({
        connection_id: "broker-1",
        event_seq: 0,
      })
    ).toBe(true)
  })

  it("rejects missing/empty connection_id and non-finite event_seq", () => {
    expect(isValidConversationConnectionInfo(null)).toBe(false)
    expect(
      isValidConversationConnectionInfo({ connection_id: "", event_seq: 0 })
    ).toBe(false)
    expect(
      isValidConversationConnectionInfo({
        connection_id: "x",
        event_seq: Number.NaN,
      })
    ).toBe(false)
    expect(isValidConversationConnectionInfo({ connection_id: "x" })).toBe(
      false
    )
  })
})

describe("AcpConnectionsProvider observe_existing intent", () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  function snapshotPatch(overrides: {
    eventSeq: number
    lastError: string | null
    lastErrorDetails?: string | null
    connectionId?: string
  }) {
    return {
      connectionId: "spawned-conn",
      status: "connected",
      sessionId: null,
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      backgroundOutstanding: 0,
      activeDelegations: [],
      lastErrorDetails: null,
      ...overrides,
    }
  }

  async function connectOwner(): Promise<AttachHandlers> {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpGetAgentStatus.mockResolvedValue({
      agent_type: "claude_code",
      enabled: true,
      available: true,
      installed_version: "1.0.0",
      host_tools_agent_mode: false,
      is_acp_adapter: true,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    return latestAttachHandlers()
  }

  it("observe_existing branches before SDK preflight and never spawns", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.acpGetAgentStatus).not.toHaveBeenCalled()
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(1)
  })

  it("discovers a child that appears inside the bounded spawn window", async () => {
    vi.useFakeTimers()
    h.acpFindConnectionForConversation
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({ connection_id: "broker-child", event_seq: 0 })
    await mountProvider()
    let pending!: Promise<void>
    await act(async () => {
      pending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300)
      await pending
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("keeps an admitted owner turn streaming when a later parent turn relocks it", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sid", 42)
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "spawned-conn",
      type: "status_changed",
      status: "prompting",
    })
    h.acpDisconnect.mockClear()
    h.acpCancel.mockClear()
    h.acpGetAgentStatus.mockClear()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(false)
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpCancel).not.toHaveBeenCalled()
    expect(h.acpGetAgentStatus).not.toHaveBeenCalled()

    emitAcpEvent(handlers, {
      seq: 2,
      connection_id: "spawned-conn",
      type: "content_delta",
      text: "reply after parent relock",
    })
    emitAcpEvent(handlers, {
      seq: 3,
      connection_id: "spawned-conn",
      type: "usage_update",
      used: 1,
      size: 100,
    })
    expect(h.store!.getConnection(TAB)?.liveMessage?.content).toEqual([
      { type: "text", text: "reply after parent relock" },
    ])

    emitAcpEvent(handlers, {
      seq: 4,
      connection_id: "spawned-conn",
      type: "turn_complete",
      session_id: "sid",
      stop_reason: "end_turn",
      mark_awaiting_reply: false,
    })
    expect(h.store!.getConnection(TAB)?.status).toBe("connected")
    expect(h.acpDisconnect).not.toHaveBeenCalled()
    expect(h.acpCancel).not.toHaveBeenCalled()
  })

  it("retries discovery across all five delays when retryObserverDiscovery is true", async () => {
    vi.useFakeTimers()
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    let pending!: Promise<void>
    await act(async () => {
      pending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    // Delays: 0, 300, 700, 1500, 2500 — advance past the full schedule.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await pending
    })
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(5)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("retryObserverDiscovery false does exactly one lookup", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(1)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("disconnect cancels in-flight observer discovery polling", async () => {
    vi.useFakeTimers()
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    let pending!: Promise<void>
    await act(async () => {
      pending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    // After first (delay 0) lookup, disconnect cancels remaining delays.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await h.actions!.disconnect(TAB)
      await vi.advanceTimersByTimeAsync(10_000)
      await pending
    })
    expect(h.acpFindConnectionForConversation.mock.calls.length).toBeLessThan(5)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("disconnect during in-flight discovery marks abandoned and does not re-bind after lookup", async () => {
    // Mid-lookup disconnect must set abandonedKeys even when an observer
    // alias exists: cancelObserverDelay alone is insufficient because the
    // await is on discovery, not a delay timer.
    let resolveLookup:
      | ((value: { connection_id: string; event_seq: number }) => void)
      | null = null
    h.acpFindConnectionForConversation.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveLookup = resolve
        })
    )
    await mountProvider()

    // First establish a live observer alias.
    h.acpFindConnectionForConversation.mockResolvedValueOnce({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")

    // Re-run observe_existing while holding the discovery lookup open.
    h.acpFindConnectionForConversation.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveLookup = resolve
        })
    )
    let pending!: Promise<void>
    await act(async () => {
      pending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    // Delay 0 fires immediately; connect is now awaiting discovery.
    await act(async () => {
      await Promise.resolve()
    })
    expect(resolveLookup).toBeTruthy()

    // Tab closes mid-lookup: must mark abandoned so the late lookup cannot
    // re-bind after releaseObserverAlias.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    expect(h.store!.getConnection(TAB)).toBeUndefined()

    h.attach.mockClear()
    await act(async () => {
      resolveLookup?.({ connection_id: "broker-child", event_seq: 1 })
      await pending
    })
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    expect(h.attach).not.toHaveBeenCalled()
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("intent supersession aborts observe polling when request changes", async () => {
    vi.useFakeTimers()
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    let observePending!: Promise<void>
    await act(async () => {
      observePending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    // Supersede with own_or_observe while observe is mid-poll.
    let ownerPending!: Promise<void>
    await act(async () => {
      ownerPending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000)
      await observePending
      await ownerPending
    })
    expect(h.acpConnect).toHaveBeenCalled()
  })

  it("stops observe discovery immediately on non-retryable auth error", async () => {
    h.acpFindConnectionForConversation.mockRejectedValue({
      status: 401,
      message: "Unauthorized",
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(1)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("retries observe discovery on retryable timeout errors", async () => {
    vi.useFakeTimers()
    h.acpFindConnectionForConversation
      .mockRejectedValueOnce(new Error("Request timed out"))
      .mockResolvedValueOnce({ connection_id: "broker-child", event_seq: 0 })
    await mountProvider()
    let pending!: Promise<void>
    await act(async () => {
      pending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300)
      await pending
    })
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(2)
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("handoff waits for broker null before acpConnect and never disconnects broker", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    const firstDetach = (
      h.attach.mock.results[0]?.value as { detach: ReturnType<typeof vi.fn> }
    )?.detach
    expect(firstDetach).toBeTruthy()

    // Two still-alive polls, then confirmed disappearance. Stay null after so
    // the post-handoff owner discovery path does not re-attach as viewer.
    let handoffLookups = 0
    h.acpFindConnectionForConversation.mockImplementation(async () => {
      handoffLookups += 1
      if (handoffLookups <= 2) {
        return { connection_id: "broker-child", event_seq: handoffLookups }
      }
      return null
    })
    h.acpConnect.mockClear()
    h.acpDisconnect.mockClear()
    h.attach.mockClear()

    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    // Alias release happens first; then delays 0, 300, 700 until null.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await handoff
    })

    expect(firstDetach).toHaveBeenCalled()
    // Never pass the old broker id to acpDisconnect at all.
    for (const call of h.acpDisconnect.mock.calls) {
      expect(call[0]).not.toBe("broker-child")
    }
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sid",
      undefined,
      {},
      42,
      null,
      null
    )
  })

  it("handoff confirmed-null owns even when stale isDelegationChild entry remains from grace", async () => {
    // Reverse-order: parent attach first leaves a connectionId-keyed
    // isDelegationChild entry. Observer alias release preserves it (grace
    // still has isDelegationChild). Confirmed-null handoff must not orphan-
    // rescue reattach that dead broker — it must proceed to owner spawn.
    await mountProvider()
    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
    })
    // Seed sessionId so orphan rescue would match if the stale entry survives.
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "session_started",
      session_id: "sid",
    })
    expect(h.store!.getConnection("broker-child")?.isDelegationChild).toBe(true)
    expect(h.store!.getConnection("broker-child")?.sessionId).toBe("sid")

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection("broker-child")?.isDelegationChild).toBe(true)

    // Handoff: first poll still "alive" under the grace-retained entry shape,
    // then confirmed null (broker gone). Stay null for post-handoff discovery.
    let handoffLookups = 0
    h.acpFindConnectionForConversation.mockImplementation(async () => {
      handoffLookups += 1
      if (handoffLookups <= 1) {
        return { connection_id: "broker-child", event_seq: handoffLookups }
      }
      return null
    })
    h.acpConnect.mockClear()
    h.acpConnect.mockResolvedValue("owner-after-grace")
    h.acpDisconnect.mockClear()
    h.attach.mockClear()

    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await handoff
    })
    vi.useRealTimers()

    // Owner spawn — not viewer re-bind to dead broker-child.
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sid",
      undefined,
      {},
      42,
      null,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("owner-after-grace")
    expect(h.store!.getConnection(TAB)?.isViewer).toBeFalsy()
    expect(h.store!.getConnection("broker-child")).toBeUndefined()
    for (const call of h.acpDisconnect.mock.calls) {
      expect(call[0]).not.toBe("broker-child")
    }
  })

  it("reverse-order: observe_existing replaces resume subscription with cold", async () => {
    await mountProvider()
    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
    })
    expect(h.attach).toHaveBeenCalledTimes(1)
    const firstDetach = (
      h.attach.mock.results[0]?.value as { detach: ReturnType<typeof vi.fn> }
    )?.detach
    expect(firstDetach).toBeTruthy()

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    h.attach.mockClear()

    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    expect(firstDetach).toHaveBeenCalled()
    expect(h.attach).toHaveBeenCalledTimes(1)
    expect(h.attach).toHaveBeenCalledWith(
      "broker-child",
      { sinceSeq: undefined, reconnectMode: "cold" },
      expect.anything()
    )
    // Canonical state preserved (same object identity via connectionId key).
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection("broker-child")?.isDelegationChild).toBe(true)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("handoff re-attaches when broker still alive, then owner connects on CONNECTION_REMOVED", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    // Handoff: discovery always returns same broker → re-attach as observer.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 1,
    })
    h.acpConnect.mockClear()
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await handoff
    })
    vi.useRealTimers()

    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)

    // Broker disappears → one-shot watcher re-invokes own_or_observe without focus.
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-after-handoff")
    const handlers = latestAttachHandlers()
    await act(async () => {
      handlers.onDetached("connection_gone")
      // Flush microtask queue for handoff watcher.
      await Promise.resolve()
      await Promise.resolve()
    })

    await waitFor(() => {
      expect(h.acpConnect).toHaveBeenCalled()
    })
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sid",
      undefined,
      {},
      42,
      null,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe(
      "owner-after-handoff"
    )
    expect(h.store!.getConnection(TAB)?.isViewer).toBeFalsy()
  })

  it("handoff auth error re-attaches observer and owner connects after broker removed", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockRejectedValue({
      status: 401,
      message: "Unauthorized",
    })
    h.acpConnect.mockClear()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-after-auth")
    const handlers = latestAttachHandlers()
    await act(async () => {
      handlers.onDetached("connection_gone")
      await Promise.resolve()
      await Promise.resolve()
    })
    await waitFor(() => {
      expect(h.acpConnect).toHaveBeenCalled()
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("owner-after-auth")
  })

  it("handoff discovers different live broker id and watches that id for removal", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-a",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-b",
      event_seq: 0,
    })
    h.acpConnect.mockClear()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-b")

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-from-b")
    // Detach for the watched broker-b subscription (latest attach).
    const handlers = latestAttachHandlers()
    await act(async () => {
      handlers.onDetached("connection_gone")
      await Promise.resolve()
      await Promise.resolve()
    })
    await waitFor(() => {
      expect(h.acpConnect).toHaveBeenCalled()
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("owner-from-b")
  })

  it("handoff re-entry fires on status_changed(disconnected) for watched broker (desktop path)", async () => {
    // Desktop has no EventStream onDetached — broker exit is status_changed.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 1,
    })
    h.acpConnect.mockClear()
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await handoff
    })
    vi.useRealTimers()

    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-after-disconnect-status")
    const handlers = latestAttachHandlers()
    // status_changed(disconnected) is the desktop firehose signal for broker exit.
    // Seq must be contiguous from lastAppliedSeq (0 → 1) or EventIngestor gaps.
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "status_changed",
      status: "disconnected",
    })
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    await waitFor(() => {
      expect(h.acpConnect).toHaveBeenCalled()
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe(
      "owner-after-disconnect-status"
    )
  })

  it("identical concurrent connect during handoff settle does not strand the tab", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    // Handoff: first poll still alive; disappearance after the duplicate connect.
    let lookups = 0
    h.acpFindConnectionForConversation.mockImplementation(async () => {
      lookups += 1
      if (lookups <= 2) {
        return { connection_id: "broker-child", event_seq: lookups }
      }
      return null
    })
    h.acpConnect.mockClear()
    h.acpConnect.mockResolvedValue("owner-after-identical")
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    // Mid-settle: identical connect must not cancel the delay and strand the tab.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      void h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await handoff
    })
    vi.useRealTimers()

    expect(h.acpConnect).toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe(
      "owner-after-identical"
    )
  })

  it("observe_existing fails closed on malformed discovery payload", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "",
      event_seq: 0,
    } as { connection_id: string; event_seq: number })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
    // Stop immediately — do not burn the full retry schedule on garbage.
    expect(h.acpFindConnectionForConversation).toHaveBeenCalledTimes(1)
  })

  it("handoff auth error does not reattach after disconnect abandons the key", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    let resolveAuth!: (err: unknown) => void
    const authPending = new Promise<never>((_, reject) => {
      resolveAuth = reject
    })
    h.acpFindConnectionForConversation.mockImplementation(
      () => authPending as Promise<null>
    )
    h.acpConnect.mockClear()
    h.attach.mockClear()

    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    // Disconnect while discovery is still awaiting → abandon.
    await act(async () => {
      await h.actions!.disconnect(TAB)
    })
    const attachCallsAfterDisconnect = h.attach.mock.calls.length
    await act(async () => {
      resolveAuth({ status: 401, message: "Unauthorized" })
      await handoff
    })
    // Must not re-attach after abandon (would resurrect a disconnected tab).
    expect(h.attach.mock.calls.length).toBe(attachCallsAfterDisconnect)
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("disconnectAll cancels observer delays and handoff watchers", async () => {
    vi.useFakeTimers()
    h.acpFindConnectionForConversation.mockResolvedValue(null)
    await mountProvider()
    let pending!: Promise<void>
    await act(async () => {
      pending = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        true
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    const lookupsBefore = h.acpFindConnectionForConversation.mock.calls.length
    await act(async () => {
      await h.actions!.disconnectAll()
      await vi.advanceTimersByTimeAsync(10_000)
      await pending
    })
    vi.useRealTimers()
    expect(h.acpFindConnectionForConversation.mock.calls.length).toBe(
      lookupsBefore
    )
    expect(h.acpConnect).not.toHaveBeenCalled()
  })

  it("handoff re-entry microtask is cancelled by disconnect before it runs", async () => {
    // After re-attach, broker removal queues own_or_observe. Close before the
    // microtask runs must not start owner ACP (Task 5 r3 Important 1).
    await mountProvider()
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })

    h.toastWarning.mockClear()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 1,
    })
    h.acpConnect.mockClear()
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await handoff
    })
    vi.useRealTimers()
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")

    h.acpFindConnectionForConversation.mockResolvedValue(null)
    h.acpConnect.mockResolvedValue("owner-should-not-spawn")
    const handlers = latestAttachHandlers()

    // Detach queues re-entry microtask; disconnect cancels it before flush.
    await act(async () => {
      handlers.onDetached("connection_gone")
      // Do NOT await Promise.resolve yet — cancel first.
      await h.actions!.disconnect(TAB)
      await Promise.resolve()
      await Promise.resolve()
    })

    // Drain any residual async connect work.
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)).toBeUndefined()
  })

  it("handoff re-entry microtask is cancelled by relock (observe_existing) before it runs", async () => {
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await mountProvider()
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 1,
    })
    h.acpConnect.mockClear()
    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await vi.advanceTimersByTimeAsync(700)
      await vi.advanceTimersByTimeAsync(1500)
      await vi.advanceTimersByTimeAsync(2500)
      await handoff
    })
    vi.useRealTimers()
    expect(h.acpConnect).not.toHaveBeenCalled()

    // Relock wants observe_existing again; broker is still discoverable.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 2,
    })
    h.acpConnect.mockResolvedValue("owner-should-not-spawn")
    const handlers = latestAttachHandlers()

    await act(async () => {
      handlers.onDetached("connection_gone")
      // Intent change before re-entry microtask runs — must not own-spawn.
      void h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
      await Promise.resolve()
      await Promise.resolve()
    })

    await waitFor(() => {
      expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    })
    expect(h.acpConnect).not.toHaveBeenCalled()
    expect(h.store!.getConnection(TAB)?.isViewer).toBe(true)
  })

  it("confirmed-null handoff owns when another observer alias keeps dead broker entry", async () => {
    // Tab A handoff confirms broker null while Tab B still aliases the same
    // connectionId-keyed entry. dropReleasedHandoffBrokerEntry returns early;
    // orphan rescue must not rebind Tab A to the dead broker (Task 5 r3 I2).
    const TAB_B = "conv-1-claude_code-42-other-tab"
    await mountProvider()
    act(() => {
      h.actions!.attachDelegationChild({
        connectionId: "broker-child",
        parentConnectionId: "parent",
        parentToolUseId: "tool-1",
        agentType: "claude_code",
      })
    })
    const handlers = latestAttachHandlers()
    emitAcpEvent(handlers, {
      seq: 1,
      connection_id: "broker-child",
      type: "session_started",
      session_id: "sid",
    })
    expect(h.store!.getConnection("broker-child")?.sessionId).toBe("sid")

    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "broker-child",
      event_seq: 0,
    })
    await act(async () => {
      await h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    await act(async () => {
      await h.actions!.connect(
        TAB_B,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "observe_existing",
        false
      )
    })
    expect(h.store!.getConnection(TAB)?.connectionId).toBe("broker-child")
    expect(h.store!.getConnection(TAB_B)?.connectionId).toBe("broker-child")

    // Handoff for TAB only: one still-alive poll, then confirmed null.
    // Keep discovery null after so post-handoff path cannot re-observe.
    let handoffLookups = 0
    h.acpFindConnectionForConversation.mockImplementation(async () => {
      handoffLookups += 1
      if (handoffLookups <= 1) {
        return { connection_id: "broker-child", event_seq: handoffLookups }
      }
      return null
    })
    h.acpConnect.mockClear()
    h.acpConnect.mockResolvedValue("owner-after-multi-alias-null")
    h.attach.mockClear()

    vi.useFakeTimers()
    let handoff!: Promise<void>
    await act(async () => {
      handoff = h.actions!.connect(
        TAB,
        "claude_code",
        "/tmp/x",
        "sid",
        42,
        null,
        null,
        "own_or_observe",
        false
      )
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(300)
      await handoff
    })
    vi.useRealTimers()

    // Owner spawn for releasing tab — not viewer re-bind to dead broker.
    expect(h.acpConnect).toHaveBeenCalledTimes(1)
    expect(h.acpConnect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/x",
      "sid",
      undefined,
      {},
      42,
      null,
      null
    )
    expect(h.store!.getConnection(TAB)?.connectionId).toBe(
      "owner-after-multi-alias-null"
    )
    expect(h.store!.getConnection(TAB)?.isViewer).toBeFalsy()
    // Other observer alias may still reference the retained broker entry.
    expect(h.store!.getConnection(TAB_B)?.connectionId).toBe("broker-child")
  })

  // Alerts are live-only, so a client that attached after the empty turn has
  // the snapshot as its ONLY channel for the diagnosis.
  it("raises an alert for snapshot-carried details without touching conn.error or notifications", async () => {
    const handlers = await connectOwner()
    h.pushAlert.mockClear()
    h.sendSystemNotification.mockClear()

    const details =
      "stderr (this turn, last 1 lines):\n  Error: 401 Unauthorized"
    h.denormalizeSnapshot.mockReturnValue(
      snapshotPatch({
        eventSeq: 5,
        lastError: "agent ended the turn without producing any response.",
        lastErrorDetails: details,
      })
    )
    hydrateSnapshot(handlers, {
      event_seq: 5,
    } as unknown as LiveSessionSnapshot)

    const alertCalls = h.pushAlert.mock.calls
    const [, , alertDetail, , alertEvidence] =
      alertCalls[alertCalls.length - 1]!
    expect(alertDetail).toBe(
      "agent ended the turn without producing any response."
    )
    expect(alertEvidence).toBe(details)
    // The tooltip string stays the single-line message.
    expect(h.store!.getConnection(TAB)!.error).toBe(
      "agent ended the turn without producing any response."
    )
    expect(h.sendSystemNotification).not.toHaveBeenCalled()
  })

  it("does not re-alert the same details on every re-attach", async () => {
    const handlers = await connectOwner()
    h.pushAlert.mockClear()

    const patch = snapshotPatch({
      eventSeq: 5,
      lastError: "boom",
      lastErrorDetails: "stderr (this turn, last 1 lines):\n  same evidence",
    })
    h.denormalizeSnapshot.mockReturnValue(patch)
    hydrateSnapshot(handlers, {
      event_seq: 5,
    } as unknown as LiveSessionSnapshot)
    const afterFirst = h.pushAlert.mock.calls.length
    expect(afterFirst).toBe(1)

    // A reconnect replays the same snapshot.
    hydrateSnapshot(handlers, {
      event_seq: 6,
    } as unknown as LiveSessionSnapshot)
    expect(h.pushAlert.mock.calls.length).toBe(afterFirst)
  })

  it("stays silent for snapshot errors that carry no details", async () => {
    const handlers = await connectOwner()
    h.pushAlert.mockClear()

    h.denormalizeSnapshot.mockReturnValue(
      snapshotPatch({ eventSeq: 5, lastError: "some older error" })
    )
    hydrateSnapshot(handlers, {
      event_seq: 5,
    } as unknown as LiveSessionSnapshot)

    // Attaching to a connection with an ordinary past error must not start
    // raising alerts it never used to.
    expect(h.pushAlert).not.toHaveBeenCalled()
  })
})

describe("global acp://event listener is mount-once", () => {
  function mountDesktop() {
    return render(
      <AcpConnectionsProvider>
        <Probe />
      </AcpConnectionsProvider>
    )
  }

  beforeEach(() => {
    // Desktop firehose path — the web/attach transport skips this effect.
    h.eventStreamValue = null
    h.desktopUnsubscribe.mockClear()
    vi.mocked(subscribeDesktopAcpEvents).mockClear()
  })

  it("subscribes exactly once across provider re-renders", async () => {
    const { rerender } = mountDesktop()
    await act(async () => {})

    expect(vi.mocked(subscribeDesktopAcpEvents)).toHaveBeenCalledTimes(1)

    for (let i = 0; i < 3; i++) {
      await act(async () => {
        rerender(
          <AcpConnectionsProvider>
            <Probe />
          </AcpConnectionsProvider>
        )
      })
    }

    expect(vi.mocked(subscribeDesktopAcpEvents)).toHaveBeenCalledTimes(1)
    // Never torn down while mounted — no window with two live listeners.
    expect(h.desktopUnsubscribe).not.toHaveBeenCalled()
  })

  it("keeps delivering events through the surviving listener after re-renders", async () => {
    // Guards the other half of the fix: the one subscription that survives
    // must still route, and each delta must land exactly once (the reported
    // symptom was doubled text). Note this canNOT distinguish an old from a
    // new `t` closure — the suite's mocked translator returns the key
    // verbatim, so both produce identical output. Ref freshness itself rests
    // on the sync effect running every render; what this catches is a dead,
    // detached, or wrongly-frozen handler.
    const { rerender } = mountDesktop()
    await act(async () => {})

    await act(async () => {
      // No conversationId → skip discovery → owner spawn (acpConnect).
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1")
    })

    await act(async () => {
      rerender(
        <AcpConnectionsProvider>
          <Probe />
        </AcpConnectionsProvider>
      )
    })

    act(() => {
      h.emitDesktopBatch(
        batch(1, [
          {
            seq: 1,
            connection_id: "spawned-conn",
            type: "status_changed",
            status: "prompting",
          } as EventEnvelope,
          {
            seq: 2,
            connection_id: "spawned-conn",
            type: "content_delta",
            text: "你好",
          } as EventEnvelope,
        ])
      )
      h.runAnimationFrame()
    })

    const live = h.store!.getConnection(TAB)!.liveMessage
    const text = (live!.content as Array<{ type: string; text?: string }>)
      .filter((b) => b.type === "text")
      .map((b) => b.text ?? "")
      .join("")
    expect(text).toBe("你好")
  })

  it("unsubscribes on unmount", async () => {
    const { unmount } = mountDesktop()
    await act(async () => {})

    expect(vi.mocked(subscribeDesktopAcpEvents)).toHaveBeenCalledTimes(1)
    unmount()
    expect(h.desktopUnsubscribe).toHaveBeenCalledTimes(1)
  })
})

describe("delegation-child attach: mid-turn hydration", () => {
  // A work-task session viewer attaches to a turn that is ALREADY running.
  // On desktop the `acp://event` firehose carries only FUTURE events, so
  // without a snapshot the child sits at DELEGATION_CHILD_ATTACH's synthetic
  // "connected" with an empty live message — the viewer shows a stale
  // persisted transcript and never streams. Real delegation children attach
  // at spawn time and must NOT pay for a snapshot fetch.
  const CHILD = "task-conn-1"

  function attachChild(hydrate: boolean) {
    h.actions!.attachDelegationChild({
      connectionId: CHILD,
      parentConnectionId: CHILD,
      parentToolUseId: "work-task-9",
      agentType: "claude_code",
      hydrate,
    })
  }

  beforeEach(() => {
    // Desktop firehose path (the web attach protocol always opens with a
    // snapshot, so the gap this covers is desktop-only).
    h.eventStreamValue = null
    h.subscribe.mockClear()
    h.acpGetSessionSnapshot.mockResolvedValue({
      connection_id: CHILD,
      event_seq: 7,
    })
    h.denormalizeSnapshot.mockReturnValue({
      connectionId: CHILD,
      status: "prompting",
      sessionId: "sess-child",
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      pendingPlanApproval: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      lastError: null,
      eventSeq: 7,
      activeDelegations: [],
    })
  })

  it("hydrates the in-flight turn, then routes later firehose events", async () => {
    await mountProvider()

    await act(async () => {
      attachChild(true)
    })
    await act(async () => {})

    expect(h.acpGetSessionSnapshot).toHaveBeenCalledWith(CHILD)
    // The load-bearing bit: "prompting" is what makes the read-only viewer
    // render the live stream instead of a settled transcript.
    expect(h.store!.getConnection(CHILD)?.status).toBe("prompting")
    expect(h.store!.getConnection(CHILD)?.lastAppliedSeq).toBe(7)

    // Reverse-map routing is installed AFTER hydration, so post-snapshot
    // events still land (and pre-snapshot ones are deduped by seq).
    act(() => {
      h.emitDesktopBatch({
        batch_id: 1,
        events: [
          {
            seq: 8,
            connection_id: CHILD,
            type: "content_delta",
            text: "hi",
          } as EventEnvelope,
        ],
      })
      h.runAnimationFrame()
    })
    expect(h.store!.getConnection(CHILD)?.lastAppliedSeq).toBe(8)
  })

  it("re-seeds delegation bindings the hydrated snapshot carries", async () => {
    // `delegation_started` is transient — never in the snapshot's event set and
    // never replayed — so a viewer opening onto a turn that already delegated
    // establishes no binding unless the snapshot's `active_delegations` is
    // fanned out. Without this the work-task dialog's sub-agent cards lose
    // their agent icon/label, the child's live sub-stream and the "待批准"
    // badge. The other three snapshot consumers already did this; the desktop
    // hydrate branch did not.
    const active = [
      {
        parent_tool_use_id: "toolu_child",
        child_connection_id: "child-conn",
        child_conversation_id: 4242,
        agent_type: "codex" as const,
        task_preview: "review the diff",
        task_id: "task-1",
      },
    ]
    h.denormalizeSnapshot.mockReturnValue({
      connectionId: CHILD,
      status: "prompting",
      sessionId: "sess-child",
      modes: null,
      configOptions: null,
      availableCommands: null,
      usage: null,
      liveMessage: null,
      pendingPermission: null,
      pendingAskQuestion: null,
      pendingUserMessage: null,
      pendingPlanApproval: null,
      promptCapabilities: null,
      selectorsReady: false,
      supportsFork: false,
      configStale: false,
      configStaleKind: null,
      lastError: null,
      eventSeq: 7,
      activeDelegations: active,
    })
    await mountProvider()

    await act(async () => {
      attachChild(true)
    })
    await act(async () => {})

    expect(h.buildDelegationSeedEnvelopes).toHaveBeenCalledWith(
      CHILD,
      active,
      7
    )
  })

  it("does not seed when the child detached while the snapshot was in flight", async () => {
    let resolveSnapshot: (v: unknown) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSnapshot = res
        })
    )
    await mountProvider()

    await act(async () => {
      attachChild(true)
    })
    await act(async () => {
      h.actions!.detachDelegationChild(CHILD)
    })
    await act(async () => {
      resolveSnapshot({ connection_id: CHILD, event_seq: 7 })
    })
    await act(async () => {})

    expect(h.buildDelegationSeedEnvelopes).not.toHaveBeenCalled()
  })

  it("skips the snapshot for a spawn-time child attach", async () => {
    await mountProvider()

    await act(async () => {
      attachChild(false)
    })
    await act(async () => {})

    expect(h.acpGetSessionSnapshot).not.toHaveBeenCalled()
    expect(h.store!.getConnection(CHILD)?.status).toBe("connected")
  })

  it("does not hydrate or route a child detached while the snapshot is in flight", async () => {
    let resolveSnapshot: (v: unknown) => void = () => {}
    h.acpGetSessionSnapshot.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSnapshot = res
        })
    )
    await mountProvider()

    await act(async () => {
      attachChild(true)
    })
    await act(async () => {
      h.actions!.detachDelegationChild(CHILD)
    })
    await act(async () => {
      resolveSnapshot({ connection_id: CHILD, event_seq: 7 })
    })
    await act(async () => {})

    // The viewer is gone: no resurrected connection state, and the firehose
    // must not be routing to a contextKey nobody is watching.
    expect(h.store!.getConnection(CHILD)).toBeUndefined()
    act(() => {
      h.emitDesktopBatch({
        batch_id: 1,
        events: [
          {
            seq: 8,
            connection_id: CHILD,
            type: "content_delta",
            text: "hi",
          } as EventEnvelope,
        ],
      })
      h.runAnimationFrame()
    })
    expect(h.store!.getConnection(CHILD)).toBeUndefined()
  })
})
