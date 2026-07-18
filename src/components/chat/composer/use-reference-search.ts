"use client"

import { useCallback, useEffect, useMemo, useState } from "react"

import { useAcpAgents } from "@/hooks/use-acp-agents"
import { getGitHead } from "@/lib/api"
import { referenceSearchCache } from "@/lib/reference-search-cache"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type { GitHeadInfo } from "@/lib/types"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useConversationExperienceStore } from "@/stores/conversation-experience-store"
import { useDelegationProfileStore } from "@/stores/delegation-profile-store"

import {
  ReferenceSearchController,
  type ReferenceSearchControllerInputs,
} from "./reference-search-controller"

/** Display headings for each group; injected so the host can localize them. */
export interface ReferenceGroupLabels {
  file: string
  agent: string
  session: string
  commit: string
  skill: string
}

/**
 * English fallbacks, matching the suggestion popup's `emptyLabel`/`loadingLabel`
 * convention (the host passes localized strings at the integration layer).
 */
export const DEFAULT_GROUP_LABELS: ReferenceGroupLabels = {
  file: "Files",
  agent: "Agents",
  session: "Sessions",
  commit: "Commits",
  skill: "Skills",
}

export interface UseReferenceSearchControllerOptions {
  folderId: number | null
  defaultPath: string | null
  enabled: boolean
  labels: ReferenceGroupLabels
}

/**
 * Constructs one independent-source {@link ReferenceSearchController} per
 * backend/folder/path after the shared agent+profile catalog is ready. Returns
 * null before readiness so the mention extension stays inert without rebuilding
 * the editor.
 */
export function useReferenceSearchController({
  folderId,
  defaultPath,
  enabled,
  labels,
}: UseReferenceSearchControllerOptions): ReferenceSearchController | null {
  const { agents, fresh: agentsFresh } = useAcpAgents()
  const profileReady = useDelegationProfileStore((s) => s.ready)
  const profileCatalog = useDelegationProfileStore((s) => s.catalog)
  const profileError = useDelegationProfileStore((s) => s.error)
  const catalogReady = agentsFresh && profileReady
  const referenceLimit =
    useConversationExperienceStore((s) => s.settings?.reference_search_limit) ??
    50
  const gitHead = useAppWorkspaceStore((s) =>
    folderId != null ? (s.gitHeads.get(folderId) ?? null) : null
  )

  const backendKey = getActiveBackendCacheKey()
  const path = defaultPath || null

  const fetchGitHead = useCallback(async (): Promise<GitHeadInfo> => {
    if (!path) {
      return {
        is_repo: false,
        branch: null,
        detached: false,
        short_sha: null,
        canonical_repo: null,
        head_sha: null,
        reference_source_epoch: null,
      }
    }
    return getGitHead(path)
  }, [path])

  const applyGitHead = useCallback(
    (head: GitHeadInfo) => {
      if (folderId == null) return
      useAppWorkspaceStore.getState().applyGitHead(folderId, head)
    },
    [folderId]
  )

  const [controller, setController] =
    useState<ReferenceSearchController | null>(null)

  // Stable constructor identity: backend / folder / path / enabled / ready.
  useEffect(() => {
    if (!enabled || !catalogReady) {
      setController((prev) => {
        prev?.close()
        return null
      })
      return
    }

    const next = new ReferenceSearchController({
      backendKey,
      folderId,
      defaultPath: path,
      cache: referenceSearchCache,
      fetchGitHead,
      applyGitHead,
    })
    setController(next)
    return () => {
      next.close()
    }
    // fetchGitHead/applyGitHead are recreated when path/folderId change, which
    // is already covered by those deps.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, catalogReady, backendKey, folderId, path])

  const inputs: ReferenceSearchControllerInputs = useMemo(
    () => ({
      agents,
      profileCatalog,
      profileCatalogError: Boolean(profileError),
      referenceLimit,
      gitHead,
      labels,
    }),
    [agents, profileCatalog, profileError, referenceLimit, gitHead, labels]
  )

  useEffect(() => {
    controller?.updateInputs(inputs)
  }, [controller, inputs])

  return controller
}
