import { getTransport } from "@/lib/transport"

/** The fixed roundtable candidates, in display and default order. */
export const ROUNDTABLE_CANDIDATES = [
  "grok",
  "antigravity",
  "cursor",
  "codex",
  "code_buddy",
] as const

export type RoundtableAgentStatusKind =
  | "ready"
  | "disabled"
  | "not_installed"
  | "unsupported"
  | "credential_missing"
  | "unqualified"

/** One row of the read-only `roundtable_agents` status. Presence only. */
export interface RoundtableAgentStatus {
  agent: string
  label: string
  status: RoundtableAgentStatusKind
  enabled: boolean
  installed: boolean
  installed_version: string | null
  profile_version: string | null
  version_matches_profile: boolean
  qualified: boolean
  credential: "settings" | "auth_file" | "missing"
  credential_keys: string[]
  last_qualification: {
    verdict: string | null
    failed_checks: string[]
    observed_at: string | null
  }
}

export interface RoundtableAgentsResponse {
  schema_version: 1
  agents: RoundtableAgentStatus[]
}

// Read-only; lives outside the sealed protocol command set.
export function loadRoundtableAgents() {
  return getTransport().call<RoundtableAgentsResponse>("roundtable_agents", {
    request: {},
  })
}

export function statusByAgent(
  rows: RoundtableAgentStatus[] | null | undefined
): Map<string, RoundtableAgentStatus> {
  return new Map((rows ?? []).map((row) => [row.agent, row]))
}

/** Ready first, then enabled-but-not-ready, disabled/not installed last.
 * Ties keep the fixed candidate order. Unknown status (not loaded yet) keeps
 * the fixed order. */
export function sortRoundtableAgents(
  agents: readonly string[],
  status: Map<string, RoundtableAgentStatus>
): string[] {
  const rank = (agent: string) => {
    const row = status.get(agent)
    if (!row) return 1
    if (row.status === "ready") return 0
    if (row.status === "disabled" || row.status === "not_installed") return 2
    return 1
  }
  return agents
    .map((agent, index) => ({ agent, index }))
    .sort((a, b) => rank(a.agent) - rank(b.agent) || a.index - b.index)
    .map(({ agent }) => agent)
}

export function isAgentSelectable(
  agent: string,
  status: Map<string, RoundtableAgentStatus>
): boolean {
  const row = status.get(agent)
  // Before the status loads, nothing is blocked; preflight still decides.
  return !row || row.status === "ready"
}

/** The first two qualified agents in the fixed order, padded with the
 * historical defaults so a seat is never empty. */
export function defaultRoundtableAgents(
  status: Map<string, RoundtableAgentStatus>
): string[] {
  const ready = ROUNDTABLE_CANDIDATES.filter(
    (agent) => status.get(agent)?.status === "ready"
  )
  const picks: string[] = [...ready.slice(0, 2)]
  for (const fallback of ["grok", "antigravity", ...ROUNDTABLE_CANDIDATES]) {
    if (picks.length >= 2) break
    if (!picks.includes(fallback)) picks.push(fallback)
  }
  return picks
}

/** Next agent for an added seat: an unused ready agent, else any ready one. */
export function nextRoundtableAgent(
  used: string[],
  status: Map<string, RoundtableAgentStatus>
): string {
  const ordered = sortRoundtableAgents(ROUNDTABLE_CANDIDATES, status).filter(
    (agent) => isAgentSelectable(agent, status)
  )
  return ordered.find((agent) => !used.includes(agent)) ?? ordered[0] ?? "grok"
}

/** i18n key suffix (Roundtable.agentStatus_*) for a not-ready agent. */
export function agentStatusReasonKey(row: RoundtableAgentStatus): string {
  if (row.status === "unqualified" && !row.version_matches_profile)
    return "version_mismatch"
  return row.status
}

/** Seat ordinal and agent a readiness error names (`participants[n].agent`). */
export function namedSeat(
  error: unknown
): { ordinal: number; agent: string } | null {
  if (!error || typeof error !== "object" || !("details" in error)) return null
  const details = (error as { details?: unknown }).details
  if (!details || typeof details !== "object") return null
  const fields = (details as { field_errors?: unknown }).field_errors
  if (!Array.isArray(fields)) return null
  for (const field of fields) {
    if (!field || typeof field !== "object") continue
    const path = (field as { path?: unknown }).path
    const reason = (field as { reason?: unknown }).reason
    if (typeof path !== "string" || typeof reason !== "string") continue
    const match = /^participants\[(\d+)\]\.agent$/.exec(path)
    if (match) return { ordinal: Number(match[1]), agent: reason }
  }
  return null
}

export function errorReason(error: unknown): string | null {
  if (!error || typeof error !== "object" || !("details" in error)) return null
  const details = (error as { details?: unknown }).details
  if (!details || typeof details !== "object") return null
  const reason = (details as { reason?: unknown }).reason
  return typeof reason === "string" ? reason : null
}
