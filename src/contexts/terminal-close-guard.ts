import type { TerminalTab } from "@/contexts/terminal-context"

/** Which terminals a close gesture targets. */
export type TerminalCloseRequest =
  | { kind: "one"; tabId: string }
  | { kind: "others"; keepTabId: string }
  | { kind: "all" }

/**
 * Snapshot state for the provider-owned confirm dialog. `title` / `liveCount`
 * are captured when the dialog opens so the copy can't shift under the user
 * while they decide.
 */
export type PendingTerminalClose =
  | { kind: "one"; tabId: string; title: string }
  | {
      kind: "others"
      keepTabId: string
      liveCount: number
      /**
       * ALL affected tab ids (live + already-exited) snapshotted when the
       * dialog opened. Confirm closes every id here so exited tabs don't
       * linger after a bulk close. `liveCount` is dialog copy only.
       */
      targetIds: string[]
    }
  | {
      kind: "all"
      liveCount: number
      /** ALL affected tab ids (live + exited); see `others` note above. */
      targetIds: string[]
    }

/**
 * Tabs that `request` would kill AND whose process is still live (a tab is
 * live until TerminalView reports onProcessExited). Empty result → close
 * immediately without confirmation.
 */
export function findLiveCloseTargets(
  tabs: TerminalTab[],
  exitedIds: ReadonlySet<string>,
  request: TerminalCloseRequest
): TerminalTab[] {
  const affected =
    request.kind === "one"
      ? tabs.filter((tab) => tab.id === request.tabId)
      : request.kind === "others"
        ? tabs.filter((tab) => tab.id !== request.keepTabId)
        : tabs
  return affected.filter((tab) => !exitedIds.has(tab.id))
}
