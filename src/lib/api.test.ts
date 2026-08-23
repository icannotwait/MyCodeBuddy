import { beforeEach, describe, expect, it, vi } from "vitest"

const mockTransport = vi.hoisted(() => ({
  call: vi.fn(),
}))
const transportMode = vi.hoisted(() => ({
  desktop: true,
  remoteConnectionId: null as number | null,
}))

vi.mock("@/lib/transport", () => ({
  getActiveRemoteConnectionId: () => transportMode.remoteConnectionId,
  getTransport: () => mockTransport,
  isDesktop: () => transportMode.desktop,
}))

// Import only after the mock declaration so callers close over it.
import {
  acpAnswerPlanApproval,
  acpAnswerQuestion,
  acpCancel,
  acpCancelQueuedPrompt,
  acpConnectOrAttach,
  acpGoalControl,
  acpPrompt,
  acpReleaseLease,
  acpRespondPermission,
  acpSetConfigOption,
  acpSetMode,
  acpTerminateSharedSession,
  cancelReferenceSearch,
  cancelToolWatchdogLease,
  closeFolderIfEmpty,
  CONVERSATION_POPOUT_RUNTIME_RESTART_REQUIRED_I18N_KEY,
  deleteConversation,
  extendToolWatchdogLease,
  getFolderConversation,
  getToolWatchdogSettings,
  setToolWatchdogSettings,
  matchReferenceRegex,
  nextReferenceSearchPage,
  resolveGrokSessionImage,
  saveTranslationAs,
  startReferenceSearch,
  translateDocument,
  validateReferenceCandidate,
} from "@/lib/api"
import { TurnBusyError } from "@/lib/turn-busy"

it("locks the runtime restart wire key to the Rust literal", () => {
  expect(CONVERSATION_POPOUT_RUNTIME_RESTART_REQUIRED_I18N_KEY).toBe(
    "ConversationPopout.runtimeRestartRequired"
  )
})

describe("completion protocol frontend command removal", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue({})
  })

  it("keeps ordinary conversation deletion outside graph mutations", async () => {
    await deleteConversation(42)

    expect(mockTransport.call).toHaveBeenCalledWith("delete_conversation", {
      conversationId: 42,
    })
  })
})

describe("getFolderConversation history window payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue({})
  })

  it("opts frontend callers into the default history window", async () => {
    await getFolderConversation(42)
    expect(mockTransport.call).toHaveBeenCalledWith("get_folder_conversation", {
      conversationId: 42,
      historyUserTurnLimit: 20,
      historyBeforeTurnId: undefined,
    })
  })

  it("keeps explicit unlimited history available", async () => {
    await getFolderConversation(42, { historyUserTurnLimit: 0 })
    expect(mockTransport.call).toHaveBeenCalledWith("get_folder_conversation", {
      conversationId: 42,
      historyUserTurnLimit: 0,
      historyBeforeTurnId: undefined,
    })
  })
})

describe("resolveGrokSessionImage transport payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue({
      path: "/tmp/images/a.png",
      origin: "session",
      mimeType: "image/png",
      dataBase64: "AA==",
    })
  })

  it("uses one transport call with exact camelCase defaults", async () => {
    await resolveGrokSessionImage({ conversationId: 42, href: "images/a.png" })
    expect(mockTransport.call).toHaveBeenCalledWith(
      "resolve_grok_session_image",
      { conversationId: 42, href: "images/a.png", includeData: false }
    )
  })

  it("passes includeData true without a direct Tauri wrapper", async () => {
    await resolveGrokSessionImage({
      conversationId: 42,
      href: "images/a.png",
      includeData: true,
    })
    expect(mockTransport.call).toHaveBeenCalledWith(
      "resolve_grok_session_image",
      { conversationId: 42, href: "images/a.png", includeData: true }
    )
  })
})

