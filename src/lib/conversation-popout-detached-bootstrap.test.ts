import { describe, expect, it } from "vitest"
import { ALL_AGENT_TYPES } from "@/lib/types"
import { buildWebConversationPopoutUrl } from "@/lib/conversation-popout"
import {
  buildReadyPayload,
  claimResultMatchesRebind,
  classifyDiscoveryResult,
  conversationWindowLabel,
  decideLiveHandoffResult,
  FOCUS_COMPOSER_EVENT,
  isAbortedPhase,
  isHandoffCompletePhase,
  parseConversationRouteAgentType,
  parseConversationPopoutQuery,
  resolveConversationRouteMode,
  resolveDetachedConnectGate,
  shouldClearSuppressOnDetachedCommitAck,
  shouldClearSuppressOnDetachedUnmount,
  shouldMountDetachedSurface,
  shouldReverseRebindAfterLiveFailure,
} from "@/lib/conversation-popout-detached-bootstrap"

describe("parseConversationRouteAgentType", () => {
  it.each(ALL_AGENT_TYPES)("accepts builtin %s", (agentType) => {
    expect(parseConversationRouteAgentType(agentType)).toBe(agentType)
  })

  it("accepts a syntactically valid registered custom wire id", () => {
    expect(parseConversationRouteAgentType("custom:goose")).toBe("custom:goose")
  })

  it.each([
    null,
    "unknown",
    "custom:",
    "custom:.hidden",
    "custom:Goose",
    "custom:a/b",
    `custom:${"a".repeat(65)}`,
  ])("rejects unsupported or malformed agent wire value %s", (agentType) => {
    expect(parseConversationRouteAgentType(agentType)).toBeNull()
  })
})

describe("FOCUS_COMPOSER_EVENT", () => {
  it("matches the Rust eval CustomEvent name for pop-out activate", () => {
    // Keep in sync with REQUEST_COMPOSER_FOCUS_JS in conversation_popout.rs
    expect(FOCUS_COMPOSER_EVENT).toBe("codeg:focus-composer")
  })
})

describe("parseConversationPopoutQuery", () => {
  const shared = {
    conversationId: "42",
    folderId: "7",
    agentType: "codex",
  }

  it("parses a valid desktop route with a required operationId", () => {
    expect(
      parseConversationPopoutQuery({
        ...shared,
        operationId: "op-1",
        mode: null,
      })
    ).toEqual({
      kind: "desktop",
      conversationId: 42,
      folderId: 7,
      agentType: "codex",
      operationId: "op-1",
    })
  })

  it("parses mode=web and ignores operationId", () => {
    expect(
      parseConversationPopoutQuery({
        ...shared,
        operationId: "ignored-desktop-token",
        mode: "web",
      })
    ).toEqual({
      kind: "web",
      conversationId: 42,
      folderId: 7,
      agentType: "codex",
    })
  })

  it("round trips custom:goose from the web URL builder through the parser", () => {
    const built = new URL(
      buildWebConversationPopoutUrl({
        conversationId: 42,
        folderId: 7,
        agentType: "custom:goose",
      }),
      "http://codeg.test"
    )

    expect(
      parseConversationPopoutQuery({
        conversationId: built.searchParams.get("conversationId"),
        folderId: built.searchParams.get("folderId"),
        agentType: built.searchParams.get("agentType"),
        operationId: built.searchParams.get("operationId"),
        mode: built.searchParams.get("mode"),
      })
    ).toEqual({
      kind: "web",
      conversationId: 42,
      folderId: 7,
      agentType: "custom:goose",
    })
  })

  it("rejects a web-shaped query that omitted mode=web", () => {
    expect(
      parseConversationPopoutQuery({
        ...shared,
        operationId: null,
        mode: null,
      })
    ).toBeNull()
  })

  it.each([
    { conversationId: "0", folderId: "7", agentType: "codex" },
    { conversationId: "1.5", folderId: "7", agentType: "codex" },
    { conversationId: "42", folderId: "-1", agentType: "codex" },
    { conversationId: "42", folderId: "7.5", agentType: "codex" },
    { conversationId: "42", folderId: "7", agentType: "unknown" },
    { conversationId: "42", folderId: "7", agentType: "custom:" },
    { conversationId: "42", folderId: "7", agentType: "custom:.hidden" },
    { conversationId: "42", folderId: "7", agentType: "custom:Goose" },
    { conversationId: "42", folderId: "7", agentType: "custom:a/b" },
    {
      conversationId: "42",
      folderId: "7",
      agentType: `custom:${"a".repeat(65)}`,
    },
  ])("rejects invalid shared fields: %o", (invalid) => {
    expect(
      parseConversationPopoutQuery({
        ...invalid,
        operationId: null,
        mode: "web",
      })
    ).toBeNull()
  })

  it("rejects whitespace desktop operation ids and unknown modes", () => {
    expect(
      parseConversationPopoutQuery({
        ...shared,
        operationId: "   ",
        mode: null,
      })
    ).toBeNull()
    expect(
      parseConversationPopoutQuery({
        ...shared,
        operationId: null,
        mode: "desktop",
      })
    ).toBeNull()
  })
})

describe("resolveConversationRouteMode", () => {
  const desktopRoute = {
    kind: "desktop" as const,
    conversationId: 42,
    folderId: 7,
    agentType: "codex" as const,
    operationId: "op-1",
  }
  const webRoute = {
    kind: "web" as const,
    conversationId: 42,
    folderId: 7,
    agentType: "codex" as const,
  }

  it.each([
    [desktopRoute, true, true, "desktop"],
    [webRoute, false, false, "web"],
    [desktopRoute, false, false, "unsupported"],
    [webRoute, true, true, "unsupported"],
    [desktopRoute, true, false, "unsupported"],
    [webRoute, true, false, "unsupported"],
    [null, false, false, "invalid"],
  ] as const)(
    "routes %o for desktop=%s local=%s as %s",
    (route, desktop, localDesktop, expected) => {
      expect(
        resolveConversationRouteMode({
          route,
          isDesktop: desktop,
          isLocalDesktop: localDesktop,
        })
      ).toBe(expected)
    }
  )
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
