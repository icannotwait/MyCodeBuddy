"use client"

/**
 * Resolves a unified "delegation card model" — agent type, task, status,
 * child ids, title, and runtime projection — from a `delegate_to_agent`
 * tool call, in priority order:
 *   live `DelegationContext` binding → persisted `meta["codeg.delegation"]`
 *   → child projection cache → parsed tool input/output.
 *
 * Lifecycle (`running` | `ok` | `err`) is separate from badge status
 * (`active` | `stalled` | `waiting_input` | …). Ticker eligibility uses
 * lifecycle only.
 *
 * Pure merge lives in `buildDelegationCardModel`; the hook adds React-state
 * reads: live binding, child permission, projection cache interest, and the
 * shared running ticker.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react"

import {
  ALL_AGENT_TYPES,
  type AgentType,
  type AttentionRequestSummary,
  type CardSummary,
  type DelegationRunSnapshot,
  type DelegationRuntimeStats,
} from "@/lib/types"
import type { ToolCallState } from "@/lib/adapters/ai-elements-adapter"
import {
  useConnectionStore,
  type ConnectionState,
} from "@/contexts/acp-connections-context"
import { useDelegatedSubSession } from "@/hooks/use-delegated-sub-session"
import {
  buildEditRollupViewModel,
  computeDelegationElapsedMs,
  formatDelegationDisplaySecondary,
  isUncorrelatedDelegationFailure,
  parseDelegateTaskId,
  parseDelegationMeta,
  parseInput,
  parseToolOutput,
  resolveDelegationStatus,
  type DelegationCardStatus,
  type EditRollupViewModel,
  type ParsedMeta,
  type ParsedToolOutput,
} from "@/lib/delegation-card"
import type { DelegationBinding } from "@/lib/delegation-binding-reduce"
import {
  delegationChildProjectionCache,
  type ChildCardProjection,
} from "@/lib/delegation-child-projection-cache"
import { delegationRunSnapshotCache } from "@/lib/delegation-run-snapshot"
import {
  getRunningTickerVersion,
  retainRunningTicker,
  subscribeRunningTicker,
} from "@/lib/delegation-running-ticker"
import {
  isLatestStickyCard,
  resolveStickyIdentity,
  stickyIdentityToString,
  type RecoverySignals,
  type StickyBucket,
  type StickyObservation,
} from "@/lib/delegation-sticky-runtime"
import {
  getStickySnapshot,
  observeSticky,
  subscribeSticky,
} from "@/lib/delegation-sticky-store"
import { getActiveBackendCacheKey } from "@/lib/transport"

const RUNNING_SNAPSHOT_REFRESH_MS = 15_000

/** The raw inputs a `delegate_to_agent` tool call carries — the props
 *  `DelegatedSubThread` already receives, and the shape `SubAgentOverlay`
 *  extracts from the last assistant turn's tool-call parts. */
export interface DelegationCardSource {
  parentToolUseId: string
  /** Required for the authorized durable run snapshot query. */
  parentConversationId?: number | null
  input?: string | null
  output?: string | null
  errorText?: string | null
  state?: ToolCallState
  meta?: Record<string, unknown> | null
}

export type DelegationLifecycleStatus = "running" | "ok" | "err"

export interface DelegationCardModel {
  agentType: AgentType | null
  agentDisplayLabel: string | null
  task: string | null
  /** Short display id (tool-output task_id when present, else broker id). */
  taskId: string | null
  /** Durable broker task id from binding / meta / child projection. */
  brokerTaskId: string | null
  /** Durable round number within the shared child conversation. */
  generation: number | null
  /** Badge status (may refine running into active/stalled/waiting_input/…). */
  status: DelegationCardStatus
  /** Lifecycle only — drives elapsed formula + ticker eligibility. */
  lifecycleStatus: DelegationLifecycleStatus
  errorCode: string | undefined
  childConversationId: number | null
  childConnectionId: string | null
  /** False when there's no live binding and the input parsed to neither an
   *  agent type nor a task (and no meta) — nothing useful to draw. */
  hasModel: boolean
  displaySecondary: string | null
  conversationTitle: string | null
  startedAt: string | null
  finishedAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  attentionRequest: AttentionRequestSummary | null
  completedDurationMs: number | null
  elapsedMs: number | null
  /** null when stats absent — never fabricate zero for missing stats. */
  toolCallCount: number | null
  editRollup: EditRollupViewModel
  /** Structured terminal summary, validated again on the client. */
  cardSummary: CardSummary | null
  /** A replacement belongs to another child session and remains a separate row. */
  isReplacement: boolean
  childTurnAnchor: string | null
  /**
   * True when sticky phase is `active_sticky` and this card is the latest for
   * the unit — drives continuous generating chrome (Task 5) without reactivating
   * historical sibling cards.
   */
  showGeneratingSegment: boolean
  /** Sticky store identity key when a bucket is projected; else null. */
  stickyKey: string | null
}

