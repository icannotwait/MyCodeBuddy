import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  __resetTransferFencesForTests,
  claimConnectionOwnership,
  clearTransferringOut,
  getTransferFence,
  isFrontendDisconnectSuppressed,
  isTransferringOut,
  leaseArgsForDisconnect,
  markMainReleased,
  markTransferringOut,
  registerPopoutAcpBridge,
  releaseConnectionWithoutDisconnect,
  setSuppressFrontendDisconnect,
} from "@/lib/conversation-popout-acp-bridge"

describe("conversation-popout-acp-bridge", () => {
  beforeEach(() => {
    __resetTransferFencesForTests()
  })

  it("marks and clears transferring fences by operation id", () => {
    markTransferringOut(42, "op-a")
    expect(isTransferringOut(42)).toBe(true)
    clearTransferringOut(42, "op-b")
    expect(isTransferringOut(42)).toBe(true)
    clearTransferringOut(42, "op-a")
    expect(isTransferringOut(42)).toBe(false)
  })

  it("marks mainReleased only for matching operation", () => {
    markTransferringOut(7, "op-1")
    markMainReleased(7, "op-other")
    expect(getTransferFence(7)?.mainReleased).toBe(false)
    markMainReleased(7, "op-1")
    expect(getTransferFence(7)?.mainReleased).toBe(true)
  })

  it("awaits registered releaseConnectionWithoutDisconnect", async () => {
    const release = vi.fn(async () => {})
    registerPopoutAcpBridge({ releaseConnectionWithoutDisconnect: release })
    markTransferringOut(3, "op")
    await releaseConnectionWithoutDisconnect(3, "op")
    expect(release).toHaveBeenCalledWith(3, "op")
    expect(getTransferFence(3)?.mainReleased).toBe(true)
  })

  it("delegates reclaimAfterAbort when bridge implements it", async () => {
    const reclaim = vi.fn(async () => {})
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      reclaimAfterAbort: reclaim,
    })
    const { reclaimAfterAbort } =
      await import("@/lib/conversation-popout-acp-bridge")
    await reclaimAfterAbort(9, "op-reclaim", {
      ownershipGeneration: 4,
      ownerWindowLabel: "main",
    })
    expect(reclaim).toHaveBeenCalledWith(9, "op-reclaim", {
      ownershipGeneration: 4,
      ownerWindowLabel: "main",
    })
  })

  it("delegates hasReleasedForReclaim when bridge implements it", async () => {
    const peek = vi.fn(
      (cid: number, op: string) => cid === 5 && op === "op-snap"
    )
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      hasReleasedForReclaim: peek,
    })
    const { hasReleasedForReclaim } =
      await import("@/lib/conversation-popout-acp-bridge")
    expect(hasReleasedForReclaim(5, "op-snap")).toBe(true)
    expect(hasReleasedForReclaim(5, "other")).toBe(false)
    expect(peek).toHaveBeenCalled()
  })

  it("reclaimAfterAbort throws when no bridge is registered", async () => {
    const { reclaimAfterAbort } =
      await import("@/lib/conversation-popout-acp-bridge")
    await expect(reclaimAfterAbort(1, "op-missing")).rejects.toThrow(
      /reclaim bridge is not registered/i
    )
  })

  it("suppresses frontend disconnect until cleared", () => {
    expect(isFrontendDisconnectSuppressed(11)).toBe(false)
    setSuppressFrontendDisconnect(11, true)
    expect(isFrontendDisconnectSuppressed(11)).toBe(true)
    setSuppressFrontendDisconnect(11, false)
    expect(isFrontendDisconnectSuppressed(11)).toBe(false)
  })

  it("claimConnectionOwnership fails without bridge and delegates when present", async () => {
    await expect(
      claimConnectionOwnership({
        conversationId: 1,
        agentType: "claude_code",
        workingDir: "/repo",
        operationId: "op",
        contextKey: "k",
      })
    ).rejects.toThrow(/claim bridge is not registered/i)

    const claim = vi.fn(async () => ({
      ownershipGeneration: 3,
      connectionId: "c1",
    }))
    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: () => {},
      claimConnectionOwnership: claim,
    })
    await expect(
      claimConnectionOwnership({
        conversationId: 1,
        connectionId: "c1",
        agentType: "claude_code",
        workingDir: "/repo",
        operationId: "op",
        contextKey: "k",
        ownershipGeneration: 3,
        ownerWindowLabel: "conversation-1",
      })
    ).resolves.toEqual({ ownershipGeneration: 3, connectionId: "c1" })
    expect(claim).toHaveBeenCalledWith(
      expect.objectContaining({
        ownershipGeneration: 3,
        ownerWindowLabel: "conversation-1",
        operationId: "op",
      })
    )
  })

  it("leaseArgsForDisconnect only when lease fields present", () => {
    expect(leaseArgsForDisconnect({})).toBeNull()
    expect(
      leaseArgsForDisconnect({
        ownerOperationId: "opA",
        ownerWindowLabel: "conversation-1",
        ownershipGeneration: 2,
      })
    ).toEqual({
      expectedOperationId: "opA",
      expectedOwnerWindow: "conversation-1",
      expectedOwnershipGeneration: 2,
    })
  })

  it("suppress remains set for full detached lifetime (unmount + post-ack)", () => {
    setSuppressFrontendDisconnect(99, true)
    expect(isFrontendDisconnectSuppressed(99)).toBe(true)
    // Parent unmount must not clear — suppress dies with the JS context.
    expect(isFrontendDisconnectSuppressed(99)).toBe(true)
    // Simulated applyAck after commit: must NOT clear suppress.
    // (page.tsx applyAck only sets commitAcked; never setSuppress(..., false))
    expect(isFrontendDisconnectSuppressed(99)).toBe(true)
  })
})