describe("tool-watchdog lease control transport payloads", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue(undefined)
  })

  it("extend sends camelCase leaseId + version (desktop+web contract)", async () => {
    await extendToolWatchdogLease("lease-abc", 7)
    expect(mockTransport.call).toHaveBeenCalledWith(
      "acp_tool_watchdog_extend",
      {
        leaseId: "lease-abc",
        version: 7,
      }
    )
    const args = mockTransport.call.mock.calls[0]![1] as Record<string, unknown>
    expect(args).not.toHaveProperty("lease_id")
    expect(Object.keys(args).sort()).toEqual(["leaseId", "version"])
  })

  it("cancel sends camelCase leaseId + version (desktop+web contract)", async () => {
    await cancelToolWatchdogLease("lease-xyz", 3)
    expect(mockTransport.call).toHaveBeenCalledWith(
      "acp_tool_watchdog_cancel",
      {
        leaseId: "lease-xyz",
        version: 3,
      }
    )
    const args = mockTransport.call.mock.calls[0]![1] as Record<string, unknown>
    expect(args).not.toHaveProperty("lease_id")
    expect(Object.keys(args).sort()).toEqual(["leaseId", "version"])
  })

  it("get settings uses acp_get_tool_watchdog_settings with no body", async () => {
    mockTransport.call.mockResolvedValue({
      enabled: true,
      warning_after_seconds: 600,
      grace_seconds: 600,
    })
    await getToolWatchdogSettings()
    expect(mockTransport.call).toHaveBeenCalledWith(
      "acp_get_tool_watchdog_settings"
    )
  })

  it("set settings sends camelCase duration fields", async () => {
    mockTransport.call.mockResolvedValue({
      enabled: false,
      warning_after_seconds: 60,
      grace_seconds: 3600,
    })
    await setToolWatchdogSettings({
      enabled: false,
      warning_after_seconds: 59,
      grace_seconds: 3601,
    })
    expect(mockTransport.call).toHaveBeenCalledWith(
      "acp_set_tool_watchdog_settings",
      {
        enabled: false,
        warningAfterSeconds: 59,
        graceSeconds: 3601,
      }
    )
    const args = mockTransport.call.mock.calls[0]![1] as Record<string, unknown>
    expect(args).not.toHaveProperty("warning_after_seconds")
    expect(args).not.toHaveProperty("grace_seconds")
  })
})

describe("closeFolderIfEmpty transport payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
  })

  it("calls close_folder_if_empty with camelCase folderId and typed closed:true", async () => {
    mockTransport.call.mockResolvedValue({ closed: true })
    const result = await closeFolderIfEmpty(42)
    expect(mockTransport.call).toHaveBeenCalledWith("close_folder_if_empty", {
      folderId: 42,
    })
    const args = mockTransport.call.mock.calls[0]![1] as Record<string, unknown>
    expect(args).not.toHaveProperty("folder_id")
    expect(result).toEqual({ closed: true })
    expect(result.closed).toBe(true)
  })

  it("returns typed closed:false when the folder is non-empty or already closed", async () => {
    mockTransport.call.mockResolvedValue({ closed: false })
    const result = await closeFolderIfEmpty(7)
    expect(mockTransport.call).toHaveBeenCalledWith("close_folder_if_empty", {
      folderId: 7,
    })
    expect(result).toEqual({ closed: false })
    expect(result.closed).toBe(false)
  })
})

describe("acpPrompt transport payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue(undefined)
    transportMode.desktop = true
    transportMode.remoteConnectionId = null
  })

  it("sends displayText and the effective app locale with the ACP prompt", async () => {
    await acpPrompt(
      "connection",
      [{ type: "text", text: "wire" }],
      1,
      2,
      "m1",
      {
        visibleText: "README.md task",
        locale: "zh_cn",
      }
    )
    expect(mockTransport.call).toHaveBeenCalledWith("acp_prompt", {
      connectionId: "connection",
      blocks: [{ type: "text", text: "wire" }],
      folderId: 1,
      conversationId: 2,
      clientMessageId: "m1",
      visibleText: "README.md task",
      locale: "zh_cn",
    })
  })
})

