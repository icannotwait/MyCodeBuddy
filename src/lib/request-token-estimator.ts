import { estimateTokens } from "@/lib/token-speed"
import type { RequestUsageSample } from "@/lib/request-usage-speed"
import type { AgentType, SessionConfigOptionInfo } from "@/lib/types"

export type EstimatorProvider = "gpt" | "grok"
export type ReasoningEffort =
  | "xhigh"
  | "max"
  | "high"
  | "medium"
  | "low"
  | "unknown"

export interface FrozenReasoningProfile {
  provider: EstimatorProvider
  effort: ReasoningEffort
  reasoningRatio: number
}

const RATIOS: Record<EstimatorProvider, Record<ReasoningEffort, number>> = {
  gpt: {
    xhigh: 0.472,
    max: 0.485,
    high: 0.41,
    medium: 0.4,
    low: 0.4,
    unknown: 0.467,
  },
  grok: {
    xhigh: 0.556,
    max: 0.57,
    high: 0.63,
    medium: 0.4,
    low: 0.4,
    unknown: 0.57,
  },
}

function normalized(value: string | null | undefined): string {
  return value?.trim().toLowerCase() ?? ""
}

function providerFor(agentType: AgentType): EstimatorProvider | null {
  if (agentType === "codex") return "gpt"
  if (agentType === "grok") return "grok"
  return null
}

function supportedEffort(
  provider: EstimatorProvider,
  value: string
): ReasoningEffort {
  const effort = normalized(value) as ReasoningEffort
  if (
    provider === "gpt" &&
    ["xhigh", "max", "high", "medium", "low"].includes(effort)
  ) {
    return effort
  }
  if (
    provider === "grok" &&
    ["xhigh", "high", "medium", "low"].includes(effort)
  ) {
    return effort
  }
  return "unknown"
}

function selectOptions(
  options: readonly SessionConfigOptionInfo[]
): SessionConfigOptionInfo[] {
  return options.filter((option) => option.kind.type === "select")
}

function parseTrailingEffort(value: string): string | null {
  const trimmed = value.trim()
  const match = /^([^\[\]]+)\[([^\[\]]+)\]$/.exec(trimmed)
  if (!match) return null
  const payload = match[2].trim()
  if (!payload) return null
  if (!payload.includes("=")) return normalized(payload)

  const pairs = payload.split(",").map((part) => {
    const separator = part.indexOf("=")
    if (separator <= 0 || separator === part.length - 1) return null
    return [
      normalized(part.slice(0, separator)),
      normalized(part.slice(separator + 1)),
    ] as const
  })
  if (pairs.some((pair) => pair === null)) return null
  const validPairs = pairs.filter(
    (pair): pair is readonly [string, string] => pair !== null
  )
  return (
    validPairs.find(([key]) => key === "reasoning_effort")?.[1] ??
    validPairs.find(([key]) => key === "effort")?.[1] ??
    null
  )
}

export function resolveReasoningProfile(
  agentType: AgentType,
  configOptions: readonly SessionConfigOptionInfo[] | null | undefined
): FrozenReasoningProfile | null {
  const provider = providerFor(agentType)
  if (!provider) return null
  const options = selectOptions(configOptions ?? [])
  const requiredCategory = provider === "gpt" ? "thought_level" : "mode"
  const explicit = options.find(
    (option) =>
      normalized(option.id) === "reasoning_effort" &&
      normalized(option.category) === requiredCategory
  )

  let effort = explicit
    ? supportedEffort(
        provider,
        explicit.kind.type === "select" ? explicit.kind.current_value : ""
      )
    : "unknown"

  if (!explicit) {
    const model =
      options.find((option) => normalized(option.id) === "model") ??
      options.find((option) => normalized(option.category) === "model")
    const parsed =
      model?.kind.type === "select"
        ? parseTrailingEffort(model.kind.current_value)
        : null
    effort = supportedEffort(provider, parsed ?? "")
  }

  return {
    provider,
    effort,
    reasoningRatio: RATIOS[provider][effort],
  }
}

export function expandVisibleTokens(
  visibleTokens: number,
  reasoningRatio: number
): number | null {
  if (!Number.isFinite(visibleTokens) || visibleTokens <= 0) return null
  if (
    !Number.isFinite(reasoningRatio) ||
    reasoningRatio < 0 ||
    reasoningRatio >= 1
  ) {
    return null
  }
  const expanded = visibleTokens / (1 - reasoningRatio)
  if (!Number.isFinite(expanded) || expanded <= 0) return null
  const rounded = Math.round(expanded)
  return rounded > 0 ? rounded : null
}

export interface SnapshotBaseline {
  currentText: string
  currentMeasurement: number
  ownerEpoch: number | null
  snapshotAtEpochStart: number
  epochLocalContribution: number
}

export interface RequestTokenEstimatorState {
  epoch: number
  startedAt: number | null
  visibleTokens: number
  frozenProfile: FrozenReasoningProfile | null
  baselines: ReadonlyMap<string, SnapshotBaseline>
}

export interface EstimatorObservation {
  agentType: Extract<AgentType, "codex" | "grok">
  configOptions: readonly SessionConfigOptionInfo[] | null | undefined
  receivedAt: number
}

export interface EstimatorHydrationSeed {
  planText: string
  toolInputs: readonly (readonly [string, string])[]
}

export interface EstimatedRequestSettlement {
  state: RequestTokenEstimatorState
  sample: RequestUsageSample | null
}