function parseTimestampMs(value: string | null | undefined): number | null {
  if (value == null || value === "") return null
  const ms = Date.parse(value)
  return Number.isFinite(ms) ? ms : null
}

/** Ticker retain only when lifecycle is running and startedAt is valid. */
export function isTickerEligible(
  model: Pick<DelegationCardModel, "lifecycleStatus" | "startedAt">
): boolean {
  return (
    model.lifecycleStatus === "running" &&
    parseTimestampMs(model.startedAt) != null
  )
}

function lifecycleFromProjection(
  projection: ChildCardProjection
): DelegationLifecycleStatus | null {
  switch (projection.taskStatus) {
    case "completed":
      return "ok"
    case "failed":
    case "canceled":
      return "err"
    case "running":
      return "running"
    default:
      return null
  }
}

function lifecycleFromRunSnapshot(
  snapshot: DelegationRunSnapshot
): DelegationLifecycleStatus {
  switch (snapshot.status) {
    case "completed":
      return "ok"
    case "failed":
    case "canceled":
      return "err"
    case "reserving":
    case "running":
      return "running"
  }
}

function cardStatusFromLifecycle(
  lifecycleStatus: DelegationLifecycleStatus
): DelegationCardStatus {
  switch (lifecycleStatus) {
    case "ok":
      return "ok"
    case "err":
      return "err"
    case "running":
      return "running"
  }
}

function agentTypeFromRunSnapshot(
  snapshot: DelegationRunSnapshot | null
): AgentType | null {
  if (!snapshot || !ALL_AGENT_TYPES.includes(snapshot.agent_type)) return null
  return snapshot.agent_type
}

/** Terminal badges cannot sit next to sticky generating chrome. */
function coerceStickyGeneratingBadge(
  status: DelegationCardStatus
): DelegationCardStatus {
  if (status === "ok" || status === "err" || status === "starting") {
    return "running"
  }
  return status
}

const CANCEL_LIKE_ERROR_CODES = new Set([
  "parent_canceled",
  "cancel_delegation",
  "canceled",
  "parent_ended",
  "parent_turn_failed",
  "join_abandoned",
  "parent_disconnected",
])

/**
 * Map already precedence-resolved lifecycle → sticky observation type.
 *
 * Must NOT re-read `runSnapshot` here: lifecycle is binding > meta > snapshot
 * (and tool/projection rules). Trusting a stale running snapshot while a higher
 * source is terminal would re-admit `active_sticky` and flip generating chrome.
 */
function resolveStickyObserveType(input: {
  lifecycleStatus: DelegationLifecycleStatus
  errorCode: string | null | undefined
}): StickyObservation["type"] {
  const { lifecycleStatus, errorCode } = input
  if (lifecycleStatus === "running") return "running"
  if (lifecycleStatus === "ok") return "completed"
  const code = errorCode?.trim() ?? ""
  if (CANCEL_LIKE_ERROR_CODES.has(code)) return "canceled"
  return "failed"
}

/**
 * Resolve lifecycle from highest-priority source. Lower sources cannot
 * reopen a terminal higher source; a higher running source is not overridden
 * by a terminal lower source either.
 *
 * After binding/meta: **terminal** child projection wins over tool state, but
 * a non-terminal (running) projection must not block a terminal tool outcome
 * (anti-stale summary vs completed parent output).
 */
function resolveLifecycleStatus(input: {
  binding: DelegationBinding | undefined
  parsedMeta: ParsedMeta | null
  runSnapshot: DelegationRunSnapshot | null
  childProjection: ChildCardProjection | null
  toolOutput: ParsedToolOutput | null
  state?: ToolCallState
  errorText?: string | null
}): DelegationLifecycleStatus {
  const {
    binding,
    parsedMeta,
    runSnapshot,
    childProjection,
    toolOutput,
    state,
    errorText,
  } = input

  if (binding) return binding.status
  if (parsedMeta) return parsedMeta.status
  if (runSnapshot) return lifecycleFromRunSnapshot(runSnapshot)

  const fromProj = childProjection
    ? lifecycleFromProjection(childProjection)
    : null
  // Terminal summary locks lifecycle even when parent still shows ack.
  if (fromProj === "ok" || fromProj === "err") return fromProj

  if (state === "output-error" || errorText) {
    if (toolOutput?.kind === "outcome") {
      return toolOutput.isError ? "err" : "ok"
    }
    return "err"
  }
  // Terminal tool outcome outranks a still-running lower summary.
  if (toolOutput?.kind === "outcome") {
    return toolOutput.isError ? "err" : "ok"
  }
  if (toolOutput?.kind === "ack") return "running"
  if (state === "output-available") return "ok"
  if (fromProj === "running") return "running"
  return "running"
}

