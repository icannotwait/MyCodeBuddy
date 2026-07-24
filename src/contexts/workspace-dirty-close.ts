/** Which file tabs a close gesture targets. */
export type FileTabCloseRequest =
  | { kind: "one"; tabId: string }
  | { kind: "others"; keepTabId: string }
  | { kind: "all" }

export interface DirtyCloseCheck {
  /** True → the caller must confirm before closing. */
  requiresConfirmation: boolean
  /** Dirty tab's title for the single-tab prompt; undefined otherwise. */
  dirtyTitle?: string
}

/**
 * Pure decision for the dirty-close guard, kept OUTSIDE every setState
 * updater: updaters must stay side-effect free (StrictMode double-invokes
 * them in dev, which used to show a blocking confirm dialog twice).
 */
export function checkDirtyClose<T extends { id: string; title: string }>(
  tabs: readonly T[],
  isDirty: (tab: T) => boolean,
  request: FileTabCloseRequest
): DirtyCloseCheck {
  if (request.kind === "one") {
    const tab = tabs.find((candidate) => candidate.id === request.tabId)
    return tab && isDirty(tab)
      ? { requiresConfirmation: true, dirtyTitle: tab.title }
      : { requiresConfirmation: false }
  }
  const affected =
    request.kind === "others"
      ? tabs.filter((candidate) => candidate.id !== request.keepTabId)
      : tabs
  return { requiresConfirmation: affected.some(isDirty) }
}

/**
 * After a bulk close of snapshotted ids, pick which tab should be active.
 *
 * - Prefer `preferredActiveId` when it still remains (dirty close-others
 *   must activate the keep tab, matching clean `closeOtherFileTabsNow`).
 * - Otherwise keep the current active id if it was not closed.
 * - If the active id was closed, fall back to the last remaining tab
 *   (or null when none remain).
 */
export function pickActiveAfterBulkClose(
  remainingIds: readonly string[],
  currentActiveId: string | null,
  closedIds: ReadonlySet<string> | readonly string[],
  preferredActiveId?: string | null
): string | null {
  const closed = closedIds instanceof Set ? closedIds : new Set(closedIds)
  if (preferredActiveId != null && remainingIds.includes(preferredActiveId)) {
    return preferredActiveId
  }
  if (currentActiveId == null || !closed.has(currentActiveId)) {
    return currentActiveId
  }
  if (remainingIds.length === 0) return null
  return remainingIds[remainingIds.length - 1] ?? null
}
