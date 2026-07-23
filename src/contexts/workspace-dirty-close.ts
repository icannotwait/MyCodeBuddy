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
