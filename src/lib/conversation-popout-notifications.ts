import { toast } from "sonner"

import {
  isPopOutPopupBlockedError,
  isPopOutRuntimeRestartRequiredError,
} from "@/lib/conversation-popout"
import { relaunchApp } from "@/lib/updater"

export const CONVERSATION_POPOUT_RUNTIME_RESTART_TOAST_ID =
  "conversation-popout-runtime-restart-required"

export interface ConversationPopoutFailureMessages {
  popupBlocked: string
  handoffFailed: string
  runtimeRestartRequired: string
  restartAction: string
  restartFailed: string
}

export function notifyConversationPopoutFailure(
  error: unknown,
  messages: ConversationPopoutFailureMessages
): void {
  if (isPopOutRuntimeRestartRequiredError(error)) {
    toast.error(messages.runtimeRestartRequired, {
      id: CONVERSATION_POPOUT_RUNTIME_RESTART_TOAST_ID,
      duration: Infinity,
      action: {
        label: messages.restartAction,
        onClick: async () => {
          try {
            await relaunchApp()
          } catch (relaunchError) {
            console.error("[ConversationPopout] relaunch failed", relaunchError)
            toast.error(messages.restartFailed)
          }
        },
      },
    })
    return
  }

  toast.error(
    isPopOutPopupBlockedError(error)
      ? messages.popupBlocked
      : messages.handoffFailed
  )
}
