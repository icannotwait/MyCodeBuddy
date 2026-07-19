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
  useSyncExternalStore,
} from "react"

import {
  type AgentType,
  type AttentionRequestSummary,
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
import {
  getRunningTickerVersion,
  retainRunningTicker,
  subscribeRunningTicker,
} from "@/lib/delegation-running-ticker"

/** The raw inputs a `delegate_to_agent` tool call carries — the props
 *  `DelegatedSubThread` already receives, and the shape `SubAgentOverlay`
 *  extracts from the last assistant turn's tool-call parts. */
export interface DelegationCardSource {
  parentToolUseId: string
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

/**
 * Resolve lifecycle from highest-priority source. Lower sources cannot
 * reopen a terminal higher source; a higher running source is not overridden
 * by a terminal lower source either.
 */
function resolveLifecycleStatus(input: {
  binding: DelegationBinding | undefined
  parsedMeta: ParsedMeta | null
  childProjection: ChildCardProjection | null
  toolOutput: ParsedToolOutput | null
  state?: ToolCallState
  errorText?: string | null
}): DelegationLifecycleStatus {
  const { binding, parsedMeta, childProjection, toolOutput, state, errorText } =
    input

  if (binding) return binding.status
  if (parsedMeta) return parsedMeta.status

  const fromProj = childProjection
    ? lifecycleFromProjection(childProjection)
    : null
  if (fromProj) return fromProj

  if (state === "output-error" || errorText) {
    if (toolOutput?.kind === "outcome") {
      return toolOutput.isError ? "err" : "ok"
    }
    return "err"
  }
  if (toolOutput?.kind === "ack") return "running"
  if (toolOutput?.kind === "outcome") {
    return toolOutput.isError ? "err" : "ok"
  }
  if (state === "output-available") return "ok"
  return "running"
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
 */
function pickRuntimeStats(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  childProjection: ChildCardProjection | null
): DelegationRuntimeStats | null {
  if (binding) return binding.runtimeStats

  if (parsedMeta) {
    if (parsedMeta.runtimeStats != null) return parsedMeta.runtimeStats
    if (!childProjection?.runtimeStats) return null
    const metaTerminal = parsedMeta.status !== "running"
    if (metaTerminal) {
      // Terminal meta + running summary → do not adopt running stats.
      return childProjection.isTerminal ? childProjection.runtimeStats : null
    }
    // Meta still running: only take non-terminal lower stats.
    return childProjection.isTerminal ? null : childProjection.runtimeStats
  }

  return childProjection?.runtimeStats ?? null
}

/**
 * Attention: higher source wins. Explicit `null` from live/meta is an
 * authoritative clear — do not fall through with `??` to a stale summary.
 */
function pickAttentionRequest(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  childProjection: ChildCardProjection | null
): AttentionRequestSummary | null {
  if (binding) {
    // Binding present → its attention (including null clear). Undefined is
    // treated as null (started events always write attentionRequest).
    return binding.attentionRequest ?? null
  }
  if (parsedMeta) {
    // ParsedMeta always includes attentionRequest (null when absent/invalid).
    return parsedMeta.attentionRequest
  }
  return childProjection?.attentionRequest ?? null
}

function pickStartedAt(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  childProjection: ChildCardProjection | null,
  runtimeStats: DelegationRuntimeStats | null
): string | null {
  if (binding) return binding.startedAt || null
  if (parsedMeta?.startedAt) return parsedMeta.startedAt
  if (childProjection?.startedAt) return childProjection.startedAt
  return runtimeStats?.started_at ?? null
}

function pickFinishedAt(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
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
    return (
      binding.finishedAt ??
      binding.runtimeStats.finished_at ??
      null
    )
  }
  if (parsedMeta) {
    if (parsedMeta.finishedAt) return parsedMeta.finishedAt
    if (parsedMeta.runtimeStats?.finished_at) {
      return parsedMeta.runtimeStats.finished_at
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
  return (
    childProjection?.finishedAt ??
    runtimeStats?.finished_at ??
    null
  )
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

/**
 * Pure field-level merge for a delegation card. See plan locked contracts:
 * live binding > ToolUse meta > child projection; attention null clears;
 * lifecycle terminal locks; duration from completion then tool output.
 */
export function buildDelegationCardModel(input: {
  parsedInput: ReturnType<typeof parseInput>
  parsedMeta: ParsedMeta | null
  toolOutput: ParsedToolOutput | null
  binding: DelegationBinding | undefined
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
    childProjection,
    childAwaitingPermission,
    state,
    errorText,
    nowMs,
    displayTaskId = null,
  } = input

  const lifecycleStatus = resolveLifecycleStatus({
    binding,
    parsedMeta,
    childProjection,
    toolOutput,
    state,
    errorText,
  })

  const status = resolveDelegationStatus({
    binding,
    parsedMeta,
    toolOutput,
    state,
    errorText,
    childAwaitingPermission,
  })

  const runtimeStats = pickRuntimeStats(binding, parsedMeta, childProjection)
  const attentionRequest = pickAttentionRequest(
    binding,
    parsedMeta,
    childProjection
  )
  const startedAt = pickStartedAt(
    binding,
    parsedMeta,
    childProjection,
    runtimeStats
  )
  const finishedAt = pickFinishedAt(
    binding,
    parsedMeta,
    childProjection,
    runtimeStats,
    lifecycleStatus
  )
  const completedDurationMs = pickCompletedDurationMs(binding, toolOutput)

  const brokerTaskId =
    binding?.taskId ??
    parsedMeta?.taskId ??
    childProjection?.taskId ??
    null

  const childConnectionId =
    binding?.childConnectionId ?? parsedMeta?.childConnectionId ?? null
  const childConversationId =
    binding?.childConversationId ??
    parsedMeta?.childConversationId ??
    toolOutput?.childConversationId ??
    childProjection?.childConversationId ??
    null

  const agentType: AgentType | null = binding?.agentType ?? parsedInput.agentType
  const errorCode =
    binding?.errorCode ?? parsedMeta?.errorCode ?? undefined

  const conversationTitle = childProjection?.title ?? null
  const task = parsedInput.task
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
    binding || parsedInput.agentType || parsedInput.task || parsedMeta
  )

  return {
    agentType,
    agentDisplayLabel: parsedInput.profileLabel,
    task,
    taskId: displayTaskId ?? brokerTaskId,
    brokerTaskId,
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

export function useDelegationCardModel(
  source: DelegationCardSource
): DelegationCardModel {
  const { parentToolUseId, input, output, errorText, state, meta } = source

  const parsedInput = useMemo(() => parseInput(input), [input])
  const parsedMeta = useMemo(() => parseDelegationMeta(meta), [meta])
  const displayTaskId = useMemo(
    () => parseDelegateTaskId(output, errorText),
    [output, errorText]
  )

  // `enabled: false` — the model never fetches the child's persisted detail
  // here; cold title/stats come from `delegationChildProjectionCache`.
  const { binding } = useDelegatedSubSession(parentToolUseId, {
    enabled: false,
  })

  const toolOutput = useMemo<ParsedToolOutput | null>(() => {
    if (errorText) {
      const parsedErr = parseToolOutput(errorText, true)
      if (parsedErr) return parsedErr
    }
    return parseToolOutput(output)
  }, [output, errorText])

  const childConversationId =
    binding?.childConversationId ??
    parsedMeta?.childConversationId ??
    toolOutput?.childConversationId ??
    null

  const childConnectionId =
    binding?.childConnectionId ?? parsedMeta?.childConnectionId ?? null

  const childProjection = useChildCardProjection(childConversationId)
  const childLive = useDelegationChildLive(childConnectionId)
  const childAwaitingPermission = childLive?.pendingPermission != null

  // Eligibility without building the full model (avoids ticker chicken-egg).
  const lifecyclePreview = resolveLifecycleStatus({
    binding,
    parsedMeta,
    childProjection,
    toolOutput,
    state,
    errorText,
  })
  const startedAtPreview = pickStartedAt(
    binding,
    parsedMeta,
    childProjection,
    pickRuntimeStats(binding, parsedMeta, childProjection)
  )
  const tickerEligible =
    lifecyclePreview === "running" && parseTimestampMs(startedAtPreview) != null

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
        childProjection,
        childAwaitingPermission,
        state,
        errorText,
        nowMs: Date.now(),
        displayTaskId,
      }),
    // tickerVersion forces elapsed recompute while running.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- tickerVersion is intentional
    [
      parsedInput,
      parsedMeta,
      toolOutput,
      binding,
      childProjection,
      childAwaitingPermission,
      state,
      errorText,
      displayTaskId,
      tickerVersion,
    ]
  )
}
