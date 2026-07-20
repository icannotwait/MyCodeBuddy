import { beforeEach, describe, expect, it } from "vitest"
import {
  __resetTransferFencesForTests,
  clearTransferringOut,
  getTransferFence,
  isTransferringOut,
  markMainReleased,
  markTransferringOut,
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
})
