import { beforeEach, describe, expect, it, vi } from "vitest"

const mockTransport = vi.hoisted(() => ({
  call: vi.fn(),
}))

vi.mock("@/lib/transport", () => ({
  getTransport: () => mockTransport,
}))

// Import only after the mock declaration so `acpPrompt` closes over it.
import { acpPrompt } from "@/lib/api"

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