describe("shared ACP transport payloads", () => {
  const shared = { generation: 7, leaseId: "lease-7" }

  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue(null)
    transportMode.desktop = true
    transportMode.remoteConnectionId = null
  })

  it("connects or attaches with the exact camel-case identity payload", async () => {
    const response = {
      connectionId: "connection-7",
      generation: 7,
      leaseId: "lease-7",
      leaseExpiresAt: "2026-08-16T08:00:00Z",
      disposition: "attached" as const,
      phase: "ready" as const,
      eventSeq: 12,
      error: null,
    }
    mockTransport.call.mockResolvedValue(response)

    await expect(
      acpConnectOrAttach({
        conversationId: 42,
        agentType: "codex",
        workingDir: "/repo",
        externalSessionId: "session-42",
        delegationRouteOverride: "codeg",
        preferredModeId: "plan",
        preferredConfigValues: { model: "gpt-5" },
        deviceId: "device-1",
        clientInstanceId: "client-1",
        requestId: "request-1",
        retryFailedGeneration: 6,
      })
    ).resolves.toBe(response)
    expect(mockTransport.call).toHaveBeenCalledWith("acp_connect_or_attach", {
      conversationId: 42,
      agentType: "codex",
      workingDir: "/repo",
      externalSessionId: "session-42",
      delegationRouteOverride: "codeg",
      preferredModeId: "plan",
      preferredConfigValues: { model: "gpt-5" },
      deviceId: "device-1",
      clientInstanceId: "client-1",
      requestId: "request-1",
      retryFailedGeneration: 6,
    })
  })

  it("sends exact lease release, queue cancel, and terminate payloads", async () => {
    await acpReleaseLease("connection-7", 7, "lease-7")
    await acpCancelQueuedPrompt("connection-7", "queue-3", shared)
    await acpTerminateSharedSession("connection-7", 7)

    expect(mockTransport.call).toHaveBeenNthCalledWith(1, "acp_release_lease", {
      connectionId: "connection-7",
      generation: 7,
      leaseId: "lease-7",
    })
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      2,
      "acp_cancel_queued_prompt",
      {
        connectionId: "connection-7",
        queueItemId: "queue-3",
        generation: 7,
        leaseId: "lease-7",
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      3,
      "acp_terminate_shared_session",
      {
        connectionId: "connection-7",
        generation: 7,
      }
    )
  })

  it("returns shared queue admission and strips uploaded image bytes", async () => {
    transportMode.desktop = false
    const admission = {
      queueItemId: "queue-8",
      enqueueSeq: 8,
      state: "queued" as const,
    }
    mockTransport.call.mockResolvedValue(admission)

    await expect(
      acpPrompt(
        "connection-7",
        [
          {
            type: "image",
            data: "base64-payload",
            mime_type: "image/png",
            uri: "file:///uploads/image.png",
          },
        ],
        3,
        42,
        "message-8",
        { visibleText: "image", locale: "en" },
        {
          ...shared,
          clientInstanceId: "client-1",
          clientRequestId: "request-8",
        }
      )
    ).resolves.toBe(admission)

    expect(mockTransport.call).toHaveBeenCalledWith("acp_prompt", {
      connectionId: "connection-7",
      blocks: [
        {
          type: "image",
          data: "",
          mime_type: "image/png",
          uri: "file:///uploads/image.png",
        },
      ],
      folderId: 3,
      conversationId: 42,
      clientMessageId: "message-8",
      visibleText: "image",
      locale: "en",
      generation: 7,
      leaseId: "lease-7",
      clientInstanceId: "client-1",
      clientRequestId: "request-8",
    })
  })

  it("normalizes legacy prompt success to null without shared wire keys", async () => {
    mockTransport.call.mockResolvedValue(undefined)

    await expect(
      acpPrompt(
        "legacy",
        [{ type: "text", text: "hello" }],
        null,
        null,
        "message-1"
      )
    ).resolves.toBeNull()

    const payload = mockTransport.call.mock.calls[0]![1]
    expect(payload).toEqual({
      connectionId: "legacy",
      blocks: [{ type: "text", text: "hello" }],
      folderId: null,
      conversationId: null,
      clientMessageId: "message-1",
      visibleText: null,
      locale: null,
    })
    expect(payload).not.toHaveProperty("generation")
    expect(payload).not.toHaveProperty("leaseId")
    expect(payload).not.toHaveProperty("clientInstanceId")
    expect(payload).not.toHaveProperty("clientRequestId")
  })

  it("maps legacy turn busy but preserves shared queue admission errors", async () => {
    const rejection = {
      code: "turn_in_progress",
      message: "turn already in progress",
    }
    mockTransport.call.mockRejectedValue(rejection)

    await expect(
      acpPrompt("legacy", [{ type: "text", text: "one" }])
    ).rejects.toBeInstanceOf(TurnBusyError)
    await expect(
      acpPrompt(
        "shared",
        [{ type: "text", text: "two" }],
        3,
        42,
        "message-2",
        { visibleText: "two", locale: "en" },
        {
          ...shared,
          clientInstanceId: "client-1",
          clientRequestId: "request-2",
        }
      )
    ).rejects.toBe(rejection)
  })

  it("adds fencing only to shared stop and interaction payloads", async () => {
    const stop = { ...shared, turnId: "turn-9" }
    const questionAnswer = {
      answers: [{ questionId: "part-1", labels: ["yes"] }],
      declined: false,
    }
    const planAnswer = { decision: "approve" as const, feedback: null }

    await acpCancel("connection-7", stop)
    await acpRespondPermission("connection-7", "permission-1", "allow", shared)
    await acpAnswerQuestion(
      "connection-7",
      "question-1",
      questionAnswer,
      shared
    )
    await acpAnswerPlanApproval(
      "connection-7",
      "approval-1",
      planAnswer,
      shared
    )

    expect(mockTransport.call).toHaveBeenNthCalledWith(1, "acp_cancel", {
      connectionId: "connection-7",
      generation: 7,
      leaseId: "lease-7",
      turnId: "turn-9",
    })
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      2,
      "acp_respond_permission",
      {
        connectionId: "connection-7",
        requestId: "permission-1",
        optionId: "allow",
        generation: 7,
        leaseId: "lease-7",
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      3,
      "acp_answer_question",
      {
        connectionId: "connection-7",
        questionId: "question-1",
        answer: questionAnswer,
        generation: 7,
        leaseId: "lease-7",
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      4,
      "acp_answer_plan_approval",
      {
        connectionId: "connection-7",
        approvalId: "approval-1",
        answer: planAnswer,
        generation: 7,
        leaseId: "lease-7",
      }
    )
  })

  it("adds optional fencing to mode, configuration, and goal mutations", async () => {
    await acpSetMode("connection-7", "plan", shared)
    await acpSetConfigOption("connection-7", "model", "gpt-5", shared)
    await acpGoalControl("connection-7", "pause", shared)

    expect(mockTransport.call).toHaveBeenNthCalledWith(1, "acp_set_mode", {
      connectionId: "connection-7",
      modeId: "plan",
      generation: 7,
      leaseId: "lease-7",
    })
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      2,
      "acp_set_config_option",
      {
        connectionId: "connection-7",
        configId: "model",
        valueId: "gpt-5",
        generation: 7,
        leaseId: "lease-7",
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(3, "acp_goal_control", {
      connectionId: "connection-7",
      action: "pause",
      generation: 7,
      leaseId: "lease-7",
    })
  })

  it("preserves unfenced legacy mode, configuration, and goal payloads", async () => {
    await acpSetMode("legacy", "plan")
    await acpSetConfigOption("legacy", "model", "gpt-5")
    await acpGoalControl("legacy", "clear")

    expect(mockTransport.call).toHaveBeenNthCalledWith(1, "acp_set_mode", {
      connectionId: "legacy",
      modeId: "plan",
    })
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      2,
      "acp_set_config_option",
      {
        connectionId: "legacy",
        configId: "model",
        valueId: "gpt-5",
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(3, "acp_goal_control", {
      connectionId: "legacy",
      action: "clear",
    })
  })

  it("preserves legacy stop and interaction payloads", async () => {
    const questionAnswer = { answers: [], declined: true }
    const planAnswer = { decision: "abandon" as const }

    await acpCancel("legacy")
    await acpRespondPermission("legacy", "permission-1", "reject")
    await acpAnswerQuestion("legacy", "question-1", questionAnswer)
    await acpAnswerPlanApproval("legacy", "approval-1", planAnswer)

    expect(mockTransport.call).toHaveBeenNthCalledWith(1, "acp_cancel", {
      connectionId: "legacy",
    })
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      2,
      "acp_respond_permission",
      {
        connectionId: "legacy",
        requestId: "permission-1",
        optionId: "reject",
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      3,
      "acp_answer_question",
      {
        connectionId: "legacy",
        questionId: "question-1",
        answer: questionAnswer,
      }
    )
    expect(mockTransport.call).toHaveBeenNthCalledWith(
      4,
      "acp_answer_plan_approval",
      {
        connectionId: "legacy",
        approvalId: "approval-1",
        answer: planAnswer,
      }
    )
  })
})

describe("translateDocument transport payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue({
      translatedContent: "你好",
      locale: "zh_cn",
      format: "markdown",
    })
  })

  it("passes timeoutMs 540000 for document translation", async () => {
    const params = {
      content: "# Hello",
      format: "markdown" as const,
      locale: "zh_cn",
      displayName: "README.md",
    }
    await translateDocument(params)
    expect(mockTransport.call).toHaveBeenCalledWith(
      "translate_document",
      params,
      { timeoutMs: 540_000 }
    )
  })
})

