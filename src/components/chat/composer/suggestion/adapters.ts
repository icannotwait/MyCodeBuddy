import type { FlatFileEntry } from "@/hooks/use-file-tree"
import { formatConversationTitle } from "@/lib/conversation-title"
import { buildFileUri } from "@/lib/reference-link"
import {
  AGENT_LABELS,
  type AcpAgentInfo,
  type DbConversationSummary,
  type DelegationProfile,
  type GitLogEntry,
  type ReferenceCandidate,
} from "@/lib/types"

import type { SuggestionItem } from "./types"

function joinPath(root: string, relative: string): string {
  const left = root.replace(/[/\\]+$/, "")
  const right = relative.replace(/^[/\\]+/, "")
  return left ? `${left}/${right}` : right
}

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

/** Workspace file → file reference (uri built from the workspace root). */
export function fileToSuggestion(
  entry: FlatFileEntry,
  workspaceRoot: string,
  sourceOrdinal = 0
): SuggestionItem {
  return withControllerDefaults({
    reference: {
      refType: "file",
      id: entry.relativePath,
      label: entry.name,
      uri: buildFileUri(joinPath(workspaceRoot, entry.relativePath)),
      meta: { fileKind: entry.kind },
    },
    detail: entry.relativePath,
    keywords: entry.relativePath,
    sourceOrdinal,
  })
}

/**
 * ACP agent → agent reference. Carries a `codeg://agent/<agent_type>` uri as a
 * routing anchor: it serializes inline as `[@label](codeg://agent/…)` and
 * renders as a badge in the transcript. The uri is opaque to the agent (the
 * readable `@label` carries the meaning); resolving it to real routing is a
 * future, separate concern.
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
 * Conversation → session reference. The serialization uri encodes codeg's
 * internal numeric conversation id as `codeg://session/<conversation_id>` — the
 * stable key the `get_session_info` MCP tool resolves directly (it then reads the
 * row's bound `external_id` + `agent_type` server-side). The `@`-panel option row
 * still shows the owning agent's icon via `meta.agentType`; the inline session
 * badge shows a neutral conversation glyph, not the agent icon.
 */
export function sessionToSuggestion(
  conversation: DbConversationSummary,
  sourceOrdinal = 0
): SuggestionItem {
  // Fold any inline reference badges in the title (`[name](file://…)`, …) down
  // to their bracket text, so the panel row and the inserted session badge read
  // like the sidebar's title (`README.md fix`, not raw `[README.md](…)`) rather
  // than leaking serialized Markdown. The numeric `#id` fallback also covers a
  // whitespace-only title (folding can't turn blank into non-blank).
  const label =
    formatConversationTitle(conversation.title).trim() || `#${conversation.id}`
  const uri = `codeg://session/${conversation.id}`
  return withControllerDefaults({
    reference: {
      refType: "session",
      id: String(conversation.id),
      label,
      uri,
      meta: {
        agentType: conversation.agent_type,
        status: conversation.status,
        branch: conversation.git_branch,
      },
    },
    detail: conversation.git_branch || conversation.status,
    keywords: `${label} ${conversation.agent_type}`,
    sourceOrdinal,
  })
}

/**
 * Git commit → commit reference (`codeg://commit/<repoKey>@<fullHash>`).
 * `repoKey` identifies the repository (e.g. its path) and is URI-encoded.
 */
export function commitToSuggestion(
  entry: GitLogEntry,
  repoKey: string,
  sourceOrdinal = 0
): SuggestionItem {
  return withControllerDefaults({
    reference: {
      refType: "commit",
      id: entry.full_hash,
      label: entry.hash,
      uri: `codeg://commit/${encodeURIComponent(repoKey)}@${entry.full_hash}`,
      meta: {
        shortHash: entry.hash,
        message: entry.message,
        author: entry.author,
        pushed: entry.pushed,
      },
    },
    detail: entry.message,
    keywords: `${entry.hash} ${entry.message} ${entry.author}`,
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

// Skills, commands and experts are no longer surfaced in the `@` panel — they
// are inserted via the `/` and `$` triggers, which build their reference attrs
// directly (see composer/invocation-reference.ts).
