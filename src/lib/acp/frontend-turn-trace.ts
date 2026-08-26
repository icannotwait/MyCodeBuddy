import { getTransport } from "@/lib/transport"

export type FrontendTurnTracePhase =
  | "send_started"
  | "send_completed"
  | "prompting_frame"
  | "live_published"
  | "banner_commit"
  | "banner_paint"
  | "first_content"

export type FrontendTurnTraceOutcome =
  | "success"
  | "turn_busy"
  | "continuation_waiting"
  | "viewer_only"
  | "failed"

export interface FrontendTurnTraceMilestone {
  phase: FrontendTurnTracePhase
  contextKey?: string
  connectionId?: string
  conversationId?: number
  clientMessageId?: string
  liveMessageId?: string
  eventSeq?: number
  receivedAtMs?: number
  elapsedMs?: number
  outcome?: FrontendTurnTraceOutcome
  sinkRegistered?: boolean
  canonicalAccepted?: boolean
  transcriptPublished?: boolean
  hasLiveTranscript?: boolean
}

export function recordFrontendTurnTrace(
  milestone: FrontendTurnTraceMilestone
): void {
  try {
    void getTransport()
      .call("record_frontend_turn_trace", {
        trace: { ...milestone, clientTimestampMs: Date.now() },
      })
      .catch(() => {})
  } catch {}
}