/**
 * Child projections track the **latest** run on a shared session. When the
 * card is already bound to a known task_id, only apply projection lifecycle/
 * stats when projection.taskId **exactly matches** (fail closed). Null,
 * undefined, or mismatched task IDs are ignored so a later continue — or a
 * stale child row without a call id — cannot mutate an earlier terminal card.
 */
function runScopedChildProjection(
  childProjection: ChildCardProjection | null,
  knownTaskId: string | null
): ChildCardProjection | null {
  if (!childProjection) return null
  if (!knownTaskId) return childProjection
  if (
    childProjection.taskId == null ||
    childProjection.taskId !== knownTaskId
  ) {
    return null
  }
  return childProjection
}

/**
 * Pick runtime stats with anti-stale rules:
 * - Prefer higher-priority source when it has stats.
 * - Terminal higher source without stats may fill from a **terminal** lower
 *   source only (never from a still-running lower summary).
 * - Running higher source without stats may fill from a non-terminal lower
 *   source only (never adopt terminal lower stats that would conflict with
 *   a still-running higher lifecycle for stats display — lower terminal
 *   is ignored when higher is running).
 * - The child projection must match this card's task_id when known.
 */
function pickRuntimeStats(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null,
  childProjection: ChildCardProjection | null
): DelegationRuntimeStats | null {
  if (binding) return binding.runtimeStats

  if (parsedMeta) {
    if (parsedMeta.runtimeStats != null) return parsedMeta.runtimeStats
    if (runSnapshot?.runtime_stats != null) return runSnapshot.runtime_stats
    if (!childProjection?.runtimeStats) return null
    const metaTerminal = parsedMeta.status !== "running"
    if (metaTerminal) {
      // Terminal meta + running summary → do not adopt running stats.
      return childProjection.isTerminal ? childProjection.runtimeStats : null
    }
    // Meta still running: only take non-terminal lower stats.
    return childProjection.isTerminal ? null : childProjection.runtimeStats
  }

  if (runSnapshot) return runSnapshot.runtime_stats

  return childProjection?.runtimeStats ?? null
}

/**
 * Attention: higher source wins. Explicit `null` from live meta is an
 * authoritative clear. Synthetic history may recover attention only from a
 * child projection whose request task id matches this exact run.
 */
function pickAttentionRequest(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null,
  childProjection: ChildCardProjection | null
): AttentionRequestSummary | null {
  const matchingProjectionAttention = (
    expectedTaskId: string | null
  ): AttentionRequestSummary | null => {
    const attention = childProjection?.attentionRequest ?? null
    if (!expectedTaskId || attention?.task_id !== expectedTaskId) return null
    return attention
  }

  if (binding) {
    // Binding present → its attention (including null clear). Undefined is
    // treated as null (started events always write attentionRequest).
    return binding.attentionRequest ?? null
  }
  if (parsedMeta) {
    if (parsedMeta.syntheticHistorical) {
      if (parsedMeta.attentionRequest) {
        return parsedMeta.attentionRequest.task_id === parsedMeta.taskId
          ? parsedMeta.attentionRequest
          : null
      }
      return matchingProjectionAttention(parsedMeta.taskId)
    }
    // ParsedMeta always includes attentionRequest (null when absent/invalid).
    return parsedMeta.attentionRequest
  }
  if (runSnapshot) {
    return matchingProjectionAttention(runSnapshot.task_id)
  }
  return childProjection?.attentionRequest ?? null
}

function pickStartedAt(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null,
  childProjection: ChildCardProjection | null,
  runtimeStats: DelegationRuntimeStats | null
): string | null {
  if (binding) return binding.startedAt || null
  if (parsedMeta?.startedAt) return parsedMeta.startedAt
  if (runSnapshot?.started_at) return runSnapshot.started_at
  if (childProjection?.startedAt) return childProjection.startedAt
  return runtimeStats?.started_at ?? null
}

