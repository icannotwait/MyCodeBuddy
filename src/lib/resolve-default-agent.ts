import { AGENT_DISPLAY_ORDER, type AgentType } from "@/lib/types"

export interface ResolveDefaultAgentInput {
  /** Folder's saved `default_agent_type`, or null if none set. */
  folderDefault: AgentType | null
  /**
   * Agent to inherit when "new conversation" is launched from inside a
   * real conversation context (caller passes the active tab's agentType
   * only when its `conversationId != null`).
   */
  inherit: AgentType | null
  /**
   * Agent used by the latest normally-created root conversation in this
   * folder. Validated against `sortedTypes` once `fresh` — unavailable
   * recency is corrected to `sortedTypes[0]`/fallback without deleting
   * the persisted value.
   */
  folderRecent: AgentType | null
  /**
   * User-sorted list of enabled+available agents. Empty during cold start
   * before the first successful `acpListAgents()` call.
   */
  sortedTypes: AgentType[]
  /** True once `acpListAgents()` has succeeded at least once this session. */
  fresh: boolean
}

export interface ResolveDefaultAgentResult {
  agentType: AgentType
  /**
   * True when the returned `agentType` is a "best guess" tentative value
   * that should be re-evaluated once the agent list becomes fresh — the
   * caller (TabProvider) tracks this on draft tabs and patches them when
   * fresh data arrives. Always false when `agentType` came from either
   * `folderDefault` or `inherit` (those are explicit user intent).
   */
  provisional: boolean
}

/**
 * Decide which agent a freshly-opened conversation should use. Pure
 * function — no side effects, no React, no DB. Lives outside hooks so the
 * priority rules can be reasoned about (and unit-tested) without spinning
 * up a renderer.
 *
 * Priority (highest first):
 *   1. `folderDefault` — the user explicitly pinned a default on this folder.
 *   2. `inherit` — "new conversation" launched from inside an existing
 *      conversation should produce another conversation with the same agent
 *      (only when the caller explicitly requests inheritance).
 *   3. `folderRecent` — the agent used by the latest normally-created root
 *      conversation in this folder. Before hydration (`fresh === false`) the
 *      recent value is returned provisionally; once fresh, it is usable only
 *      if present in `sortedTypes` — otherwise the resolver falls through
 *      without deleting the persisted recency.
 *   4. `sortedTypes[0]` — first entry of the user-managed drag-sorted list.
 *   5. `AGENT_DISPLAY_ORDER[0]` — final fallback when even the sorted list
 *      isn't available yet (cold start).
 *
 * The result is marked `provisional: true` for cases 3 (pre-fresh), 4, and 5
 * when `fresh` is false — i.e. the sorted list might still be stale or empty
 * seed data from localStorage, and the caller should re-resolve once fresh
 * data arrives.
 */
export function resolveDefaultAgent(
  input: ResolveDefaultAgentInput
): ResolveDefaultAgentResult {
  const { folderDefault, inherit, folderRecent, sortedTypes, fresh } = input
  if (folderDefault) {
    return { agentType: folderDefault, provisional: false }
  }
  if (inherit) {
    return { agentType: inherit, provisional: false }
  }
  if (folderRecent) {
    if (!fresh) {
      return { agentType: folderRecent, provisional: true }
    }
    if (sortedTypes.includes(folderRecent)) {
      return { agentType: folderRecent, provisional: false }
    }
  }
  if (sortedTypes.length > 0) {
    return { agentType: sortedTypes[0], provisional: !fresh }
  }
  return { agentType: AGENT_DISPLAY_ORDER[0], provisional: !fresh }
}
