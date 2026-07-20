import { useEffect } from "react"
import { act, render } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  AcpConnectionsProvider,
  useAcpActions,
  useConnectionStore,
  __getPublishedConnectionMapsCount,
  __resetPublishedConnectionMapsCount,
  __resetStreamingConfigForProviderTests,
  __connectionsReducerForTests,
  __resetWritableConnectionsCloneCount,
  __getWritableConnectionsCloneCount,
} from "@/contexts/acp-connections-context"
import type {
  DbConversationSummary,
  DesktopAcpEventBatch,
  DesktopDeliveryFailure,
} from "@/lib/types"
import { parsePermissionToolCall } from "@/lib/permission-request"
import { saveConfigPreference } from "@/lib/selector-prefs-storage"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"
import type { AttachHandlers } from "@/lib/transport/types"
import type {
  EventEnvelope,
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
    acpDisconnect: vi.fn(),
    acpGetSessionSnapshot: vi.fn(),
    acpGetDesktopDeliveryCapabilities: vi.fn(),
    buildDelegationSeedEnvelopes: vi.fn(() => []),
    denormalizeSnapshot: vi.fn(),
    pushAlert: vi.fn(),
    isDesktop: true,
    rafQueue,
    desktopBatchHandler: null as ((batch: DesktopAcpEventBatch) => void) | null,
    desktopFailureHandler: null as
      | ((failure: DesktopDeliveryFailure) => void)
      | null,
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
    subscribeRaw(handler: (event: EventEnvelope) => void) {
      // Registered via useAcpEvent after mount — tests call actions path.
      // Provider exposes subscribers only through the hook; for raw tests we
      // use a lightweight wrapper registered in the describe block.
      void handler
    },
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
  subscribe: vi.fn(async () => () => {}),
  getEventStream: () => h.eventStreamValue,
}))

vi.mock("@/lib/delegation-seed", () => ({
  buildDelegationSeedEnvelopes: h.buildDelegationSeedEnvelopes,
}))

vi.mock("@/contexts/alert-context", () => ({
  useAlertContext: () => ({ pushAlert: h.pushAlert }),
}))

vi.mock("@/contexts/active-folder-context", () => ({
  useActiveFolder: () => ({ activeFolder: { path: "/tmp/x", name: "x" } }),
}))