function pickFinishedAt(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null,
  childProjection: ChildCardProjection | null,
  runtimeStats: DelegationRuntimeStats | null,
  lifecycleStatus: DelegationLifecycleStatus
): string | null {
  // Running lifecycle never surfaces a finished timestamp from lower sources.
  if (lifecycleStatus === "running") {
    if (binding) return binding.finishedAt ?? null
    if (parsedMeta) return parsedMeta.finishedAt
    return null
  }

  if (binding) {
    return binding.finishedAt ?? binding.runtimeStats.finished_at ?? null
  }
  if (parsedMeta) {
    if (parsedMeta.finishedAt) return parsedMeta.finishedAt
    if (parsedMeta.runtimeStats?.finished_at) {
      return parsedMeta.runtimeStats.finished_at
    }
    if (runSnapshot) {
      return (
        runSnapshot.finished_at ??
        runSnapshot.runtime_stats?.finished_at ??
        null
      )
    }
    // Terminal meta may fill finishedAt from a terminal lower projection.
    if (childProjection?.isTerminal) {
      return (
        childProjection.finishedAt ??
        childProjection.runtimeStats?.finished_at ??
        null
      )
    }
    return null
  }
  if (runSnapshot) {
    return (
      runSnapshot.finished_at ?? runSnapshot.runtime_stats?.finished_at ?? null
    )
  }
  return childProjection?.finishedAt ?? runtimeStats?.finished_at ?? null
}

function pickCompletedDurationMs(
  binding: DelegationBinding | undefined,
  toolOutput: ParsedToolOutput | null
): number | null {
  if (
    binding &&
    typeof binding.completedDurationMs === "number" &&
    Number.isFinite(binding.completedDurationMs) &&
    binding.completedDurationMs >= 0
  ) {
    return binding.completedDurationMs
  }
  if (
    toolOutput?.kind === "outcome" &&
    typeof toolOutput.durationMs === "number" &&
    Number.isFinite(toolOutput.durationMs) &&
    toolOutput.durationMs >= 0
  ) {
    return toolOutput.durationMs
  }
  return null
}

function effectiveDelegationMeta(
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null
): ParsedMeta | null {
  return parsedMeta?.syntheticHistorical && runSnapshot ? null : parsedMeta
}

/**
 * Pure field-level merge for a delegation card. See plan locked contracts:
 * live binding > live ToolUse meta > immutable run snapshot > synthetic
 * historical meta > child projection;
 * attention null clears;
 * lifecycle terminal locks; duration from completion then tool output.
 */
