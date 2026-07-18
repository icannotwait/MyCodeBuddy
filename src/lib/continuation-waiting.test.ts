import { describe, expect, it } from "vitest"
import {
  ContinuationWaitingError,
  continuationWaitingError,
  isContinuationWaitingRejection,
} from "./continuation-waiting"

describe("ContinuationWaitingError", () => {
  it("recognizes the structured waiting rejection and preserves its fields", () => {
    const rejection = {
      code: "conversation_waiting_for_subagents",
      message: "Conversation is waiting for subagents",
      i18n_params: { conversationId: "42", state: "arming" },
    }

    expect(isContinuationWaitingRejection(rejection)).toBe(true)
    const error = continuationWaitingError(rejection)
    expect(error).toBeInstanceOf(ContinuationWaitingError)
    expect(error.conversationId).toBe(42)
    expect(error.continuationState).toBe("arming")
  })

  it("does not treat turn-busy errors as continuation waiting", () => {
    expect(
      isContinuationWaitingRejection({
        code: "turn_in_progress",
        message: "turn already in progress for this connection",
      })
    ).toBe(false)
  })
})
