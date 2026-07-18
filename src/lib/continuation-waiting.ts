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
