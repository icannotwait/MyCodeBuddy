import { describe, expect, it } from "vitest"
import {
  buildReadyPayload,
  claimResultMatchesRebind,
  classifyDiscoveryResult,
  conversationWindowLabel,
  decideLiveHandoffResult,
  FOCUS_COMPOSER_EVENT,
  isAbortedPhase,
  isHandoffCompletePhase,
  parseConversationPopoutQuery,
  resolveDetachedConnectGate,
  shouldClearSuppressOnDetachedCommitAck,
  shouldClearSuppressOnDetachedUnmount,
  shouldMountDetachedSurface,
  shouldReverseRebindAfterLiveFailure,
} from "@/lib/conversation-popout-detached-bootstrap"

describe("FOCUS_COMPOSER_EVENT", () => {
  it("matches the Rust eval CustomEvent name for pop-out activate", () => {
    // Keep in sync with REQUEST_COMPOSER_FOCUS_JS in conversation_popout.rs
    expect(FOCUS_COMPOSER_EVENT).toBe("codeg:focus-composer")
  })
})

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

  it("keeps suppress after commit ack", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: true,
        commitAcked: true,
      }).suppressFrontendDisconnect
    ).toBe(true)
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: true,
        commitAcked: true,
      })
    ).toEqual({ isActive: true, suppressFrontendDisconnect: true })
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

  it("cold path: enables connect only after commit-ack (suppress stays)", () => {
    expect(
      resolveDetachedConnectGate({
        bootstrapReady: true,
        isLivePath: false,
        commitAcked: true,
      })
    ).toEqual({ isActive: true, suppressFrontendDisconnect: true })
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

describe("classifyDiscoveryResult", () => {
  it("treats null discovery as true cold (none)", () => {
    expect(classifyDiscoveryResult({ discovered: null, error: null })).toEqual({
      kind: "none",
    })
  })

  it("classifies live connection id", () => {
    expect(
      classifyDiscoveryResult({
        discovered: { connection_id: "c-1" },
        error: null,
      })
    ).toEqual({ kind: "live", connectionId: "c-1" })
  })

  it("does not collapse discovery transport errors into cold", () => {
    expect(
      classifyDiscoveryResult({
        discovered: null,
        error: new Error("transport down"),
        errorMessage: "transport down",
      })
    ).toEqual({ kind: "error", message: "transport down" })
  })
})

describe("claimResultMatchesRebind", () => {
  it("requires matching connectionId and ownershipGeneration", () => {
    expect(
      claimResultMatchesRebind({
        claimResult: { connectionId: "c1", ownershipGeneration: 3 },
        expectedConnectionId: "c1",
        expectedOwnershipGeneration: 3,
      })
    ).toBe(true)
    expect(
      claimResultMatchesRebind({
        claimResult: { connectionId: "c1" },
        expectedConnectionId: "c1",
        expectedOwnershipGeneration: 3,
      })
    ).toBe(false)
    expect(
      claimResultMatchesRebind({
        claimResult: { connectionId: "c1", ownershipGeneration: 9 },
        expectedConnectionId: "c1",
        expectedOwnershipGeneration: 3,
      })
    ).toBe(false)
    expect(
      claimResultMatchesRebind({
        claimResult: { connectionId: "other", ownershipGeneration: 3 },
        expectedConnectionId: "c1",
        expectedOwnershipGeneration: 3,
      })
    ).toBe(false)
    expect(
      claimResultMatchesRebind({
        claimResult: null,
        expectedConnectionId: "c1",
        expectedOwnershipGeneration: 3,
      })
    ).toBe(false)
  })

  it("generation mismatch is claim failure that requires reverse", () => {
    const matches = claimResultMatchesRebind({
      claimResult: { connectionId: "c1", ownershipGeneration: 1 },
      expectedConnectionId: "c1",
      expectedOwnershipGeneration: 4,
    })
    expect(matches).toBe(false)
    const d = decideLiveHandoffResult({
      connectionId: "c1",
      rebindError: null,
      ownershipGeneration: 4,
      claimError: new Error(
        "claimConnectionOwnership did not confirm the rebinding connection"
      ),
      claimErrorMessage:
        "claimConnectionOwnership did not confirm the rebinding connection",
    })
    expect(d.kind).toBe("failed")
    if (d.kind !== "failed") return
    expect(
      shouldReverseRebindAfterLiveFailure({
        rebindSucceeded: d.rebindSucceeded,
        ownershipGeneration: d.ownershipGeneration,
      })
    ).toBe(true)
  })
})

describe("decideLiveHandoffResult / reverse rebind", () => {
  it("succeeds only when rebind+claim both ok with generation", () => {
    expect(
      decideLiveHandoffResult({
        connectionId: "c1",
        rebindError: null,
        ownershipGeneration: 4,
        claimError: null,
      })
    ).toEqual({
      kind: "success",
      connectionId: "c1",
      ownershipGeneration: 4,
    })
  })

  it("rebind failure: no ready, no reverse", () => {
    const d = decideLiveHandoffResult({
      connectionId: "c1",
      rebindError: new Error("cas"),
      rebindErrorMessage: "cas",
      ownershipGeneration: null,
      claimError: null,
    })
    expect(d.kind).toBe("failed")
    if (d.kind !== "failed") return
    expect(d.rebindSucceeded).toBe(false)
    expect(
      shouldReverseRebindAfterLiveFailure({
        rebindSucceeded: d.rebindSucceeded,
        ownershipGeneration: d.ownershipGeneration,
      })
    ).toBe(false)
  })

  it("claim failure after rebind: reverse required, no ready", () => {
    const d = decideLiveHandoffResult({
      connectionId: "c1",
      rebindError: null,
      ownershipGeneration: 7,
      claimError: new Error("claim"),
      claimErrorMessage: "claim",
    })
    expect(d).toEqual({
      kind: "failed",
      message: "claim",
      rebindSucceeded: true,
      connectionId: "c1",
      ownershipGeneration: 7,
    })
    expect(
      shouldReverseRebindAfterLiveFailure({
        rebindSucceeded: true,
        ownershipGeneration: 7,
      })
    ).toBe(true)
  })
})

describe("shouldMountDetachedSurface / suppress unmount policy", () => {
  it("cold path: does not mount overlays until isActive (commit-ack)", () => {
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

  it("never clears suppress on detached parent unmount", () => {
    expect(shouldClearSuppressOnDetachedUnmount()).toBe(false)
  })

  it("never clears suppress on detached commit-ack (full window lifetime)", () => {
    expect(shouldClearSuppressOnDetachedCommitAck()).toBe(false)
  })
})
