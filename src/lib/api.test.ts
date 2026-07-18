import { beforeEach, describe, expect, it, vi } from "vitest"

const mockTransport = vi.hoisted(() => ({
  call: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => mockTransport,
}))

// Import only after the mock declaration so callers close over it.
import {
  acpPrompt,
  cancelReferenceSearch,
  matchReferenceRegex,
  nextReferenceSearchPage,
  startReferenceSearch,
  validateReferenceCandidate,
} from "@/lib/api"

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
