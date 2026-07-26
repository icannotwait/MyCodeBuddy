import type { SessionConfigOptionInfo } from "@/lib/types"

/**
 * Codex ACP (official npm) advertises a session config option for Fast mode
 * (`service_tier=fast`). Product hides this control in settings and in the
 * session composer; filter it at the host boundary so any agent package that
 * still emits it never reaches UI or preference plumbing.
 */
export const HIDDEN_SESSION_CONFIG_OPTION_IDS = new Set(["fast-mode"])

export function isHiddenSessionConfigOptionId(configId: string): boolean {
  return HIDDEN_SESSION_CONFIG_OPTION_IDS.has(configId)
}

export function isHiddenSessionConfigOption(
  option: Pick<SessionConfigOptionInfo, "id">
): boolean {
  return isHiddenSessionConfigOptionId(option.id)
}

/**
 * Drop host-hidden session config options. Returns the same array reference
 * when nothing is filtered (cheap identity for React equality checks).
 */
export function filterSessionConfigOptions(
  options: SessionConfigOptionInfo[] | null | undefined
): SessionConfigOptionInfo[] | null {
  if (options == null) return null
  if (options.length === 0) return options
  if (!options.some(isHiddenSessionConfigOption)) return options
  return options.filter((option) => !isHiddenSessionConfigOption(option))
}