vi.mock("@/lib/notification", () => ({
  sendSystemNotification: vi.fn(async () => undefined),
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

vi.mock("@/lib/api", () => ({
  acpGetAgentStatus: h.acpGetAgentStatus,
  acpFindConnectionForConversation: h.acpFindConnectionForConversation,
  acpConnect: h.acpConnect,
  acpDisconnect: h.acpDisconnect,
  acpGetSessionSnapshot: h.acpGetSessionSnapshot,
  acpGetDesktopDeliveryCapabilities: h.acpGetDesktopDeliveryCapabilities,
  acpPrompt: acpPromptMock,
  acpAnswerQuestion: acpAnswerQuestionMock,
  acpSetMode: vi.fn(),
  acpSetConfigOption: vi.fn(),
  acpCancel: vi.fn(),
  acpRespondPermission: vi.fn(),
  acpTouchConnection: vi.fn(),
  // Imported by the conversation runtime store (a real dependency of the
  // provider via the background-activity bridge). The settled path fires a
  // refetchDetail; reject it so the store's error path absorbs it (these
  // tests assert the refetch was ISSUED, not its payload).
  getFolderConversation: vi.fn(async () => {
    throw new Error("detail not seeded in this suite")
  }),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({
    isDesktop: () => h.isDesktop,
    subscribe: vi.fn(async () => () => {}),
    call: vi.fn(),
  }),
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

async function mountProvider() {
  render(
    <AcpConnectionsProvider>
      <Probe />
    </AcpConnectionsProvider>
  )
  await act(async () => {})
}

const TAB = "conv-1-claude_code-42"

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
  h.rafQueue.length = 0
  h.buildDelegationSeedEnvelopes.mockClear()
  h.acpGetAgentStatus.mockReset()
  h.acpFindConnectionForConversation.mockReset()
  h.acpConnect.mockReset()
  h.acpDisconnect.mockReset()
  h.acpGetSessionSnapshot.mockReset()
  h.acpGetDesktopDeliveryCapabilities.mockReset()
  h.denormalizeSnapshot.mockReset()
  h.pushAlert.mockReset()
  acpPromptMock.mockReset()
  acpPromptMock.mockResolvedValue(undefined)
  acpAnswerQuestionMock.mockReset()
  acpAnswerQuestionMock.mockResolvedValue(undefined)
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
    eventSeq: 0,
    activeDelegations: [],
    delegationRoute: null,
  })
  // Agent is installed + available so the connect preflight passes.
  h.acpGetAgentStatus.mockResolvedValue({
    agent_type: "claude_code",
    enabled: true,
    available: true,
    installed_version: "1.0.0",
  })
  h.acpConnect.mockResolvedValue("spawned-conn")
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
    // A re-connect at the same tab with a different workingDir hits the
    // replace-existing path. If the existing entry is a viewer, that path must
    // NOT acpDisconnect the owner's connection.
    h.acpFindConnectionForConversation.mockResolvedValue({
      connection_id: "owner-conn",
      event_seq: 0,
    })
    await mountProvider()

    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/x", "sess-1", 42)
    })
    await act(async () => {
      await h.actions!.connect(TAB, "claude_code", "/tmp/other", "sess-1", 42)
    })

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

    expect(h.acpDisconnect).toHaveBeenCalledWith("spawned-conn")
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
      eventSeq: 5,
      activeDelegations: [],
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
    // preferredConfigValues, conversationId, delegationRouteOverride.
    expect(h.acpConnect).toHaveBeenCalledWith(
      "codex",
      "/repo",
      undefined,
      undefined,
      {},
      7,
      "native"
    )
    const conn = h.store!.getConnection(TAB)
    expect(conn?.conversationId).toBe(7)
    expect(conn?.delegationRouteOverride).toBe("native")
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
    expect(h.acpDisconnect).toHaveBeenCalledWith("spawned-conn")
    // …then reconnect reuses the stored conversation id + route override exactly
    // (sessionId is whatever the connection last held — typically from snapshot).
    expect(h.acpConnect).toHaveBeenCalledWith(
      "codex",
      "/repo",
      conn?.sessionId ?? undefined,
      undefined,
      {},
      42,
      "native"
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

    const call = h.pushAlert.mock.calls.find(
      (c) =>
        typeof c[1] === "string" &&
        (c[1] as string).includes("blocked.sdkMissing")
    )
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

  it("background_activity mirrors outstanding, applies overlay turns, and notifies settled tasks", async () => {
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    const { sendSystemNotification } = await import("@/lib/notification")
    const notify = vi.mocked(sendSystemNotification)
    notify.mockClear()
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

    // 4. a settlement folds into persisted turns via a detail refetch (the
    //    parser joins ack + notification into the card's terminal state).
    //    The fetch must go out with the DB row id, not the runtime key.
    const { getFolderConversation } = await import("@/lib/api")
    expect(vi.mocked(getFolderConversation)).toHaveBeenCalledWith(42)

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
      installed_version: "0.2.98",
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
    sessionId = "sess-1"
  ) {
    h.eventStreamValue = null
    h.acpConnect.mockResolvedValue(connectionId)
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
      await h.actions!.connect(contextKey, "claude_code", "/tmp/x", sessionId)
    })
  }

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
      // user_message produces no FrameAction store mutations — cursor only.
      h.emitDesktopBatch(
        batch(2, [
          {
            connection_id: "owner-conn",
            seq: 2,
            type: "user_message",
            message_id: "m1",
            blocks: [{ type: "text", text: "hi" }],
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
      ...overrides,
    }
  }

  const framePathFixtures: Array<{
    name: string
    action: import("@/contexts/acp-connections-context").__FrameActionForTests
    conn?: Partial<import("@/contexts/acp-connections-context").ConnectionState>
  }> = [
    {
      name: "CONTENT_DELTA",
      action: { type: "CONTENT_DELTA", contextKey: "k1", text: "hi" },
    },
    {
      name: "THINKING",
      action: { type: "THINKING", contextKey: "k1", text: "hmm" },
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
      },
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
        "SET_PENDING_QUESTION",
        "STATUS_CHANGED",
        "THINKING",
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

  it("does not begin activity for an unknown connection context", async () => {
    await mountProvider()

    await act(async () => {
      await h.actions!.sendPrompt("missing-key", [
        { type: "text", text: "wire" },
      ])
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
