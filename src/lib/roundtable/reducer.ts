import type { RoundtableEvent, RoundtableView } from "@/lib/roundtable/types"

export const initialRoundtableView: RoundtableView = {
  status: "running",
  accepted: [],
  preview: null,
  catchingUp: false,
  lastSeq: 0,
  previewIncarnation: null,
  previewAttemptId: null,
  previewSeq: -1,
  previewClosed: false,
  closedPreviewKeys: [],
}

export function reduceRoundtable(
  view: RoundtableView,
  event: RoundtableEvent
): RoundtableView {
  if (!Number.isSafeInteger(event.seq) || event.seq < 0) return view
  if (event.kind === "gap") {
    return { ...view, status: "catching_up", catchingUp: true, preview: null }
  }
  if (event.kind === "preview" || event.kind === "preview_reset") {
    if (view.catchingUp) return view
    const incarnation = event.incarnation ?? 0
    const attemptId = event.attemptId ?? null
    const key = previewKey(incarnation, attemptId)
    if (view.closedPreviewKeys.includes(key)) return view
    const sameAttempt =
      view.previewIncarnation === incarnation &&
      view.previewAttemptId === attemptId
    if (event.kind === "preview_reset") {
      if (sameAttempt && (view.previewClosed || event.seq < view.previewSeq)) {
        return view
      }
      return {
        ...view,
        preview: null,
        previewIncarnation: incarnation,
        previewAttemptId: attemptId,
        previewSeq: event.seq,
        previewClosed: false,
      }
    }
    if (view.previewIncarnation !== null && !sameAttempt) return view
    if (view.previewClosed || event.seq <= view.previewSeq) return view
    return {
      ...view,
      preview: event.text ?? null,
      previewIncarnation: incarnation,
      previewAttemptId: attemptId,
      previewSeq: event.seq,
    }
  }
  if (event.seq <= view.lastSeq || view.catchingUp) return view
  if (event.seq !== view.lastSeq + 1) {
    return { ...view, status: "catching_up", catchingUp: true, preview: null }
  }
  if (event.kind === "projection") {
    if (!event.snapshot || event.snapshot.highWaterSeq !== event.seq) {
      return { ...view, status: "catching_up", catchingUp: true, preview: null }
    }
    const synced = resyncRoundtable(
      event.snapshot.accepted,
      event.snapshot.highWaterSeq,
      event.snapshot.status,
      event.snapshot.closedAttemptIds
    )
    return {
      ...synced,
      closedPreviewKeys: [
        ...view.closedPreviewKeys,
        ...synced.closedPreviewKeys,
      ],
    }
  }
  if (event.kind !== "accepted") {
    return { ...view, status: "catching_up", catchingUp: true, preview: null }
  }
  return {
    ...view,
    preview: null,
    accepted: [...view.accepted, event.text ?? ""],
    lastSeq: event.seq,
    previewClosed: true,
    closedPreviewKeys: [
      ...view.closedPreviewKeys,
      previewKey(
        event.incarnation ?? view.previewIncarnation,
        event.attemptId ?? view.previewAttemptId
      ),
    ],
  }
}

export function resyncRoundtable(
  accepted: string[],
  highWaterSeq: number,
  status: RoundtableView["status"] = "running",
  closedAttemptIds: string[] = []
): RoundtableView {
  if (!Number.isSafeInteger(highWaterSeq) || highWaterSeq < 0) {
    throw new Error("Invalid roundtable replay watermark")
  }
  return {
    ...initialRoundtableView,
    status,
    accepted: [...accepted],
    preview: null,
    catchingUp: false,
    lastSeq: highWaterSeq,
    previewClosed: true,
    closedPreviewKeys: closedAttemptIds.map((id) => previewKey(null, id)),
  }
}

function previewKey(
  incarnation: string | number | null,
  attemptId: string | null
) {
  return attemptId === null
    ? JSON.stringify([incarnation, null])
    : `attempt:${attemptId}`
}
