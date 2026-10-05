import type { RoundtableEvent, RoundtableView } from "@/lib/roundtable/types"

export const initialRoundtableView: RoundtableView = {
  status: "running",
  accepted: [],
  preview: null,
  catchingUp: false,
}

export function reduceRoundtable(
  view: RoundtableView,
  event: RoundtableEvent,
  seen: Set<number>
): RoundtableView {
  if (seen.has(event.seq)) return view
  seen.add(event.seq)
  if (event.kind === "preview") {
    return { ...view, preview: event.text ?? null }
  }
  if (event.kind === "gap") {
    return { ...view, status: "catching_up", catchingUp: true, preview: null }
  }
  return {
    ...view,
    preview: null,
    accepted: [...view.accepted, event.text ?? ""],
  }
}

export function resyncRoundtable(accepted: string[]): RoundtableView {
  return {
    status: "running",
    accepted,
    preview: null,
    catchingUp: false,
  }
}