export function buildDelegationCardModel(input: {
  parsedInput: ReturnType<typeof parseInput>
  parsedMeta: ParsedMeta | null
  toolOutput: ParsedToolOutput | null
  binding: DelegationBinding | undefined
  runSnapshot?: DelegationRunSnapshot | null
  childProjection: ChildCardProjection | null
  childAwaitingPermission: boolean
  state?: ToolCallState
  errorText?: string | null
  nowMs: number
  /** Optional short display id already extracted from tool output. */
  displayTaskId?: string | null
  /**
   * Sticky bucket snapshot (read-only). Pure merge only — never mutates the
   * sticky store. Latest-only generating projection + peak tool fold.
   */
  stickyBucket?: StickyBucket | null
  /** Parent tool-use id for latest-only card identity compare. */
  parentToolUseId?: string | null
}): DelegationCardModel {
  const {
    parsedInput,
    parsedMeta,
    toolOutput,
    binding,
    runSnapshot = null,
    childProjection,
    childAwaitingPermission,
    state,
    errorText,
    nowMs,
    displayTaskId = null,
    stickyBucket = null,
    parentToolUseId = null,
  } = input

  // Cold reconstruction injects correlation metadata before the immutable run
  // DTO is fetched. Once that DTO arrives it is the fresher durable lifecycle
  // source; live broker metadata remains higher priority as before.
  const effectiveMeta = effectiveDelegationMeta(parsedMeta, runSnapshot)

  // Known before projection merge so a later run on the same child cannot
  // overwrite this card's lifecycle/stats while the snapshot is still cold.
  const knownTaskId =
    binding?.taskId ??
    effectiveMeta?.taskId ??
    runSnapshot?.task_id ??
    displayTaskId ??
    null
  const uncorrelatedFailure = isUncorrelatedDelegationFailure(
    toolOutput,
    knownTaskId
  )
  const scopedChildProjection = uncorrelatedFailure ? null : childProjection
  const runScopedProjection = runScopedChildProjection(
    scopedChildProjection,
    knownTaskId
  )

  const lifecycleStatus = resolveLifecycleStatus({
    binding,
    parsedMeta: effectiveMeta,
    runSnapshot,
    childProjection: runScopedProjection,
    toolOutput,
    state,
    errorText,
  })

  const status =
    !binding && !effectiveMeta && runSnapshot
      ? cardStatusFromLifecycle(lifecycleStatus)
      : resolveDelegationStatus({
          binding,
          parsedMeta: effectiveMeta,
          toolOutput,
          state,
          errorText,
          childAwaitingPermission:
            !uncorrelatedFailure && childAwaitingPermission,
          childTaskStatus: runScopedProjection?.taskStatus ?? null,
        })

  const runtimeStats = pickRuntimeStats(
    binding,
    effectiveMeta,
    runSnapshot,
    runScopedProjection
  )
  const attentionRequest = pickAttentionRequest(
    binding,
    effectiveMeta,
    runSnapshot,
    runScopedProjection
  )
  const startedAt = pickStartedAt(
    binding,
    effectiveMeta,
    runSnapshot,
    runScopedProjection,
    runtimeStats
  )
  const finishedAt = pickFinishedAt(
    binding,
    effectiveMeta,
    runSnapshot,
    runScopedProjection,
    runtimeStats,
    lifecycleStatus
  )
  const completedDurationMs = pickCompletedDurationMs(binding, toolOutput)

  const brokerTaskId =
    binding?.taskId ??
    effectiveMeta?.taskId ??
    runSnapshot?.task_id ??
    runScopedProjection?.taskId ??
    null

  const childConnectionId = uncorrelatedFailure
    ? null
    : (binding?.childConnectionId ?? effectiveMeta?.childConnectionId ?? null)
  // Child conversation identity is shared across runs; title/id may come from
  // the latest projection even when run-scoped fields are suppressed.
  const childConversationId = uncorrelatedFailure
    ? null
    : (binding?.childConversationId ??
      effectiveMeta?.childConversationId ??
      runSnapshot?.child_conversation_id ??
      toolOutput?.childConversationId ??
      scopedChildProjection?.childConversationId ??
      null)

  const agentType: AgentType | null =
    binding?.agentType ??
    parsedInput.agentType ??
    agentTypeFromRunSnapshot(runSnapshot)
  // Cold recovery may only have summary error_code — fold projection last.
  // Correlation failures never mint a run snapshot; surface the wire code from
  // the parent tool outcome so the badge is not mislabeled as spawn/unresumable.
  const toolErrorCode =
    toolOutput?.kind === "outcome" ? toolOutput.errorCode : null
  const errorCode =
    binding?.errorCode ??
    effectiveMeta?.errorCode ??
    runSnapshot?.error_code ??
    runScopedProjection?.errorCode ??
    toolErrorCode ??
    undefined

  const conversationTitle = scopedChildProjection?.title ?? null
  const task =
    parsedInput.task ??
    binding?.task ??
    effectiveMeta?.task ??
    runSnapshot?.task_preview ??
    null
  const displaySecondary = formatDelegationDisplaySecondary(
    conversationTitle,
    task
  )

  const elapsedMs = computeDelegationElapsedMs({
    lifecycleStatus,
    startedAt,
    finishedAt,
    completedDurationMs,
    nowMs,
  })

  const toolCallCount =
    runtimeStats != null ? runtimeStats.tool_call_count : null
  const editRollup = buildEditRollupViewModel(runtimeStats)

  const hasModel = Boolean(
    binding ||
    parsedInput.agentType ||
    parsedInput.task ||
    parsedMeta ||
    runSnapshot
  )

  const generation = runSnapshot?.generation ?? parsedMeta?.generation ?? null

  // Latest-only sticky projection (pure — no store mutation).
  const cardIdentity = {
    taskId: brokerTaskId,
    parentToolUseId,
    generation,
  }
  const isLatest = stickyBucket
    ? isLatestStickyCard(cardIdentity, stickyBucket)
    : false
  const showGeneratingSegment =
    stickyBucket?.phase === "active_sticky" && isLatest
  const stickyKey = stickyBucket?.identityKey ?? null

  let projectedLifecycle = lifecycleStatus
  let projectedStatus = status
  let projectedStartedAt = startedAt
  let projectedFinishedAt = finishedAt
  let projectedElapsedMs = elapsedMs
  let projectedToolCallCount = toolCallCount

  if (showGeneratingSegment && stickyBucket) {
    projectedLifecycle = "running"
    projectedStatus = coerceStickyGeneratingBadge(status)
    // Anchor drives continuous elapsed + ticker eligibility.
    if (stickyBucket.anchorStartedAtMs != null) {
      projectedStartedAt = new Date(
        stickyBucket.anchorStartedAtMs
      ).toISOString()
      projectedFinishedAt = null
      const elapsed = nowMs - stickyBucket.anchorStartedAtMs
      projectedElapsedMs = elapsed >= 0 ? elapsed : null
    } else {
      projectedFinishedAt = null
      projectedElapsedMs = computeDelegationElapsedMs({
        lifecycleStatus: "running",
        startedAt: projectedStartedAt,
        finishedAt: null,
        completedDurationMs: null,
        nowMs,
      })
    }
    // Peak-sum tools when any task peaks were observed; never invent zeros.
    if (stickyBucket.peakByTaskId.size > 0) {
      projectedToolCallCount = stickyBucket.lastDisplayToolCount
    }
  }

  return {
    agentType,
    agentDisplayLabel: parsedInput.profileLabel,
    task,
    taskId: displayTaskId ?? brokerTaskId,
    brokerTaskId,
    generation,
    status: projectedStatus,
    lifecycleStatus: projectedLifecycle,
    errorCode,
    childConversationId,
    childConnectionId,
    hasModel,
    displaySecondary,
    conversationTitle,
    startedAt: projectedStartedAt,
    finishedAt: projectedFinishedAt,
    runtimeStats,
    attentionRequest,
    completedDurationMs,
    elapsedMs: projectedElapsedMs,
    toolCallCount: projectedToolCallCount,
    editRollup,
    cardSummary: binding?.cardSummary ?? runSnapshot?.card_summary ?? null,
    isReplacement: Boolean(runSnapshot?.replaced_task_id),
    childTurnAnchor: runSnapshot?.child_turn_anchor ?? null,
    showGeneratingSegment,
    stickyKey,
  }
}

