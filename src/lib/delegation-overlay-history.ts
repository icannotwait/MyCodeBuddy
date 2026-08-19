import {
  parseDelegateRunIdentity,
  type DelegationRunIdentity,
} from "@/lib/delegation-card"
import type { DelegationCardSource } from "@/hooks/use-delegation-card-model"
import type { DbConversationSummary } from "@/lib/types"

type OverlayTaskStatus = "running" | "completed" | "failed" | "canceled"

function overlayTaskStatus(child: DbConversationSummary): OverlayTaskStatus {
  switch (child.delegation_task_status) {
    case "running":
    case "completed":
    case "failed":
    case "canceled":
      return child.delegation_task_status
    case "pending":
      return "running"
    case "cancelled":
      return "canceled"
    default:
      break
  }
  switch (child.status) {
    case "in_progress":
      return "running"
    case "pending_review":
    case "completed":
      return "completed"
    case "cancelled":
    case "canceled":
      return "canceled"
    default:
      return "running"
  }
}

function metaStatus(
  status: OverlayTaskStatus
): "running" | "completed" | "failed" {
  if (status === "running") return "running"
  if (status === "completed") return "completed"
  return "failed"
}

function sourceState(
  status: OverlayTaskStatus
): NonNullable<DelegationCardSource["state"]> {
  if (status === "running") return "input-available"
  if (status === "completed") return "output-available"
  return "output-error"
}

function launchTimeMs(child: DbConversationSummary): number {
  const raw = child.delegation_started_at ?? child.created_at
  const ms = Date.parse(raw)
  return Number.isFinite(ms) ? ms : child.id
}

export function sortChildConversationsForOverlay(
  children: readonly DbConversationSummary[]
): DbConversationSummary[] {
  return children.slice().sort((left, right) => {
    const delta = launchTimeMs(left) - launchTimeMs(right)
    if (delta !== 0) return delta
    return left.id - right.id
  })
}

export function childConversationToDelegationSource(
  parentConversationId: number,
  child: DbConversationSummary
): DelegationCardSource {
  const status = overlayTaskStatus(child)
  const parentToolUseId =
    child.parent_tool_use_id?.trim() || `child-${child.id}`
  const rootTaskId = child.delegation_call_id?.trim() || null
  const title = child.title?.trim() || null
  const errorCode = child.delegation_error_code?.trim() || null

  return {
    parentToolUseId,
    parentConversationId,
    input: JSON.stringify({
      agent_type: child.agent_type,
      task: title ?? "",
    }),
    output: JSON.stringify({
      status:
        status === "failed" || status === "canceled" ? "completed" : status,
      child_conversation_id: child.id,
      root_task_id: rootTaskId,
      text: status === "running" ? undefined : "",
      message:
        status === "failed" || status === "canceled"
          ? errorCode || "Delegation failed."
          : undefined,
      error_code: errorCode,
    }),
    errorText: null,
    state: sourceState(status),
    meta: {
      "codeg.delegation": {
        status: metaStatus(status),
        child_conversation_id: child.id,
        root_task_id: rootTaskId,
        task_preview: title,
        error_code: errorCode,
        started_at: child.delegation_started_at ?? child.created_at,
        finished_at: child.delegation_finished_at ?? null,
        runtime_stats: child.delegation_runtime_stats ?? null,
        synthetic_historical: true,
      },
    },
  }
}

function readNonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null
}

function readRootTaskId(source: DelegationCardSource): string | null {
  const fromJson = (raw: string | null | undefined): string | null => {
    if (!raw) return null
    try {
      const parsed: unknown = JSON.parse(raw)
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return null
      }
      return readNonEmptyString(
        (parsed as Record<string, unknown>)["root_task_id"]
      )
    } catch {
      return null
    }
  }

  const fromOutput = fromJson(source.output)
  if (fromOutput) return fromOutput
  const meta = source.meta?.["codeg.delegation"]
  if (meta && typeof meta === "object" && !Array.isArray(meta)) {
    return readNonEmptyString((meta as Record<string, unknown>)["root_task_id"])
  }
  return null
}

function sourceIdentity(source: DelegationCardSource): DelegationRunIdentity {
  const identity = parseDelegateRunIdentity({
    parentConversationId: source.parentConversationId ?? 0,
    parentToolUseId: source.parentToolUseId,
    input: source.input,
    output: source.output,
    errorText: source.errorText,
    meta: source.meta,
  })
  const rootTaskId = readRootTaskId(source)
  if (
    rootTaskId &&
    rootTaskId !== identity.taskId &&
    !identity.linkedTaskIds.includes(rootTaskId)
  ) {
    return {
      ...identity,
      linkedTaskIds: [...identity.linkedTaskIds, rootTaskId],
    }
  }
  return identity
}

function taskIdsOf(identity: DelegationRunIdentity): Set<string> {
  const ids = new Set<string>()
  if (identity.taskId) ids.add(identity.taskId)
  for (const taskId of identity.linkedTaskIds) ids.add(taskId)
  return ids
}

function findExactBaseIndex(
  bases: readonly DelegationRunIdentity[],
  usedBase: ReadonlySet<number>,
  overlay: DelegationRunIdentity
): number | undefined {
  for (let index = 0; index < bases.length; index++) {
    if (usedBase.has(index)) continue
    const base = bases[index]
    if (base.parentToolUseId === overlay.parentToolUseId) return index
    if (overlay.taskId && overlay.taskId === base.taskId) return index
  }
  return undefined
}

function findLooseBaseIndex(
  bases: readonly DelegationRunIdentity[],
  usedBase: ReadonlySet<number>,
  overlay: DelegationRunIdentity
): number | undefined {
  const overlayTasks = taskIdsOf(overlay)
  let found: number | undefined
  for (let index = 0; index < bases.length; index++) {
    if (usedBase.has(index)) continue
    const base = bases[index]
    const childMatch =
      overlay.childConversationId != null &&
      overlay.childConversationId === base.childConversationId
    let taskMatch = false
    if (overlayTasks.size > 0) {
      for (const taskId of taskIdsOf(base)) {
        if (overlayTasks.has(taskId)) {
          taskMatch = true
          break
        }
      }
    }
    if (childMatch || taskMatch) found = index
  }
  return found
}

/**
 * Overlay wins on shared identity. Exact tool / current-task matches first.
 * Child or linked-task correlation replaces the latest unused base row so a
 * live continuation does not clobber an older historical run. Unmatched
 * overlay rows append.
 */
export function mergeDelegationSourceLayers(
  base: readonly DelegationCardSource[],
  overlay: readonly DelegationCardSource[]
): DelegationCardSource[] {
  if (overlay.length === 0) return base.slice()
  if (base.length === 0) return overlay.slice()

  const baseIdentities = base.map(sourceIdentity)
  const overlayIdentities = overlay.map(sourceIdentity)
  const usedBase = new Set<number>()
  const usedOverlay = new Set<number>()
  const replaced = new Map<number, DelegationCardSource>()

  overlay.forEach((source, overlayIndex) => {
    const identity = overlayIdentities[overlayIndex]
    const index =
      findExactBaseIndex(baseIdentities, usedBase, identity) ??
      findLooseBaseIndex(baseIdentities, usedBase, identity)
    if (index == null) return
    usedBase.add(index)
    usedOverlay.add(overlayIndex)
    replaced.set(index, source)
  })

  const merged: DelegationCardSource[] = []
  base.forEach((source, index) => {
    merged.push(replaced.get(index) ?? source)
  })
  overlay.forEach((source, index) => {
    if (!usedOverlay.has(index)) merged.push(source)
  })
  return merged
}