export function createRequestTokenEstimator(): RequestTokenEstimatorState {
  return {
    epoch: 0,
    startedAt: null,
    visibleTokens: 0,
    frozenProfile: null,
    baselines: new Map(),
  }
}

function addContribution(
  state: RequestTokenEstimatorState,
  contribution: number,
  observation: EstimatorObservation
): RequestTokenEstimatorState {
  if (!Number.isFinite(contribution) || contribution === 0) return state
  const nextVisible = Math.max(0, state.visibleTokens + contribution)
  if (state.startedAt !== null) {
    return { ...state, visibleTokens: nextVisible }
  }
  if (contribution < 0 || nextVisible <= 0) {
    return { ...state, visibleTokens: nextVisible }
  }
  const profile = resolveReasoningProfile(
    observation.agentType,
    observation.configOptions
  )
  if (!profile) return state
  return {
    ...state,
    startedAt: observation.receivedAt,
    visibleTokens: nextVisible,
    frozenProfile: profile,
  }
}

export function observeEstimatedDelta(
  state: RequestTokenEstimatorState,
  text: string,
  observation: EstimatorObservation
): RequestTokenEstimatorState {
  return addContribution(state, estimateTokens(text), observation)
}

export function observeEstimatedSnapshot(
  state: RequestTokenEstimatorState,
  key: string,
  text: string,
  observation: EstimatorObservation
): RequestTokenEstimatorState {
  const previous = state.baselines.get(key) ?? {
    currentText: "",
    currentMeasurement: 0,
    ownerEpoch: null,
    snapshotAtEpochStart: 0,
    epochLocalContribution: 0,
  }
  if (text === previous.currentText) return state

  const isAppend = text.startsWith(previous.currentText)
  const currentMeasurement = isAppend
    ? previous.currentMeasurement +
      estimateTokens(text, previous.currentText.length)
    : estimateTokens(text)
  let snapshotAtEpochStart = previous.snapshotAtEpochStart
  let epochLocalContribution = previous.epochLocalContribution

  if (previous.ownerEpoch !== state.epoch) {
    snapshotAtEpochStart = isAppend ? previous.currentMeasurement : 0
    epochLocalContribution = isAppend
      ? currentMeasurement - previous.currentMeasurement
      : currentMeasurement
  } else if (isAppend) {
    epochLocalContribution += currentMeasurement - previous.currentMeasurement
  } else {
    epochLocalContribution = Math.max(
      0,
      currentMeasurement - snapshotAtEpochStart
    )
  }

  const priorLocal =
    previous.ownerEpoch === state.epoch ? previous.epochLocalContribution : 0
  const baselines = new Map(state.baselines)
  baselines.set(key, {
    currentText: text,
    currentMeasurement,
    ownerEpoch: state.epoch,
    snapshotAtEpochStart,
    epochLocalContribution,
  })
  return addContribution(
    { ...state, baselines },
    epochLocalContribution - priorLocal,
    observation
  )
}

export function hasUnsettledEstimatedRequest(
  state: RequestTokenEstimatorState
): boolean {
  return state.startedAt !== null
}

export function hasPositiveEstimatedOutput(
  state: RequestTokenEstimatorState
): boolean {
  return hasUnsettledEstimatedRequest(state) && state.visibleTokens > 0
}

function advanceEpoch(
  state: RequestTokenEstimatorState
): RequestTokenEstimatorState {
  const baselines = new Map<string, SnapshotBaseline>()
  for (const [key, baseline] of state.baselines) {
    baselines.set(key, {
      ...baseline,
      ownerEpoch: null,
      snapshotAtEpochStart: 0,
      epochLocalContribution: 0,
    })
  }
  return {
    epoch: state.epoch + 1,
    startedAt: null,
    visibleTokens: 0,
    frozenProfile: null,
    baselines,
  }
}

export function discardEstimatedRequest(
  state: RequestTokenEstimatorState
): RequestTokenEstimatorState {
  if (state.startedAt === null && state.visibleTokens === 0) return state
  return advanceEpoch(state)
}

export function settleEstimatedRequest(
  state: RequestTokenEstimatorState,
  endedAt: number
): EstimatedRequestSettlement {
  if (state.startedAt === null) return { state, sample: null }
  const nextState = advanceEpoch(state)
  const durationMs = endedAt - state.startedAt
  if (
    !Number.isFinite(durationMs) ||
    durationMs < 1 ||
    state.visibleTokens <= 0 ||
    !state.frozenProfile
  ) {
    return { state: nextState, sample: null }
  }
  const outputTokens = expandVisibleTokens(
    state.visibleTokens,
    state.frozenProfile.reasoningRatio
  )
  return {
    state: nextState,
    sample: outputTokens ? { outputTokens, durationMs, estimated: true } : null,
  }
}

export function replaceEstimatorFromHydration(
  state: RequestTokenEstimatorState,
  seed: EstimatorHydrationSeed
): RequestTokenEstimatorState {
  const baselines = new Map<string, SnapshotBaseline>()
  const addSeed = (key: string, text: string) => {
    baselines.set(key, {
      currentText: text,
      currentMeasurement: estimateTokens(text),
      ownerEpoch: null,
      snapshotAtEpochStart: 0,
      epochLocalContribution: 0,
    })
  }
  addSeed("plan", seed.planText)
  for (const [key, text] of seed.toolInputs) addSeed(key, text)
  return {
    epoch: state.epoch + 1,
    startedAt: null,
    visibleTokens: 0,
    frozenProfile: null,
    baselines,
  }
}
