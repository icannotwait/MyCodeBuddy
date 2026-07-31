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

import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react"

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
import {
  buildDelegationWorkUnitRuntime,
  type WorkUnitRunObservation,
} from "@/lib/delegation-work-unit-runtime"
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
  /** Active work units prepend the localized generating/streaming segment. */
  showGeneratingSegment: boolean
  /** Stable canonical work-unit identity shared by inline and overlay cards. */
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

/**
 * Live bindings are indexed by parent tool-use id, but `useDelegatedSubSession`
 * may also surface a later continue on the **same child conversation**. Multi-
 * turn cards must only adopt a binding that belongs to this exact turn so
 * card_summary / lifecycle / stats (Critical·Important·…) never bleed across
 * generations.
 */
export function scopeDelegationBindingForCard(
  binding: DelegationBinding | undefined,
  parentToolUseId: string,
  knownTaskId: string | null
): DelegationBinding | undefined {
  if (!binding) return undefined
  if (binding.parentToolUseId !== parentToolUseId) return undefined
  if (knownTaskId && binding.taskId !== knownTaskId) return undefined
  return binding
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

  return {
    agentType,
    agentDisplayLabel: parsedInput.profileLabel,
    task,
    taskId: displayTaskId ?? brokerTaskId,
    brokerTaskId,
    generation: runSnapshot?.generation ?? parsedMeta?.generation ?? null,
    status,
    lifecycleStatus,
    errorCode,
    childConversationId,
    childConnectionId,
    hasModel,
    displaySecondary,
    conversationTitle,
    startedAt,
    finishedAt,
    runtimeStats,
    attentionRequest,
    completedDurationMs,
    elapsedMs,
    toolCallCount,
    editRollup,
    cardSummary: binding?.cardSummary ?? runSnapshot?.card_summary ?? null,
    isReplacement: Boolean(runSnapshot?.replaced_task_id),
    childTurnAnchor: runSnapshot?.child_turn_anchor ?? null,
    showGeneratingSegment: false,
    stickyKey: null,
  }
}

