import { beforeEach, describe, expect, it, vi } from "vitest"

const mockTransport = vi.hoisted(() => ({
  call: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  getActiveRemoteConnectionId: () => null,
  getTransport: () => mockTransport,
  isDesktop: () => true,
}))

// Import only after the mock declaration so callers close over it.
import {
  acpPrompt,
  cancelReferenceSearch,
  cancelToolWatchdogLease,
  closeFolderIfEmpty,
  extendToolWatchdogLease,
  getToolWatchdogSettings,
  setToolWatchdogSettings,
  matchReferenceRegex,
  nextReferenceSearchPage,
  saveTranslationAs,
  startReferenceSearch,
  translateDocument,
  validateReferenceCandidate,
} from "@/lib/api"

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