/**
 * Subscribe to the child connection's `ConnectionState` (live message,
 * pending permission, etc.) from the shared connections store. Returns
 * `undefined` while no synthetic entry exists yet. Re-renders on every state
 * change via `useSyncExternalStore`.
 */
function useDelegationChildLive(
  childConnectionId: string | null
): ConnectionState | undefined {
  const store = useConnectionStore()
  const subscribe = useCallback(
    (cb: () => void) => {
      if (!childConnectionId) return () => {}
      return store.subscribeKey(childConnectionId, cb)
    },
    [store, childConnectionId]
  )
  const getSnapshot = useCallback(
    () =>
      childConnectionId ? store.getConnection(childConnectionId) : undefined,
    [store, childConnectionId]
  )
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

function useChildCardProjection(
  childConversationId: number | null
): ChildCardProjection | null {
  useEffect(() => {
    if (childConversationId == null) return
    const release = delegationChildProjectionCache.retain(childConversationId)
    delegationChildProjectionCache.ensure(childConversationId)
    return release
  }, [childConversationId])

  const subscribe = useCallback(
    (cb: () => void) => delegationChildProjectionCache.subscribe(cb),
    []
  )
  const getSnapshot = useCallback(() => {
    if (childConversationId == null) return null
    return delegationChildProjectionCache.get(childConversationId)
  }, [childConversationId])

  return useSyncExternalStore(subscribe, getSnapshot, () => null)
}

function useDelegationRunSnapshot(
  parentConversationId: number | null | undefined,
  taskId: string | null
): DelegationRunSnapshot | null {
  const subscribe = useCallback(
    (cb: () => void) => delegationRunSnapshotCache.subscribe(cb),
    []
  )
  const getSnapshot = useCallback(
    () => delegationRunSnapshotCache.get(parentConversationId, taskId),
    [parentConversationId, taskId]
  )
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, () => null)

  useEffect(() => {
    if (parentConversationId == null || !taskId) return
    const refresh = () =>
      delegationRunSnapshotCache.ensure(parentConversationId, taskId)
    refresh()
    const terminal =
      snapshot?.status === "completed" ||
      snapshot?.status === "failed" ||
      snapshot?.status === "canceled"
    if (terminal) return
    const timer = window.setInterval(refresh, RUNNING_SNAPSHOT_REFRESH_MS)
    return () => window.clearInterval(timer)
  }, [parentConversationId, taskId, snapshot?.status])

  return snapshot
}

function parseWorkUnitKeyFromInput(
  raw: string | null | undefined
): string | null {
  if (!raw || typeof raw !== "string") return null
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return null
    }
    const obj = parsed as Record<string, unknown>
    const key = obj.work_unit_key ?? obj.workUnitKey
    return typeof key === "string" && key.length > 0 ? key : null
  } catch {
    return null
  }
}

