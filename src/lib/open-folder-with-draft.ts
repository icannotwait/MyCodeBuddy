import type { AgentType, FolderDetail } from "@/lib/types"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabStore } from "@/stores/tab-store"

export type OpenFolderWithDraftOptions = {
  inheritFromActive?: boolean
  folderDefaultAgent?: AgentType | null
  folderRecentAgent?: AgentType | null
}

function draftOptionsFromDetail(
  detail: FolderDetail,
  options?: OpenFolderWithDraftOptions
) {
  return {
    inheritFromActive: options?.inheritFromActive,
    folderDefaultAgent:
      options?.folderDefaultAgent !== undefined
        ? options.folderDefaultAgent
        : detail.default_agent_type,
    folderRecentAgent:
      options?.folderRecentAgent !== undefined
        ? options.folderRecentAgent
        : detail.last_agent_type,
  }
}

/**
 * User-intent open choke point: silent membership open + ensure singleton draft
 * targets the folder. Low-level store `openFolder` / `openWorktreeFolder` /
 * `addFolderToWorkspaceById` stay silent (deep-link, pet-focus, system reg).
 */
export async function openFolderWithDraft(
  path: string,
  options?: OpenFolderWithDraftOptions
): Promise<FolderDetail> {
  const detail = await useAppWorkspaceStore.getState().openFolder(path)
  useTabStore
    .getState()
    .openNewConversationTab(
      detail.id,
      detail.path,
      draftOptionsFromDetail(detail, options)
    )
  return detail
}

/** User-intent worktree open + draft (branch switch / new worktree UX). */
export async function openWorktreeFolderWithDraft(
  path: string,
  sourceFolderId: number,
  options?: OpenFolderWithDraftOptions
): Promise<FolderDetail> {
  const detail = await useAppWorkspaceStore
    .getState()
    .openWorktreeFolder(path, sourceFolderId)
  useTabStore
    .getState()
    .openNewConversationTab(
      detail.id,
      detail.path,
      draftOptionsFromDetail(detail, options)
    )
  return detail
}

/** User-intent open-by-id + draft (history / switch-to-registered folder). */
export async function openFolderByIdWithDraft(
  folderId: number,
  options?: OpenFolderWithDraftOptions
): Promise<FolderDetail> {
  const detail = await useAppWorkspaceStore
    .getState()
    .addFolderToWorkspaceById(folderId)
  useTabStore
    .getState()
    .openNewConversationTab(
      detail.id,
      detail.path,
      draftOptionsFromDetail(detail, options)
    )
  return detail
}
