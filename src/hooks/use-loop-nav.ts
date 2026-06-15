"use client"

import { useCallback, useMemo, useSyncExternalStore } from "react"

import {
  loopNavToSearch,
  navCloseArtifact,
  navCloseSpace,
  navGotoIssue,
  navOpenArtifact,
  navOpenSpace,
  navSelectIssue,
  navSetLoops,
  navSetSettings,
  navSetTab,
  parseLoopNav,
  type LoopNav,
  type LoopTab,
} from "@/lib/loop-nav"

// Setters fire this so sibling hook instances re-read the URL (replaceState
// does not emit popstate). popstate covers browser back/forward.
const NAV_EVENT = "codeg:loop-nav"

function subscribe(cb: () => void): () => void {
  if (typeof window === "undefined") return () => {}
  window.addEventListener("popstate", cb)
  window.addEventListener(NAV_EVENT, cb)
  return () => {
    window.removeEventListener("popstate", cb)
    window.removeEventListener(NAV_EVENT, cb)
  }
}

// The raw search string is the external snapshot: identical strings compare
// equal under Object.is, so useSyncExternalStore only re-renders on real change.
function getSnapshot(): string {
  return typeof window === "undefined" ? "" : window.location.search
}
function getServerSnapshot(): string {
  return ""
}

/**
 * Read/write loop navigation through the `/workspace` query string — the single
 * source of truth, so navigation survives refresh and is shareable. Writes go
 * through the pure transitions in `loop-nav.ts` (which own the cascade
 * invariants), exposed here as semantic actions; there is deliberately no raw
 * field-patch setter, so no call site can leave a stale child param behind.
 * `replaceState` keeps the workbench out of the history stack (it's one "place"
 * in the tabbed workspace); making browser-Back close the artifact/settings
 * overlay later is a one-line `pushState` change inside that single action.
 */
export function useLoopNav() {
  const search = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot)
  const nav = useMemo(() => parseLoopNav(search), [search])

  // Apply a pure transition: read the live URL, transform, write + notify.
  const apply = useCallback((fn: (n: LoopNav) => LoopNav) => {
    if (typeof window === "undefined") return
    const next = fn(parseLoopNav(window.location.search))
    const nextSearch = loopNavToSearch(next, window.location.search)
    const url = `${window.location.pathname}${nextSearch}${window.location.hash}`
    window.history.replaceState(window.history.state, "", url)
    window.dispatchEvent(new Event(NAV_EVENT))
  }, [])

  // Stable across renders (depend only on `apply`), so consumers can list any
  // action in an effect's deps without re-subscribing.
  const actions = useMemo(
    () => ({
      toggleLoops: () => apply((n) => navSetLoops(n, !n.loops)),
      exitLoops: () => apply((n) => navSetLoops(n, false)),
      openSpace: (id: number) => apply((n) => navOpenSpace(n, id)),
      closeSpace: () => apply(navCloseSpace),
      selectIssue: (id: number | null) => apply((n) => navSelectIssue(n, id)),
      gotoIssue: (spaceId: number, issueId: number) =>
        apply((n) => navGotoIssue(n, spaceId, issueId)),
      setTab: (tab: LoopTab) => apply((n) => navSetTab(n, tab)),
      openArtifact: (id: number) => apply((n) => navOpenArtifact(n, id)),
      closeArtifact: () => apply(navCloseArtifact),
      openSettings: () => apply((n) => navSetSettings(n, true)),
      closeSettings: () => apply((n) => navSetSettings(n, false)),
    }),
    [apply]
  )

  return { nav, ...actions }
}
