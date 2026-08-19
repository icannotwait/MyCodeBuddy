import type { AgentType } from "@/lib/types"

/**
 * Agents that emit normalized request-usage samples (see
 * `acp::request_usage`). Adding a new adapter is: parse it in Rust, then
 * add the agent type here.
 */
const REQUEST_USAGE_AGENTS = new Set<AgentType>([
  "claude_code",
  "codex",
  "grok",
])

export function supportsRequestUsageDisplay(agentType: AgentType): boolean {
  return REQUEST_USAGE_AGENTS.has(agentType)
}

export interface RequestUsageSample {
  outputTokens: number
  durationMs?: number | null
}

export interface RequestUsageSnapshot {
  outputTokens: number
  generationMs: number
  tps: number
  sampleCount: number
}

export interface TurnGenerationStat {
  userOrdinal: number
  generationMs: number
  generationTokens: number
}

export interface OverlayTurn {
  id: string
  role: string
  generation_ms?: number | null
  generation_tokens?: number | null
}

export interface HiddenUserTurnsDetail {
  history_window?: {
    total_user_turn_count: number
    returned_user_turn_count: number
  } | null
  user_turns_before_offset?: number | null
}

export interface UserOrdinalTurn {
  id: string
  role: string
}

/** Prefer the agent-reported duration; otherwise the client-measured burst. */
export function resolveRequestUsageSample(
  sample: RequestUsageSample,
  measuredDurationMs?: number | null
): RequestUsageSample {
  const reported = sample.durationMs ?? 0
  if (reported > 0) return { ...sample, durationMs: reported }
  const measured = measuredDurationMs ?? 0
  return { ...sample, durationMs: measured > 0 ? measured : 0 }
}

/**
 * 0-based index of the user turn that started the in-flight reply.
 * Windowed history contributes hidden user turns via total − returned.
 */
export function userOrdinalForCurrentTurn(opts: {
  totalUserTurnCount: number
  returnedUserTurnCount: number
  loadedTurns: readonly UserOrdinalTurn[]
  localTurns: readonly UserOrdinalTurn[]
}): number {
  const hidden = Math.max(
    0,
    opts.totalUserTurnCount - opts.returnedUserTurnCount
  )
  const loadedIds = new Set(opts.loadedTurns.map((t) => t.id))
  let extraUser = 0
  for (const t of opts.localTurns) {
    if (t.role === "user" && !loadedIds.has(t.id)) extraUser += 1
  }
  let loadedUser = 0
  for (const t of opts.loadedTurns) {
    if (t.role === "user") loadedUser += 1
  }
  return Math.max(0, hidden + loadedUser + extraUser - 1)
}

/**
 * User turns before the loaded window. Prefers `history_window` totals;
 * falls back to the index-window prefix count.
 */
export function hiddenUserTurnsFromDetail(
  detail: HiddenUserTurnsDetail | null | undefined
): number {
  const window = detail?.history_window
  if (window) {
    return Math.max(
      0,
      window.total_user_turn_count - window.returned_user_turn_count
    )
  }
  return Math.max(0, detail?.user_turns_before_offset ?? 0)
}

/** Stamp generation stats onto the first assistant after each matching user. */
export function overlayGenerationOnTurns<T extends OverlayTurn>(
  turns: T[],
  stats: readonly TurnGenerationStat[],
  userOrdinalOffset = 0
): T[] {
  if (stats.length === 0) return turns
  const byOrdinal = new Map(stats.map((s) => [s.userOrdinal, s]))
  let userIdx = userOrdinalOffset
  const out = turns.slice()
  for (let i = 0; i < out.length; i++) {
    if (out[i].role !== "user") continue
    const stat = byOrdinal.get(userIdx)
    userIdx += 1
    if (!stat) continue
    let assistantIdx = -1
    for (let j = i + 1; j < out.length; j++) {
      if (out[j].role === "user") break
      if (out[j].role === "assistant") {
        assistantIdx = j
        break
      }
    }
    if (assistantIdx < 0) continue
    if (out[assistantIdx].generation_ms != null) continue
    out[assistantIdx] = {
      ...out[assistantIdx],
      generation_ms: stat.generationMs,
      generation_tokens: stat.generationTokens,
    }
  }
  return out
}

export const EMPTY_REQUEST_USAGE: RequestUsageSnapshot = {
  outputTokens: 0,
  generationMs: 0,
  tps: 0,
  sampleCount: 0,
}

const EMPTY = EMPTY_REQUEST_USAGE

export function accumulateRequestUsage(
  prev: RequestUsageSnapshot,
  sample: RequestUsageSample
): RequestUsageSnapshot {
  const tokens = sample.outputTokens
  const durationMs = sample.durationMs ?? 0
  if (tokens <= 0 || durationMs <= 0) return prev
  const outputTokens = prev.outputTokens + tokens
  const generationMs = prev.generationMs + durationMs
  return {
    outputTokens,
    generationMs,
    tps: outputTokens / (generationMs / 1000),
    sampleCount: prev.sampleCount + 1,
  }
}

/** Token-weighted output speed across completed request-usage samples. */
export class RequestUsageAccumulator {
  private outputTokens = 0
  private generationMs = 0
  private sampleCount = 0

  push(sample: RequestUsageSample): void {
    const tokens = sample.outputTokens
    const durationMs = sample.durationMs ?? 0
    if (tokens <= 0 || durationMs <= 0) return
    this.outputTokens += tokens
    this.generationMs += durationMs
    this.sampleCount += 1
  }

  reset(): void {
    this.outputTokens = 0
    this.generationMs = 0
    this.sampleCount = 0
  }

  snapshot(): RequestUsageSnapshot {
    if (this.sampleCount === 0 || this.generationMs <= 0) {
      return { ...EMPTY }
    }
    return {
      outputTokens: this.outputTokens,
      generationMs: this.generationMs,
      tps: this.outputTokens / (this.generationMs / 1000),
      sampleCount: this.sampleCount,
    }
  }
}
