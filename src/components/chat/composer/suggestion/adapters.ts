import type { FlatFileEntry } from "@/hooks/use-file-tree"
import {
  AGENT_LABELS,
  type AcpAgentInfo,
  type DbConversationSummary,
  type GitLogEntry,
} from "@/lib/types"

import type { SuggestionItem } from "./types"

/**
 * Build a `file://` URI from an absolute path (POSIX or Windows), percent-
 * encoding each path segment so spaces / `#` / `?` / `%` can't corrupt the URI.
 * Mirrors `toFileUri` in message-input.tsx.
 */
export function pathToFileUri(absolutePath: string): string {
  const normalized = absolutePath.replace(/\\/g, "/")
  const encoded = normalized.split("/").map(encodeURIComponent).join("/")
  return normalized.startsWith("/") ? `file://${encoded}` : `file:///${encoded}`
}

function joinPath(root: string, relative: string): string {
  const left = root.replace(/[/\\]+$/, "")
  const right = relative.replace(/^[/\\]+/, "")
  return left ? `${left}/${right}` : right
}

/** Workspace file → file reference (uri built from the workspace root). */
export function fileToSuggestion(
  entry: FlatFileEntry,
  workspaceRoot: string
): SuggestionItem {
  return {
    reference: {
      refType: "file",
      id: entry.relativePath,
      label: entry.name,
      uri: pathToFileUri(joinPath(workspaceRoot, entry.relativePath)),
      meta: { fileKind: entry.kind },
    },
    detail: entry.relativePath,
    keywords: entry.relativePath,
  }
}

/**
 * ACP agent → agent reference. Carries a `codeg://agent/<agent_type>` uri as a
 * routing anchor: it serializes inline as `[@label](codeg://agent/…)` and
 * renders as a badge in the transcript. The uri is opaque to the agent (the
 * readable `@label` carries the meaning); resolving it to real routing is a
 * future, separate concern.
 */
export function agentToSuggestion(agent: AcpAgentInfo): SuggestionItem {
  return {
    reference: {
      refType: "agent",
      id: agent.agent_type,
      label: agent.name || AGENT_LABELS[agent.agent_type],
      uri: `codeg://agent/${agent.agent_type}`,
      meta: { agentType: agent.agent_type, available: agent.available },
    },
    detail: agent.description || null,
    keywords: agent.agent_type,
  }
}

/**
 * Conversation → session reference. The serialization uri encodes the agent's
 * own session id as `codeg://session/<agent_type>_<external_id>` (so a transcript
 * badge can show the right agent icon and a future codeg-mcp can resolve it by
 * `(agent_type, external_id)`); sessions without an `external_id` fall back to
 * the internal numeric id. The in-app `id` stays the numeric id either way.
 */
export function sessionToSuggestion(
  conversation: DbConversationSummary
): SuggestionItem {
  const label = conversation.title?.trim() || `#${conversation.id}`
  const uri = conversation.external_id
    ? `codeg://session/${conversation.agent_type}_${conversation.external_id}`
    : `codeg://session/${conversation.id}`
  return {
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
  }
}

/**
 * Git commit → commit reference (`codeg://commit/<repoKey>@<fullHash>`).
 * `repoKey` identifies the repository (e.g. its path) and is URI-encoded.
 */
export function commitToSuggestion(
  entry: GitLogEntry,
  repoKey: string
): SuggestionItem {
  return {
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
  }
}

// Skills, commands and experts are no longer surfaced in the `@` panel — they
// are inserted via the `/` / `$` triggers and the expert menu, which build their
// reference attrs directly (see composer/invocation-reference.ts).
