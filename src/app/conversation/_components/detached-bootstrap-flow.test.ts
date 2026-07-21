import { describe, expect, it } from "vitest"
import {
  buildReadyPayload,
  classifyDiscoveryResult,
  conversationWindowLabel,
  decideLiveHandoffResult,
  resolveDetachedConnectGate,
  shouldClearSuppressOnDetachedUnmount,
  shouldMountDetachedSurface,
  shouldReverseRebindAfterLiveFailure,
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

  it("live rebind failure does not allow ready / mount", () => {
    const discovery = classifyDiscoveryResult({
      discovered: { connection_id: "live-1" },
      error: null,
    })
    expect(discovery.kind).toBe("live")

    const decision = decideLiveHandoffResult({
      connectionId: "live-1",
      rebindError: new Error("cas lost"),
      rebindErrorMessage: "cas lost",
      ownershipGeneration: null,
      claimError: null,
    })
    expect(decision.kind).toBe("failed")

    // Page must not set bootstrapReady / readyEmitted on this path.
    const gate = resolveDetachedConnectGate({
      bootstrapReady: false,
      isLivePath: false,
      commitAcked: false,
    })
    expect(gate.isActive).toBe(false)
    expect(
      shouldMountDetachedSurface({
        valid: true,
        hasError: true,
        bootstrapReady: false,
        readyEmitted: false,
        isActive: gate.isActive,
      })
    ).toBe(false)
  })

  it("discovery error is not cold ready", () => {
    const discovery = classifyDiscoveryResult({
      discovered: null,
      error: new Error("rpc failed"),
      errorMessage: "rpc failed",
    })
    expect(discovery.kind).toBe("error")
    // True cold only when discovery succeeds with no connection.
    expect(
      classifyDiscoveryResult({ discovered: null, error: null }).kind
    ).toBe("none")
  })

  it("claim failure after rebind requires reverse; suppress stays on unmount", () => {
    const decision = decideLiveHandoffResult({
      connectionId: "live-2",
      rebindError: null,
      ownershipGeneration: 3,
      claimError: new Error("claim"),
      claimErrorMessage: "claim",
    })
    expect(decision.kind).toBe("failed")
    if (decision.kind === "failed") {
      expect(
        shouldReverseRebindAfterLiveFailure({
          rebindSucceeded: decision.rebindSucceeded,
          ownershipGeneration: decision.ownershipGeneration,
        })
      ).toBe(true)
    }
    expect(shouldClearSuppressOnDetachedUnmount()).toBe(false)
  })

  it("cold: surface/overlays deferred until commit-ack activation", () => {
    const readyPayload = buildReadyPayload({
      conversationId: 2,
      operationId: "op",
    })
    expect(readyPayload.connectionId).toBeNull()

    expect(
      shouldMountDetachedSurface({
        valid: true,
        hasError: false,
        bootstrapReady: true,
        readyEmitted: true,
        isActive: false,
      })
    ).toBe(false)

    expect(
      shouldMountDetachedSurface({
        valid: true,
        hasError: false,
        bootstrapReady: true,
        readyEmitted: true,
        isActive: true,
      })
    ).toBe(true)
  })
})
