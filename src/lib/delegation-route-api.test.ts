import { beforeEach, describe, expect, it, vi } from "vitest"

const call = vi.fn()

vi.mock("@/lib/transport", () => ({
  getTransport: () => ({
    call,
    isDesktop: () => true,
  }),
}))

import {
  acpConnect,
  acpDisconnect,
  createChatConversation,
  createConversation,
  setConversationDelegationRoute,
  setDraftDelegationRoutePreference,
} from "@/lib/api"

describe("delegation route API parameter order + wire payloads", () => {
  beforeEach(() => {
    call.mockReset()
    call.mockResolvedValue(undefined)
  })

  it("acpConnect sends conversationId then delegationRouteOverride as trailing optionals", async () => {
    call.mockResolvedValueOnce("conn-1")
    await acpConnect(
      "codex",
      "/repo",
      "sess-1",
      "mode-a",
      { k: "v" },
      7,
      "native"
    )
    expect(call).toHaveBeenCalledWith("acp_connect", {
      agentType: "codex",
      workingDir: "/repo",
      sessionId: "sess-1",
      preferredModeId: "mode-a",
      preferredConfigValues: { k: "v" },
      conversationId: 7,
      delegationRouteOverride: "native",
      ownerOperationId: null,
    })
  })

  it("acpConnect forwards ownerOperationId for detached cold connect", async () => {
    call.mockResolvedValueOnce("conn-cold")
    await acpConnect(
      "codex",
      "/repo",
      "sess-1",
      null,
      null,
      7,
      null,
      "op-detached"
    )
    expect(call).toHaveBeenCalledWith(
      "acp_connect",
      expect.objectContaining({ ownerOperationId: "op-detached" })
    )
  })

  it("acpDisconnect passes lease fields when present", async () => {
    call.mockResolvedValueOnce(undefined)
    await acpDisconnect("conn-1", {
      expectedOwnerWindow: "conversation-7",
      expectedOperationId: "opA",
      expectedOwnershipGeneration: 3,
    })
    expect(call).toHaveBeenCalledWith("acp_disconnect", {
      connectionId: "conn-1",
      expectedOwnerWindow: "conversation-7",
      expectedOperationId: "opA",
      expectedOwnershipGeneration: 3,
      origin: "legacy_unspecified",
    })
  })

  it("acpDisconnect without lease keeps null lease fields", async () => {
    call.mockResolvedValueOnce(undefined)
    await acpDisconnect("conn-2")
    expect(call).toHaveBeenCalledWith("acp_disconnect", {
      connectionId: "conn-2",
      expectedOwnerWindow: null,
      expectedOperationId: null,
      expectedOwnershipGeneration: null,
      origin: "legacy_unspecified",
    })
  })

  it("createConversation puts route override last in the wire payload", async () => {
    call.mockResolvedValueOnce(99)
    await createConversation(3, "codex", "title", "native")
    expect(call).toHaveBeenCalledWith("create_conversation", {
      folderId: 3,
      agentType: "codex",
      title: "title",
      delegationRouteOverride: "native",
    })
  })

  it("createChatConversation puts route override last in the wire payload", async () => {
    call.mockResolvedValueOnce({
      conversationId: 5,
      folderId: 1,
      folder: {},
    })
    await createChatConversation("codex", "t", "/scratch", "codeg")
    expect(call).toHaveBeenCalledWith("create_chat_conversation", {
      agentType: "codex",
      title: "t",
      existingDir: "/scratch",
      delegationRouteOverride: "codeg",
    })
  })

  it("setConversationDelegationRoute and setDraftDelegationRoutePreference wire keys", async () => {
    call.mockResolvedValueOnce({ id: 12 })
    await setConversationDelegationRoute(12, null)
    expect(call).toHaveBeenCalledWith("set_conversation_delegation_route", {
      conversationId: 12,
      routeOverride: null,
    })
    call.mockResolvedValueOnce(undefined)
    await setDraftDelegationRoutePreference("conn-draft", "native")
    expect(call).toHaveBeenCalledWith("set_draft_delegation_route_preference", {
      connectionId: "conn-draft",
      routeOverride: "native",
    })
  })
})