describe("saveTranslationAs transport payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue({
      absolutePath: "/ws/README.zh_cn.md",
    })
  })

  it("sends flat folderId relativePath content payload", async () => {
    const params = {
      folderId: 7,
      relativePath: "README.zh_cn.md",
      content: "你好",
    }
    const result = await saveTranslationAs(params)
    expect(mockTransport.call).toHaveBeenCalledWith(
      "save_translation_as",
      params
    )
    expect(result.absolutePath).toBe("/ws/README.zh_cn.md")
  })
})

describe("reference search transport payloads", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue({})
  })

  it("reference_calls_use_flat_protocol_payloads_and_forward_signals", async () => {
    const controller = new AbortController()
    const signal = controller.signal

    const startReq = {
      searchSessionId: "11111111-1111-4111-8111-111111111111",
      sourceSequence: 1,
      requestId: "22222222-2222-4222-8222-222222222222",
      source: "file" as const,
      query: "src/",
      workspacePath: "/repo",
    }
    await startReferenceSearch(startReq, signal)
    expect(mockTransport.call).toHaveBeenLastCalledWith(
      "start_reference_search",
      startReq,
      { timeoutMs: 35_000, signal }
    )
    expect(mockTransport.call.mock.calls.at(-1)?.[1]).not.toHaveProperty(
      "request"
    )

    const nextReq = {
      searchSessionId: startReq.searchSessionId,
      sourceSequence: 1,
      requestId: startReq.requestId,
      source: "file" as const,
      pageIndex: 1,
    }
    await nextReferenceSearchPage(nextReq, signal)
    expect(mockTransport.call).toHaveBeenLastCalledWith(
      "next_reference_search_page",
      nextReq,
      { timeoutMs: 35_000, signal }
    )

    const cancelReq = {
      searchSessionId: startReq.searchSessionId,
      sourceSequence: 1,
      requestId: startReq.requestId,
      source: "file" as const,
    }
    await cancelReferenceSearch(cancelReq)
    expect(mockTransport.call).toHaveBeenLastCalledWith(
      "cancel_reference_search",
      cancelReq
    )
    // Guarded cancel: no CallOptions.
    expect(mockTransport.call.mock.calls.at(-1)?.length).toBe(2)

    const validateReq = {
      validationRequestId: "33333333-3333-4333-8333-333333333333",
      source: "file" as const,
      uri: "file:///repo/a.ts",
      query: "a",
      workspacePath: "/repo",
    }
    await validateReferenceCandidate(validateReq, signal)
    expect(mockTransport.call).toHaveBeenLastCalledWith(
      "validate_reference_candidate",
      validateReq,
      { signal }
    )

    const regexReq = {
      query: "re:foo",
      descriptors: [
        {
          id: "d1",
          sourceOrdinal: 0,
          primary: ["foo"],
          secondary: [],
        },
      ],
    }
    await matchReferenceRegex(regexReq, signal)
    expect(mockTransport.call).toHaveBeenLastCalledWith(
      "match_reference_regex",
      regexReq,
      { signal }
    )

    // Conversation start objects omit workspacePath entirely (no own property).
    const conversationStart = {
      searchSessionId: startReq.searchSessionId,
      sourceSequence: 1,
      requestId: "44444444-4444-4444-8444-444444444444",
      source: "conversation" as const,
      query: "title",
    }
    expect(
      Object.prototype.hasOwnProperty.call(conversationStart, "workspacePath")
    ).toBe(false)
    await startReferenceSearch(conversationStart, signal)
    const conversationPayload = mockTransport.call.mock.calls.at(-1)?.[1] as
      | Record<string, unknown>
      | undefined
    expect(conversationPayload).toBeDefined()
    expect(
      Object.prototype.hasOwnProperty.call(conversationPayload, "workspacePath")
    ).toBe(false)
  })
})
