import type { RoundtableCommandName } from "./api"

export interface PendingRoundtableMutation {
  key: string
  command: RoundtableCommandName
  request: Record<string, unknown>
  extra: Record<string, unknown>
  paid: boolean
  uncertain: boolean
}

/** A fresh confirmation token is authorization, not a new user intent. */
export function prepareRoundtableMutation(
  pending: PendingRoundtableMutation | null,
  command: RoundtableCommandName,
  roomId: string | undefined,
  revision: string | undefined,
  extra: Record<string, unknown>,
  newId: () => string
): PendingRoundtableMutation {
  const intent = { ...extra }
  delete intent.confirmed_preflight_id
  const key = JSON.stringify([command, roomId, intent])
  if (pending?.key === key) return pending
  const cancellation = command === "roundtable_pause" || command === "roundtable_stop"
  if (pending?.paid && pending.uncertain && !cancellation) {
    throw new Error("paid_outcome_unknown")
  }
  const paid = [
    "roundtable_start",
    "roundtable_resume",
    "roundtable_retry_synthesis",
  ].includes(command) ||
    (command === "roundtable_interject" && extra.mode === "restart_current")
  // Preserve the serialized payload, not mutable references to UI form state.
  const request = JSON.parse(JSON.stringify({
    ...(roomId ? { room_id: roomId, expected_revision: revision } : {}),
    ...extra,
    request_id: newId(),
  })) as Record<string, unknown>
  return {
    key, command, request,
    extra: JSON.parse(JSON.stringify(extra)) as Record<string, unknown>,
    paid, uncertain: false,
  }
}

export function isDefinitiveRoundtableRejection(error: unknown): boolean {
  return !!error && typeof error === "object" && "code" in error && [
    "invalid_argument", "revision_conflict", "forbidden", "not_found",
    "capacity_limited", "insufficient_budget", "capability_unqualified",
    "policy_unenforceable", "context_too_large", "capacity_unknown",
    "cannot_reach_quorum", "no_next_phase", "idempotency_conflict",
  ].includes(String(error.code))
}

// In-memory, per-browser-document retention. Client-side room navigation keeps
// this registry; a full reload or tab close is an explicit recovery boundary.
// Do not imply server-global exactly-once or silently persist private input.
const unresolvedPaidByRoom = new Map<string, PendingRoundtableMutation>()

export function pendingPaidMutation(scope: string): PendingRoundtableMutation | null {
  return unresolvedPaidByRoom.get(scope) ?? null
}

export function rememberPendingPaidMutation(scope: string, pending: PendingRoundtableMutation): void {
  if (!pending.paid) return
  const existing = unresolvedPaidByRoom.get(scope)
  if (existing && existing.request.request_id !== pending.request.request_id) {
    throw new Error("paid_outcome_unknown")
  }
  // A request in flight is also unresolved if its component is navigated away.
  pending.uncertain = true
  unresolvedPaidByRoom.set(scope, pending)
}

export function clearPendingPaidMutation(scope: string, requestId: unknown): void {
  if (unresolvedPaidByRoom.get(scope)?.request.request_id === requestId) {
    unresolvedPaidByRoom.delete(scope)
  }
}
