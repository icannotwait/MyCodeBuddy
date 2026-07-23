import { beforeEach, describe, expect, it, vi } from "vitest"

const mockTransport = vi.hoisted(() => ({
  call: vi.fn(),
  isDesktop: vi.fn(() => true),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => mockTransport,
  isDesktop: () => mockTransport.isDesktop(),
}))

vi.mock("@/lib/utils", () => ({
  randomUUID: () => "test-action-uuid",
}))

import { sendSystemNotification } from "@/lib/notification"

describe("sendSystemNotification desktop payload", () => {
  beforeEach(() => {
    mockTransport.call.mockReset()
    mockTransport.call.mockResolvedValue(undefined)
    mockTransport.isDesktop.mockReturnValue(true)
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => true,
    })
  })

  it("sends camelCase actionId + conversationId for conversation targets", async () => {
    await sendSystemNotification("Title", "Body", {
      kind: "conversation",
      conversationId: 42,
    })
    expect(mockTransport.call).toHaveBeenCalledWith("send_notification", {
      title: "Title",
      body: "Body",
      actionId: "test-action-uuid",
      conversationId: 42,
      dedupeKey: null,
    })
    const args = mockTransport.call.mock.calls[0]![1] as Record<string, unknown>
    expect(args).not.toHaveProperty("action_id")
    expect(args).not.toHaveProperty("conversation_id")
    expect(args).not.toHaveProperty("dedupe_key")
  })

  it("forwards host dedupeKey for multi-window once-per-version gate", async () => {
    await sendSystemNotification(
      "Title",
      "Body",
      { kind: "conversation", conversationId: 9 },
      { dedupeKey: "lease-1:2" }
    )
    expect(mockTransport.call).toHaveBeenCalledWith(
      "send_notification",
      expect.objectContaining({
        actionId: "test-action-uuid",
        conversationId: 9,
        dedupeKey: "lease-1:2",
      })
    )
  })

  it("omits actionId/conversationId when no target", async () => {
    await sendSystemNotification("Title", "Body")
    expect(mockTransport.call).toHaveBeenCalledWith("send_notification", {
      title: "Title",
      body: "Body",
      actionId: null,
      conversationId: null,
      dedupeKey: null,
    })
  })

  it("does not invoke when document is visible", async () => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => false,
    })
    await sendSystemNotification("Title", "Body", {
      kind: "conversation",
      conversationId: 1,
    })
    expect(mockTransport.call).not.toHaveBeenCalled()
  })
})
