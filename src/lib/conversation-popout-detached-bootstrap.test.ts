import { describe, expect, it } from "vitest"
import {
  buildReadyPayload,
  isAbortedPhase,
  isHandoffCompletePhase,
  parseConversationPopoutQuery,
  resolveDetachedConnectGate,
  conversationWindowLabel,
} from "@/lib/conversation-popout-detached-bootstrap"

describe("parseConversationPopoutQuery", () => {
  it("returns null for missing operationId", () => {
    expect(
      parseConversationPopoutQuery({
        conversationId: "1",
        folderId: "2",
        agentType: "claude_code",
        operationId: "",
      })
    ).toBeNull()
  })

  it("parses valid query params", () => {
    expect(
      parseConversationPopoutQuery({
        conversationId: "42",
        folderId: "7",
        agentType: "codex",
        operationId: "op-1",
      })
    ).toEqual({
      conversationId: 42,
      folderId: 7,
      agentType: "codex",
      operationId: "op-1",
    })
  })
})

describe("resolveDetachedConnectGate (claim-before-activate)", () => {
  it("keeps connect disabled until bootstrap ready", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: false,
        isLivePath: true,
        commitAcked: false,
      })
    ).toEqual({ isActive: false, suppressFrontendDisconnect: true })
  })

  it("live path: activates after claim, still suppresses disconnect until ack", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: true,
        commitAcked: false,
      })
    ).toEqual({ isActive: true, suppressFrontendDisconnect: true })
  })

  it("live path: clears suppress after commit-ack", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: true,
        commitAcked: true,
      })
    ).toEqual({ isActive: true, suppressFrontendDisconnect: false })
  })

  it("cold path: stays inactive and suppressed until commit-ack", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: false,
        commitAcked: false,
      })
    ).toEqual({ isActive: false, suppressFrontendDisconnect: true })
  })

  it("cold path: enables connect only after commit-ack", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: false,
        commitAcked: true,
      })
    ).toEqual({ isActive: true, suppressFrontendDisconnect: false })
  })
})

describe("handoff phase helpers", () => {
  it("recognizes handoff_complete variants", () => {
    expect(isHandoffCompletePhase("handoff_complete")).toBe(true)
    expect(isHandoffCompletePhase("HandoffComplete")).toBe(true)
    expect(isHandoffCompletePhase("ready_pending")).toBe(false)
  })

  it("recognizes aborted", () => {
    expect(isAbortedPhase("aborted")).toBe(true)
    expect(isAbortedPhase("handoff_complete")).toBe(false)
  })
})

describe("buildReadyPayload / label", () => {
  it("emits ready without generation on cold path", () => {
    expect(
      buildReadyPayload({
        conversationId: 9,
        operationId: "op",
      })
    ).toEqual({
      conversationId: 9,
      operationId: "op",
      ownershipGeneration: null,
      connectionId: null,
    })
  })

  it("builds conversation window label", () => {
    expect(conversationWindowLabel(5)).toBe("conversation-5")
  })
})