export function useDelegationCardModel(
  source: DelegationCardSource
): DelegationCardModel {
  const {
    parentToolUseId,
    parentConversationId,
    input,
    output,
    errorText,
    state,
    meta,
  } = source

  const parsedInput = useMemo(() => parseInput(input), [input])
  const parsedMeta = useMemo(() => parseDelegationMeta(meta), [meta])
  const displayTaskId = useMemo(
    () => parseDelegateTaskId(output, errorText),
    [output, errorText]
  )
  const workUnitKey = useMemo(() => parseWorkUnitKeyFromInput(input), [input])

  // `enabled: false` — the model never fetches the child's persisted detail
  // here; cold title/stats come from `delegationChildProjectionCache`.
  const { binding } = useDelegatedSubSession(parentToolUseId, {
    enabled: false,
  })

  const snapshotTaskId = binding?.taskId ?? parsedMeta?.taskId ?? displayTaskId
  const runSnapshot = useDelegationRunSnapshot(
    parentConversationId,
    snapshotTaskId
  )

  const toolOutput = useMemo<ParsedToolOutput | null>(() => {
    if (errorText) {
      const parsedErr = parseToolOutput(errorText, true)
      if (parsedErr) return parsedErr
    }
    return parseToolOutput(output)
  }, [output, errorText])

  const currentTaskId =
    binding?.taskId ??
    parsedMeta?.taskId ??
    runSnapshot?.task_id ??
    displayTaskId
  const uncorrelatedFailure = isUncorrelatedDelegationFailure(
    toolOutput,
    currentTaskId
  )

  const childConversationId = uncorrelatedFailure
    ? null
    : (binding?.childConversationId ??
      parsedMeta?.childConversationId ??
      runSnapshot?.child_conversation_id ??
      toolOutput?.childConversationId ??
      null)

  const childConnectionId = uncorrelatedFailure
    ? null
    : (binding?.childConnectionId ?? parsedMeta?.childConnectionId ?? null)

  const childProjection = useChildCardProjection(childConversationId)
  const childLive = useDelegationChildLive(childConnectionId)
  const childAwaitingPermission = childLive?.pendingPermission != null

  // Sticky identity (subscribe key) — pure resolve, no mutation.
  const backendCacheKey = getActiveBackendCacheKey()
  const stickyIdentityKey = useMemo(() => {
    const resolved = resolveStickyIdentity({
      backendCacheKey,
      parentConversationId,
      childConversationId,
      workUnitKey,
      taskId: currentTaskId,
    })
    return resolved ? stickyIdentityToString(resolved) : null
  }, [
    backendCacheKey,
    parentConversationId,
    childConversationId,
    workUnitKey,
    currentTaskId,
  ])

  const subscribeStickyStore = useCallback(
    (cb: () => void) => subscribeSticky(cb),
    []
  )
  const getStickyBucketSnapshot = useCallback((): StickyBucket | null => {
    if (!stickyIdentityKey) return null
    return getStickySnapshot(stickyIdentityKey) ?? null
  }, [stickyIdentityKey])
  const stickyBucket = useSyncExternalStore(
    subscribeStickyStore,
    getStickyBucketSnapshot,
    () => null
  )

  // Eligibility without building the full model (avoids ticker chicken-egg).
  const knownTaskId = currentTaskId
  const tickerMeta = effectiveDelegationMeta(parsedMeta, runSnapshot)
  const runScopedProjection = runScopedChildProjection(
    childProjection,
    knownTaskId
  )
  const lifecyclePreview = resolveLifecycleStatus({
    binding,
    parsedMeta: tickerMeta,
    runSnapshot,
    childProjection: runScopedProjection,
    toolOutput,
    state,
    errorText,
  })
  const runtimeStatsPreview = pickRuntimeStats(
    binding,
    tickerMeta,
    runSnapshot,
    runScopedProjection
  )
  const startedAtPreview = pickStartedAt(
    binding,
    tickerMeta,
    runSnapshot,
    runScopedProjection,
    runtimeStatsPreview
  )
  const generationPreview =
    runSnapshot?.generation ?? tickerMeta?.generation ?? null
  const stickyGeneratingPreview =
    stickyBucket?.phase === "active_sticky" &&
    isLatestStickyCard(
      {
        taskId: currentTaskId,
        parentToolUseId,
        generation: generationPreview,
      },
      stickyBucket
    )
  const stickyAnchorStartedAt =
    stickyGeneratingPreview && stickyBucket?.anchorStartedAtMs != null
      ? new Date(stickyBucket.anchorStartedAtMs).toISOString()
      : null
  const tickerStartedAt = stickyAnchorStartedAt ?? startedAtPreview
  const tickerEligible =
    (lifecyclePreview === "running" || stickyGeneratingPreview) &&
    parseTimestampMs(tickerStartedAt) != null

  const observeErrorCode =
    binding?.errorCode ??
    tickerMeta?.errorCode ??
    runSnapshot?.error_code ??
    runScopedProjection?.errorCode ??
    (toolOutput?.kind === "outcome" ? toolOutput.errorCode : null) ??
    null
  const observeFinishedAt =
    binding?.finishedAt ??
    tickerMeta?.finishedAt ??
    runSnapshot?.finished_at ??
    runScopedProjection?.finishedAt ??
    null
  const observeToolCallCount = runtimeStatsPreview?.tool_call_count ?? null
  // Same null-clear precedence as pure model (binding/meta explicit null wins).
  const openAttention =
    pickAttentionRequest(
      binding,
      tickerMeta,
      runSnapshot,
      runScopedProjection
    ) != null
  const liveBindingRunning = binding?.status === "running"
  const childProjectionRunning = runScopedProjection?.taskStatus === "running"
  const activeRunNonTerminal =
    runSnapshot?.status === "running" || runSnapshot?.status === "reserving"
  const continueOrReplaceAdmitted =
    (generationPreview != null && generationPreview > 1) ||
    Boolean(runSnapshot?.replaced_task_id)
  const observeType = resolveStickyObserveType({
    lifecycleStatus: lifecyclePreview,
    errorCode: observeErrorCode,
  })

  // Dedupe identical observations so store emit → re-render cannot loop when
  // parents pass fresh meta object identities each render.
  const lastObserveSigRef = useRef<string | null>(null)

  // Observe sticky only from effects — never from pure build.
  useEffect(() => {
    if (!currentTaskId || uncorrelatedFailure) return

    const recovery: RecoverySignals = {
      liveBindingRunning,
      childProjectionRunning,
      activeRunNonTerminal,
      openAttention,
      // Wait projection is conversation-scoped; treat as weak positive only
      // when this card has a known child in the unit (not global-wait alone).
      parentWaitingForThisChild: false,
      continueOrReplaceAdmitted,
    }

    const sig = [
      backendCacheKey,
      parentConversationId ?? "",
      childConversationId ?? "",
      workUnitKey ?? "",
      observeType,
      currentTaskId,
      generationPreview ?? "",
      parentToolUseId,
      startedAtPreview ?? "",
      observeFinishedAt ?? "",
      observeToolCallCount ?? "",
      observeErrorCode ?? "",
      liveBindingRunning,
      childProjectionRunning,
      activeRunNonTerminal,
      openAttention,
      continueOrReplaceAdmitted,
    ].join("\0")
    if (lastObserveSigRef.current === sig) return
    lastObserveSigRef.current = sig

    observeSticky({
      backendCacheKey,
      parentConversationId,
      childConversationId,
      workUnitKey,
      type: observeType,
      taskId: currentTaskId,
      nowMs: Date.now(),
      generation: generationPreview,
      parentToolUseId,
      startedAt: startedAtPreview,
      finishedAt: observeFinishedAt,
      toolCallCount: observeToolCallCount,
      errorCode: observeErrorCode,
      recovery,
    })
  }, [
    backendCacheKey,
    parentConversationId,
    childConversationId,
    workUnitKey,
    currentTaskId,
    uncorrelatedFailure,
    observeType,
    generationPreview,
    parentToolUseId,
    startedAtPreview,
    observeFinishedAt,
    observeToolCallCount,
    observeErrorCode,
    liveBindingRunning,
    childProjectionRunning,
    activeRunNonTerminal,
    openAttention,
    continueOrReplaceAdmitted,
  ])

  useEffect(() => {
    if (!tickerEligible) return
    return retainRunningTicker()
  }, [tickerEligible])

  const subscribeTicker = useCallback(
    (cb: () => void) => {
      if (!tickerEligible) return () => {}
      return subscribeRunningTicker(cb)
    },
    [tickerEligible]
  )
  const tickerVersion = useSyncExternalStore(
    subscribeTicker,
    getRunningTickerVersion,
    () => 0
  )

  return useMemo(
    () =>
      buildDelegationCardModel({
        parsedInput,
        parsedMeta,
        toolOutput,
        binding,
        runSnapshot,
        childProjection,
        childAwaitingPermission,
        state,
        errorText,
        nowMs: Date.now(),
        displayTaskId,
        stickyBucket,
        parentToolUseId,
      }),
    // tickerVersion forces elapsed recompute while running.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- tickerVersion is intentional
    [
      parsedInput,
      parsedMeta,
      toolOutput,
      binding,
      runSnapshot,
      childProjection,
      childAwaitingPermission,
      state,
      errorText,
      displayTaskId,
      stickyBucket,
      parentToolUseId,
      tickerVersion,
    ]
  )
}
