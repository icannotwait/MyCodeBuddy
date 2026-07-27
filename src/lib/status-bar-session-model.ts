import type { SessionConfigOptionInfo } from "@/lib/types"

/** ACP session config ids for model / reasoning (Codex, Grok, etc.). */
export const SESSION_MODEL_CONFIG_ID = "model"
export const SESSION_REASONING_CONFIG_ID = "reasoning_effort"

export interface SessionModelDisplay {
  /** Display label for the current model (name preferred over raw value). */
  model: string | null
  /**
   * Thinking / reasoning effort from conversation history only.
   * Never synthesized from live session config when archive/turns lack it.
   */
  thinkingLevel: string | null
}

function selectCurrentLabel(
  option: SessionConfigOptionInfo | undefined
): string | null {
  if (!option || option.kind.type !== "select") return null
  const current = option.kind.current_value
  if (!current) return null
  const fromOptions = option.kind.options.find((item) => item.value === current)
  if (fromOptions?.name) return fromOptions.name
  for (const group of option.kind.groups) {
    const hit = group.options.find((item) => item.value === current)
    if (hit?.name) return hit.name
  }
  return current
}

function normalizeLabel(value: string | null | undefined): string | null {
  if (!value) return null
  const trimmed = value.trim()
  return trimmed ? trimmed : null
}

/**
 * Resolve status-bar model + thinking from the open conversation.
 *
 * - **Model**: turns/history first, then live session `configOptions.model`
 * - **Effort**: turns/history **only** — if archive has no effort, do not show
 *   one (live config is not a substitute for real dialogue data)
 */
export function resolveSessionModelDisplay(params: {
  configOptions?: SessionConfigOptionInfo[] | null
  conversationModel?: string | null
  conversationEffort?: string | null
}): SessionModelDisplay {
  const options = params.configOptions ?? []
  const modelOption =
    options.find((option) => option.id === SESSION_MODEL_CONFIG_ID) ??
    options.find((option) => option.category === "model")

  const modelFromTurns = normalizeLabel(params.conversationModel)
  const modelFromSession = selectCurrentLabel(modelOption)

  return {
    model: modelFromTurns ?? modelFromSession,
    thinkingLevel: normalizeLabel(params.conversationEffort),
  }
}
