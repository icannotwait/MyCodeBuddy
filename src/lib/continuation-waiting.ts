const CONTINUATION_WAITING_CODE = "conversation_waiting_for_subagents"

export class ContinuationWaitingError extends Error {
  constructor(
    public readonly conversationId: number | null,
    public readonly continuationState: string | null
  ) {
    super("conversation is waiting for subagents")
    this.name = "ContinuationWaitingError"
  }
}

type WaitingPayload = {
  code?: unknown
  i18n_params?: { conversationId?: unknown; state?: unknown }
}

export function isContinuationWaitingRejection(error: unknown): boolean {
  return (
    !!error &&
    typeof error === "object" &&
    (error as WaitingPayload).code === CONTINUATION_WAITING_CODE
  )
}

export function continuationWaitingError(
  error: unknown
): ContinuationWaitingError {
  const params =
    error && typeof error === "object"
      ? (error as WaitingPayload).i18n_params
      : undefined
  const rawConversationId = params?.conversationId
  const conversationId =
    typeof rawConversationId === "string" && /^-?\d+$/.test(rawConversationId)
      ? Number(rawConversationId)
      : typeof rawConversationId === "number" &&
          Number.isInteger(rawConversationId)
        ? rawConversationId
        : null
  const continuationState =
    typeof params?.state === "string" ? params.state : null
  return new ContinuationWaitingError(conversationId, continuationState)
}

/** i18n key under `Folder.chat.acpConnections` for a redacted continuation failure. */
export type ContinuationFailureI18nKey =
  | "backendErrors.parentConnectionLost"
  | "backendErrors.suspendDrainTimeout"
  | "backendErrors.continuationFailed"

/**
 * Map a durable/live continuation failure code to a stable i18n key.
 * Known special cases keep specific copy; every other/current/future code uses
 * the generic continuation-failed message so cold projection and live error
 * events cannot drift.
 */
export function continuationFailureI18nKey(
  code: string | null | undefined
): ContinuationFailureI18nKey {
  switch (code) {
    case "parent_connection_lost":
      return "backendErrors.parentConnectionLost"
    case "suspend_drain_timeout":
      return "backendErrors.suspendDrainTimeout"
    default:
      return "backendErrors.continuationFailed"
  }
}

/** Known ACP error `code` values that belong to the continuation-failure family. */
export function isContinuationFailureCode(
  code: string | null | undefined
): boolean {
  if (!code) return false
  switch (code) {
    case "arm_failed":
    case "suspend_dispatch_failed":
    case "suspend_drain_timeout":
    case "parent_connection_lost":
    case "prompt_delivery_failed":
    case "state_conflict":
      return true
    default:
      return false
  }
}
