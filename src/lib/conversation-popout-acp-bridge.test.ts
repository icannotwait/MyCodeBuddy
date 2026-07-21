import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  __resetTransferFencesForTests,
  claimConnectionOwnership,
  clearTransferringOut,
  getTransferFence,
  isFrontendDisconnectSuppressed,
  isTransferringOut,
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

  it("suppresses frontend disconnect until cleared", () => {
    expect(isFrontendDisconnectSuppressed(11)).toBe(false)
    setSuppressFrontendDisconnect(11, true)
    expect(isFrontendDisconnectSuppressed(11)).toBe(true)
    setSuppressFrontendDisconnect(11, false)
    expect(isFrontendDisconnectSuppressed(11)).toBe(false)
  })

  it("claimConnectionOwnership is null-safe and delegates", async () => {
    await expect(
      claimConnectionOwnership({
        conversationId: 1,
        agentType: "claude_code",
        workingDir: "/repo",
        operationId: "op",
        contextKey: "k",
      })
    ).resolves.toBeUndefined()

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
      })
    ).resolves.toEqual({ ownershipGeneration: 3, connectionId: "c1" })
    expect(claim).toHaveBeenCalled()
  })
})
