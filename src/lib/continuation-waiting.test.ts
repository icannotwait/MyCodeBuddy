import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import { describe, expect, it } from "vitest"
import {
  ContinuationWaitingError,
  continuationFailureI18nKey,
  continuationWaitingError,
  isContinuationWaitingRejection,
} from "./continuation-waiting"

const LOCALE_FILES = [
  "ar",
  "de",
  "en",
  "es",
  "fr",
  "ja",
  "ko",
  "pt",
  "zh-CN",
  "zh-TW",
] as const

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

describe("continuationFailureI18nKey", () => {
  it("maps parent_connection_lost and suspend_drain_timeout to specific keys", () => {
    expect(continuationFailureI18nKey("parent_connection_lost")).toBe(
      "backendErrors.parentConnectionLost"
    )
    expect(continuationFailureI18nKey("suspend_drain_timeout")).toBe(
      "backendErrors.suspendDrainTimeout"
    )
  })

  it("maps known and unknown continuation failure codes to the generic key", () => {
    expect(continuationFailureI18nKey("arm_failed")).toBe(
      "backendErrors.continuationFailed"
    )
    expect(continuationFailureI18nKey("prompt_delivery_failed")).toBe(
      "backendErrors.continuationFailed"
    )
    expect(continuationFailureI18nKey("future_unknown_code")).toBe(
      "backendErrors.continuationFailed"
    )
  })
})

describe("continuation waiting locale coverage", () => {
  it("all ten locale JSON files contain waiting labels plus failure labels", () => {
    for (const locale of LOCALE_FILES) {
      const raw = readFileSync(
        resolve(process.cwd(), `src/i18n/messages/${locale}.json`),
        "utf8"
      )
      const messages = JSON.parse(raw) as {
        Folder: {
          chat: {
            acpConnections: {
              backendErrors: Record<string, string>
            }
            chatInput: Record<string, string>
          }
        }
      }
      const backend = messages.Folder.chat.acpConnections.backendErrors
      const chatInput = messages.Folder.chat.chatInput

      expect(chatInput.waitingForSubagents, locale).toBeTruthy()
      expect(chatInput.waitingForSubagentsHint, locale).toBeTruthy()
      expect(backend.conversationWaitingForSubagents, locale).toBeTruthy()
      expect(backend.parentConnectionLost, locale).toBeTruthy()
      expect(backend.suspendDrainTimeout, locale).toBeTruthy()
      expect(backend.continuationFailed, locale).toBeTruthy()

      if (locale !== "en") {
        // Native translations: non-English locales must not copy English strings.
        expect(chatInput.waitingForSubagents).not.toBe(
          "Waiting for subagents..."
        )
        expect(backend.parentConnectionLost).not.toBe(
          "The delegation wait ended because the agent connection was lost."
        )
      }
    }
  })
})
