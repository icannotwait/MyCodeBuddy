import { describe, expect, it } from "vitest"

import { isConversationInterruptedAgentText } from "@/lib/delegation-conversation-interrupted"

describe("isConversationInterruptedAgentText", () => {
  it("accepts star-wrapped full string", () => {
    expect(
      isConversationInterruptedAgentText("*Conversation interrupted*")
    ).toBe(true)
  })

  it("accepts underscore-wrapped full string", () => {
    expect(
      isConversationInterruptedAgentText("_Conversation interrupted_")
    ).toBe(true)
  })

  it("trims surrounding whitespace before matching", () => {
    expect(
      isConversationInterruptedAgentText("  *Conversation interrupted*  \n")
    ).toBe(true)
    expect(
      isConversationInterruptedAgentText("\t_Conversation interrupted_\n")
    ).toBe(true)
  })

  it("rejects bare text without emphasis markers", () => {
    expect(isConversationInterruptedAgentText("Conversation interrupted")).toBe(
      false
    )
  })

  it("rejects bold multi-marker wrappers", () => {
    expect(
      isConversationInterruptedAgentText("**Conversation interrupted**")
    ).toBe(false)
    expect(
      isConversationInterruptedAgentText("__Conversation interrupted__")
    ).toBe(false)
  })

  it("rejects multi-paragraph and partial matches", () => {
    expect(
      isConversationInterruptedAgentText(
        "*Conversation interrupted*\n\nMore text"
      )
    ).toBe(false)
    expect(
      isConversationInterruptedAgentText("Before *Conversation interrupted*")
    ).toBe(false)
    expect(
      isConversationInterruptedAgentText("*Conversation interrupted* after")
    ).toBe(false)
    expect(isConversationInterruptedAgentText("*Conversation*")).toBe(false)
    expect(isConversationInterruptedAgentText("")).toBe(false)
  })
})
