import { closeFolderIfEmpty } from "@/lib/api"
import {
  getFolderEventGeneration,
  useAppWorkspaceStore,
} from "@/stores/app-workspace-store"

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
 * Apply a successful conditional close locally, fenced against concurrent
 * reopen / draft retarget that advanced membership generation or re-bound
 * the singleton draft to `folderId` while the request was in flight.
 *
 * Stale `closed: true` must not strip membership under a live draft.
 */
function applyClosedTrue(
  folderId: number,
  generationAtStart: number,
  draftStillOnFolder: DraftOnFolderCheck
): void {
  const fencedRefetch = () => useAppWorkspaceStore.getState().fetchFolders()

  // Generation advanced (user reopen / upsert / optimistic membership change
  // after we captured) → do not drop; reconverge while preserving intent.
  if (getFolderEventGeneration() !== generationAtStart) {
    void fencedRefetch()
    return
  }
  // Draft retargeted back onto F without a membership event — keep open list.
  if (draftStillOnFolder(folderId)) {
    void fencedRefetch()
    return
  }

  useAppWorkspaceStore.getState().dropFolderFromOpenList(folderId)
  void fencedRefetch()
}

/**
 * Visibility-only empty-folder close after a draft leave transition.
 * Never uses the user-remove cascade. Applies the draft-leave result table:
 *
 * | closed: true  | drop only if gen+draft fence holds; else preserve + refetch |
 * | closed: false | fenced membership refetch; no draft recreate               |
 * | transport err | fenced refetch; retry once if still open+leave             |
 */
export async function maybeCloseEmptyFolder(
  folderId: number,
  draftStillOnFolder: DraftOnFolderCheck
): Promise<void> {
  if (!leavePredicateHolds(folderId, draftStillOnFolder)) return

  const fencedRefetch = () => useAppWorkspaceStore.getState().fetchFolders()
  // Capture membership generation before the network call so a concurrent
  // reopen/upsert cannot be clobbered by a stale closed:true.
  const generationAtStart = getFolderEventGeneration()

  try {
    const { closed } = await closeFolderIfEmpty(folderId)
    if (closed) {
      applyClosedTrue(folderId, generationAtStart, draftStillOnFolder)
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
    // Retry captures a fresh generation so reopen during the first attempt is
    // not ignored, and reopen during the retry is still fenced.
    const retryGeneration = getFolderEventGeneration()
    try {
      const { closed } = await closeFolderIfEmpty(folderId)
      if (closed) {
        applyClosedTrue(folderId, retryGeneration, draftStillOnFolder)
      } else {
        void fencedRefetch()
      }
    } catch (retryErr) {
      console.error(
        "[maybeCloseEmptyFolder] closeFolderIfEmpty retry failed:",
        retryErr
      )
      void fencedRefetch()
    }
  }
}
