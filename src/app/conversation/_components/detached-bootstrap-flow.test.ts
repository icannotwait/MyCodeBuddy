import { describe, expect, it } from "vitest"
import {
  resolveDetachedConnectGate,
  buildReadyPayload,
  conversationWindowLabel,
} from "@/lib/conversation-popout-detached-bootstrap"

/**
 * Detached bootstrap ordering tests (claim-before-activate / cold until ack).
 * Mirrors the page effect sequence without mounting Next/Tauri.
 */
describe("detached bootstrap flow", () => {
  it("live: does not activate before claim (bootstrapReady false)", () => {
    const gate = resolveDetachedConnectGate({
      bootstrapReady: false,
      isLivePath: true,
      commitAcked: false,
    })
    expect(gate.isActive).toBe(false)
    expect(gate.suppressFrontendDisconnect).toBe(true)
  })

  it("live: after claim, active but still suppress until ack", () => {
    const gate = resolveDetachedConnectGate({
      bootstrapReady: true,
      isLivePath: true,
      commitAcked: false,
    })
    expect(gate.isActive).toBe(true)
    expect(gate.suppressFrontendDisconnect).toBe(true)
  })

  it("cold: ready payload has no generation; connect gated until ack", () => {
    const payload = buildReadyPayload({
      conversationId: 1,
      operationId: "op-cold",
    })
    expect(payload.ownershipGeneration).toBeNull()
    expect(payload.connectionId).toBeNull()

    const before = resolveDetachedConnectGate({
      bootstrapReady: true,
      isLivePath: false,
      commitAcked: false,
    })
    expect(before.isActive).toBe(false)

    const after = resolveDetachedConnectGate({
      bootstrapReady: true,
      isLivePath: false,
      commitAcked: true,
    })
    expect(after.isActive).toBe(true)
    expect(after.suppressFrontendDisconnect).toBe(false)
  })

  it("uses conversation-{id} label for rebind target", () => {
    expect(conversationWindowLabel(12)).toBe("conversation-12")
  })
})