function rawDelegationMeta(
  source: DelegationCardSource
): Record<string, unknown> | null {
  const value = source.meta?.["codeg.delegation"]
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function sourceToolOutput(
  source: DelegationCardSource
): ParsedToolOutput | null {
  if (source.errorText) {
    const parsedError = parseToolOutput(source.errorText, true)
    if (parsedError) return parsedError
  }
  return parseToolOutput(source.output)
}

function sourceObservation(
  source: DelegationCardSource
): WorkUnitRunObservation {
  const parsedMeta = parseDelegationMeta(source.meta)
  const toolOutput = sourceToolOutput(source)
  const taskId =
    parseDelegateTaskId(source.output, source.errorText) ??
    parsedMeta?.taskId ??
    null
  const runtimeStats = parsedMeta?.runtimeStats ?? null
  const rawMeta = rawDelegationMeta(source)
  const rawLastAgentActivityAt = rawMeta?.last_agent_activity_at

  return {
    identity: taskId ?? source.parentToolUseId,
    taskId,
    lifecycleStatus: resolveLifecycleStatus({
      binding: undefined,
      parsedMeta,
      runSnapshot: null,
      childProjection: null,
      toolOutput,
      state: source.state,
      errorText: source.errorText,
    }),
    errorCode:
      parsedMeta?.errorCode ??
      (toolOutput?.kind === "outcome" ? toolOutput.errorCode : null),
    startedAt: parsedMeta?.startedAt ?? runtimeStats?.started_at ?? null,
    finishedAt: parsedMeta?.finishedAt ?? runtimeStats?.finished_at ?? null,
    lastAgentActivityAt:
      typeof rawLastAgentActivityAt === "string"
        ? rawLastAgentActivityAt
        : null,
    runtimeStats,
    current: false,
  }
}

export function mergeDelegationWorkUnitModel(input: {
  model: DelegationCardModel
  sources: readonly DelegationCardSource[]
  stickyKey: string | null
  nowMs: number
  hasLiveBinding: boolean
  explicitUserCancel: boolean
}): DelegationCardModel {
  const currentTaskId = input.model.brokerTaskId ?? input.model.taskId
  const currentIdentity =
    currentTaskId ??
    input.sources[input.sources.length - 1]?.parentToolUseId ??
    input.stickyKey ??
    "current"
  const runs = input.sources.map(sourceObservation)
  runs.push({
    identity: currentIdentity,
    taskId: currentTaskId,
    lifecycleStatus: input.model.lifecycleStatus,
    errorCode: input.model.errorCode ?? null,
    startedAt: input.model.startedAt,
    finishedAt: input.model.finishedAt,
    lastAgentActivityAt: null,
    runtimeStats: input.model.runtimeStats,
    current: true,
  })

  const projection = buildDelegationWorkUnitRuntime({
    runs,
    nowMs: input.nowMs,
    hasLiveBinding: input.hasLiveBinding,
    explicitUserCancel: input.explicitUserCancel,
  })
  const runtimeStats = projection.runtimeStats ?? input.model.runtimeStats
  const status =
    projection.statusOverride && input.model.status === "err"
      ? projection.statusOverride
      : input.model.status

  return {
    ...input.model,
    status,
    lifecycleStatus:
      projection.lifecycleOverride ?? input.model.lifecycleStatus,
    errorCode: projection.suppressErrorCode ? undefined : input.model.errorCode,
    startedAt: projection.startedAt ?? input.model.startedAt,
    finishedAt: projection.activeSticky
      ? null
      : (projection.finishedAt ?? input.model.finishedAt),
    elapsedMs: projection.elapsedMs ?? input.model.elapsedMs,
    runtimeStats,
    toolCallCount: projection.toolCallCount ?? input.model.toolCallCount,
    editRollup: buildEditRollupViewModel(runtimeStats),
    showGeneratingSegment: projection.activeSticky,
    stickyKey: input.stickyKey,
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

export function useDelegationCardModel(
  source: DelegationCardSource,
  options?: {
    workUnitSources?: readonly DelegationCardSource[]
    stickyKey?: string | null
    explicitUserCancel?: boolean
  }
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

  const toolOutput = useMemo<ParsedToolOutput | null>(() => {
    if (errorText) {
      const parsedError = parseToolOutput(errorText, true)
      if (parsedError) return parsedError
    }
    return parseToolOutput(output)
  }, [output, errorText])
  const fallbackUncorrelatedFailure = isUncorrelatedDelegationFailure(
    toolOutput,
    parsedMeta?.taskId ?? displayTaskId
  )
  const fallbackChildConversationId = fallbackUncorrelatedFailure
    ? null
    : (parsedMeta?.childConversationId ??
      toolOutput?.childConversationId ??
      null)

  // Source-local task id before live binding (may be contaminated by a later
  // continue on the shared child). Used to scope binding + snapshot fetch.
  const sourceTaskId = parsedMeta?.taskId ?? displayTaskId

  // `enabled: false` — the model never fetches the child's persisted detail
  // here; cold title/stats come from `delegationChildProjectionCache`.
  // Still pass fallbackChildConversationId for the hook's internal child id
  // resolution path, but discard any binding that is not this exact turn.
  const { binding: rawBinding } = useDelegatedSubSession(parentToolUseId, {
    enabled: false,
    fallbackChildConversationId,
  })
  const binding = scopeDelegationBindingForCard(
    rawBinding,
    parentToolUseId,
    sourceTaskId
  )

  const snapshotTaskId = binding?.taskId ?? sourceTaskId
  const runSnapshot = useDelegationRunSnapshot(
    parentConversationId,
    snapshotTaskId
  )

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

  const defaultWorkUnitSources = useMemo<readonly DelegationCardSource[]>(
    () => [
      {
        parentToolUseId,
        parentConversationId,
        input,
        output,
        errorText,
        state,
        meta,
      },
    ],
    [
      parentToolUseId,
      parentConversationId,
      input,
      output,
      errorText,
      state,
      meta,
    ]
  )
  const workUnitSources = options?.workUnitSources ?? defaultWorkUnitSources
  const stickyKey = options?.stickyKey ?? null
  const explicitUserCancel = options?.explicitUserCancel ?? false

  // Build the same sticky merge used by the final model before subscribing;
  // recoverable terminal records must keep the shared ticker alive.
  const previewNowMs = Date.now()
  const previewModel = mergeDelegationWorkUnitModel({
    model: buildDelegationCardModel({
      parsedInput,
      parsedMeta,
      toolOutput,
      binding,
      runSnapshot,
      childProjection,
      childAwaitingPermission,
      state,
      errorText,
      nowMs: previewNowMs,
      displayTaskId,
    }),
    sources: workUnitSources,
    stickyKey,
    nowMs: previewNowMs,
    hasLiveBinding: binding != null,
    explicitUserCancel,
  })
  const tickerEligible = isTickerEligible(previewModel)

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
    () => {
      const nowMs = Date.now()
      return mergeDelegationWorkUnitModel({
        model: buildDelegationCardModel({
          parsedInput,
          parsedMeta,
          toolOutput,
          binding,
          runSnapshot,
          childProjection,
          childAwaitingPermission,
          state,
          errorText,
          nowMs,
          displayTaskId,
        }),
        sources: workUnitSources,
        stickyKey,
        nowMs,
        hasLiveBinding: binding != null,
        explicitUserCancel,
      })
    },
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
      workUnitSources,
      stickyKey,
      explicitUserCancel,
      tickerVersion,
    ]
  )
}
