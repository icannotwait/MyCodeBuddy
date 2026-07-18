import {
  AGENT_LABELS,
  type AcpAgentInfo,
  type DelegationProfile,
  type ReferenceCandidate,
} from "@/lib/types"

import type { SuggestionItem } from "./types"

function withControllerDefaults(
  item: Omit<
    SuggestionItem,
    "selectable" | "freshness" | "sourceOrdinal" | "regexRank"
  > &
    Partial<
      Pick<
        SuggestionItem,
        "selectable" | "freshness" | "sourceOrdinal" | "regexRank"
      >
    >
): SuggestionItem {
  return {
    ...item,
    selectable: item.selectable ?? true,
    freshness: item.freshness ?? "fresh",
    sourceOrdinal: item.sourceOrdinal ?? 0,
    regexRank: item.regexRank ?? null,
  }
}

export type CatalogSearchEntry =
  | { kind: "agent"; agent: AcpAgentInfo }
  | {
      kind: "profile"
      profile: DelegationProfile
      backingAgent: AcpAgentInfo
    }

/**
 * Declared primary/secondary fields for Agent/Profile catalog matching.
 * Primary is the display name; secondary is agent type, description, model
 * (empty model slot for base agents so field tiers stay comparable).
 */
export function catalogSearchFields(entry: CatalogSearchEntry): {
  primary: string[]
  secondary: string[]
} {
  if (entry.kind === "agent") {
    const { agent } = entry
    return {
      primary: [agent.name || AGENT_LABELS[agent.agent_type]],
      secondary: [agent.agent_type, agent.description, ""],
    }
  }
  const { profile, backingAgent } = entry
  return {
    primary: [`${AGENT_LABELS[profile.agent_type]}:${profile.name}`],
    secondary: [
      profile.agent_type,
      backingAgent.description,
      profile.config_values.model ?? "",
    ],
  }
}

export function profileToSuggestion(
  profile: DelegationProfile,
  sourceOrdinal = 0
): SuggestionItem {
  const agentLabel = AGENT_LABELS[profile.agent_type]
  return withControllerDefaults({
    reference: {
      refType: "delegation_profile",
      id: profile.id,
      label: `${agentLabel}:${profile.name}`,
      uri: `codeg://delegation-profile/${profile.agent_type}/${profile.id}`,
      meta: { agentType: profile.agent_type, profileId: profile.id },
    },
    detail: profile.config_values.model ?? null,
    keywords: `${profile.name} ${profile.agent_type} ${profile.config_values.model ?? ""}`,
    sourceOrdinal,
  })
}

/**
 * ACP agent → agent reference. Carries a `codeg://agent/<agent_type>` uri as a
 * routing anchor: it serializes inline as `[@label](codeg://agent/…)` and
 * renders as a badge in the transcript.
 */
export function agentToSuggestion(
  agent: AcpAgentInfo,
  sourceOrdinal = 0
): SuggestionItem {
  return withControllerDefaults({
    reference: {
      refType: "agent",
      id: agent.agent_type,
      label: agent.name || AGENT_LABELS[agent.agent_type],
      uri: `codeg://agent/${agent.agent_type}`,
      meta: { agentType: agent.agent_type, available: agent.available },
    },
    detail: agent.description || null,
    keywords: agent.agent_type,
    sourceOrdinal,
  })
}

/**
 * Map an authoritative backend candidate into a mention suggestion without
 * rebuilding URI/identity/rank values.
 */
export function candidateToSuggestion(
  candidate: ReferenceCandidate,
  freshness: SuggestionItem["freshness"],
  selectable = true
): SuggestionItem {
  const meta = candidate.metadata
  if (meta.kind === "file") {
    return {
      reference: {
        refType: "file",
        id: candidate.id,
        label: candidate.label,
        uri: candidate.uri,
        meta: {
          fileKind: meta.entryKind === "directory" ? "dir" : "file",
        },
      },
      detail: candidate.detail,
      keywords: candidate.keywords,
      selectable,
      freshness,
      sourceOrdinal: candidate.sourceOrdinal,
      regexRank: candidate.regexRank,
    }
  }
  if (meta.kind === "conversation") {
    return {
      reference: {
        refType: "session",
        id: candidate.id,
        label: candidate.label,
        uri: candidate.uri,
        meta: {
          agentType: meta.agentType,
          status: meta.status,
          branch: meta.branch,
        },
      },
      detail: candidate.detail,
      keywords: candidate.keywords,
      selectable,
      freshness,
      sourceOrdinal: candidate.sourceOrdinal,
      regexRank: candidate.regexRank,
    }
  }
  return {
    reference: {
      refType: "commit",
      id: candidate.id,
      label: candidate.label,
      uri: candidate.uri,
      meta: {
        shortHash: meta.shortHash,
        message: meta.subject,
        author: meta.author,
        pushed: null,
      },
    },
    detail: candidate.detail,
    keywords: candidate.keywords,
    selectable,
    freshness,
    sourceOrdinal: candidate.sourceOrdinal,
    regexRank: candidate.regexRank,
  }
}

export function catalogEntryToSuggestion(
  entry: CatalogSearchEntry,
  sourceOrdinal: number,
  freshness: SuggestionItem["freshness"] = "fresh",
  selectable = true
): SuggestionItem {
  if (entry.kind === "agent") {
    return {
      ...agentToSuggestion(entry.agent, sourceOrdinal),
      freshness,
      selectable,
    }
  }
  return {
    ...profileToSuggestion(entry.profile, sourceOrdinal),
    freshness,
    selectable,
  }
}
