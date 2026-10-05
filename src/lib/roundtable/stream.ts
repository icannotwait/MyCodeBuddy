export interface PreviewChunk {
  incarnation: number
  seq: number
}

export function coalescePreview(
  chunks: PreviewChunk[],
  currentIncarnation: number
) {
  const live = chunks.filter(
    (chunk) => chunk.incarnation === currentIncarnation
  )
  if (live.length === 0) return null
  return { first: live[0].seq, last: live[live.length - 1].seq }
}

export function resyncAfterReconnect(
  serverAccepted: string[],
  highWaterSeq: number,
  status: RoundtableStatus = "running",
  closedAttemptIds: string[] = []
) {
  return resyncRoundtable(
    serverAccepted,
    highWaterSeq,
    status,
    closedAttemptIds
  )
}
import { reduceRoundtable, resyncRoundtable } from "@/lib/roundtable/reducer"
import type {
  RoundtableStatus,
  RoundtableView,
  RoundtableProjection,
  RoundtablePreviewFrame,
} from "@/lib/roundtable/types"

export function applyRoundtablePreview(
  view: RoundtableView,
  payload: unknown,
  projection: RoundtableProjection,
  subscriptionId: string
): RoundtableView {
  if (!payload || typeof payload !== "object") return view
  const frame = payload as RoundtablePreviewFrame
  if (
    frame.room_id !== projection.body.room_id ||
    frame.subscription_id !== subscriptionId ||
    frame.run_epoch !== projection.body.run_epoch ||
    projection.body.status !== "running" ||
    typeof frame.text !== "string" ||
    typeof frame.incarnation !== "string"
  )
    return view
  const attempt = projection.body.replay.attempts.find(
    (item) => item.attempt_id === frame.attempt_id
  )
  if (
    !attempt ||
    !["admitted", "streaming", "validating"].includes(attempt.state)
  )
    return view
  const turn = projection.body.replay.turns.find(
    (item) =>
      item.turn_id === attempt.turn_id && item.speaker_id === frame.speaker_id
  )
  const phase = projection.body.phase_refs.find(
    (item) =>
      item.phase_id === turn?.phase_id &&
      item.revision === frame.phase_revision &&
      item.state === "running"
  )
  if (
    !phase ||
    (view.previewIncarnation !== null &&
      view.previewIncarnation !== frame.incarnation)
  )
    return view
  const rawSeq =
    frame.reset_baseline_seq ?? frame.last_chunk_seq ?? frame.chunk_seq
  if (typeof rawSeq !== "string" || !/^(0|[1-9][0-9]*)$/.test(rawSeq))
    return view
  const seq = Number(rawSeq)
  if (!Number.isSafeInteger(seq)) return view
  const text = (view.preview ?? "") + frame.text
  if (new TextEncoder().encode(text).byteLength > 65536) return view
  return reduceRoundtable(view, {
    kind: frame.reset_baseline_seq !== undefined ? "preview_reset" : "preview",
    seq,
    text,
    incarnation: frame.incarnation,
    attemptId: frame.attempt_id,
  })
}
