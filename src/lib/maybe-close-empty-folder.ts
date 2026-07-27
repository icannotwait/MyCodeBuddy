import { closeFolderIfEmpty } from "@/lib/api"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"

/**
 * Draft-leave helpers for empty-folder visibility.
 *
 * Tab-store wires these after local leave transitions. The draft-on-folder
 * check is injected to avoid a circular import with `tab-store`.
 */

export type DraftOnFolderCheck = (folderId: number) => boolean

/** Zero local live conversations for `folderId` (pre-check only). */
export function hasLocalLiveConversations(folderId: number): boolean {
  return useAppWorkspaceStore
    .getState()
    .conversations.some((c) => c.folder_id === folderId)
}

/**
 * Local leave predicate: zero live conversations and no draft still bound.
 * Backend remains authoritative on the conditional-close call.
 */
export function leavePredicateHolds(
  folderId: number,
  draftStillOnFolder: DraftOnFolderCheck
): boolean {
  if (folderId <= 0) return false
  if (hasLocalLiveConversations(folderId)) return false
  return !draftStillOnFolder(folderId)
}

/**
 * Visibility-only empty-folder close after a draft leave transition.
 * Never uses the user-remove cascade. Applies the draft-leave result table:
 *
 * | closed: true  | idempotent local drop; optional fenced refetch |
 * | closed: false | fenced membership refetch; no draft recreate   |
 * | transport err | fenced refetch; retry once if still open+leave |
 */
export async function maybeCloseEmptyFolder(
  folderId: number,
  draftStillOnFolder: DraftOnFolderCheck
): Promise<void> {
  if (!leavePredicateHolds(folderId, draftStillOnFolder)) return

  const dropIdempotent = () => {
    useAppWorkspaceStore.getState().dropFolderFromOpenList(folderId)
  }
  const fencedRefetch = () => useAppWorkspaceStore.getState().fetchFolders()

  try {
    const { closed } = await closeFolderIfEmpty(folderId)
    if (closed) {
      dropIdempotent()
      void fencedRefetch()
    } else {
      // Non-empty / already closed / concurrent change — reconverge membership.
      void fencedRefetch()
    }
  } catch (err) {
    console.error("[maybeCloseEmptyFolder] closeFolderIfEmpty failed:", err)
    await fencedRefetch()
    const stillOpen = useAppWorkspaceStore
      .getState()
      .folders.some((f) => f.id === folderId)
    if (!stillOpen || !leavePredicateHolds(folderId, draftStillOnFolder)) {
      return
    }
    try {
      const { closed } = await closeFolderIfEmpty(folderId)
      if (closed) dropIdempotent()
      void fencedRefetch()
    } catch (retryErr) {
      console.error(
        "[maybeCloseEmptyFolder] closeFolderIfEmpty retry failed:",
        retryErr
      )
      void fencedRefetch()
    }
  }
}
