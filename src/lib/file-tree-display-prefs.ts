"use client"

import { useCallback, useSyncExternalStore } from "react"

export const FILE_TREE_SHOW_IGNORED_STORAGE_KEY =
  "workspace:file-tree-show-ignored"
export const FILE_TREE_SHOW_IGNORED_CHANGED_EVENT =
  "codeg:file-tree-show-ignored-changed"

const SNAPSHOT_HYDRATING = 0
const SNAPSHOT_HIDDEN = 1
const SNAPSHOT_SHOWN = 2
type PreferenceSnapshot =
  | typeof SNAPSHOT_HYDRATING
  | typeof SNAPSHOT_HIDDEN
  | typeof SNAPSHOT_SHOWN

export function loadShowIgnoredFiles(): boolean {
  if (typeof window === "undefined") return false
  try {
    return (
      window.localStorage.getItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY) === "true"
    )
  } catch {
    return false
  }
}

export function saveShowIgnoredFiles(value: boolean): void {
  if (typeof window === "undefined") return
  try {
    window.localStorage.setItem(
      FILE_TREE_SHOW_IGNORED_STORAGE_KEY,
      String(value)
    )
  } catch {
    // Storage can be unavailable in restricted browser contexts.
  }
  window.dispatchEvent(
    new CustomEvent<boolean>(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, {
      detail: value,
    })
  )
}

function getPreferenceSnapshot(): PreferenceSnapshot {
  return loadShowIgnoredFiles() ? SNAPSHOT_SHOWN : SNAPSHOT_HIDDEN
}

function getServerSnapshot(): PreferenceSnapshot {
  return SNAPSHOT_HYDRATING
}

function subscribePreference(onStoreChange: () => void): () => void {
  const onChanged = () => onStoreChange()
  const onStorage = (event: StorageEvent) => {
    if (
      event.key !== null &&
      event.key !== FILE_TREE_SHOW_IGNORED_STORAGE_KEY
    ) {
      return
    }
    onStoreChange()
  }
  window.addEventListener(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, onChanged)
  window.addEventListener("storage", onStorage)
  return () => {
    window.removeEventListener(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, onChanged)
    window.removeEventListener("storage", onStorage)
  }
}

export function useShowIgnoredFiles(): [
  boolean,
  (value: boolean) => void,
  boolean,
] {
  const snapshot = useSyncExternalStore(
    subscribePreference,
    getPreferenceSnapshot,
    getServerSnapshot
  )
  const setShowIgnored = useCallback((value: boolean) => {
    saveShowIgnoredFiles(value)
  }, [])
  return [
    snapshot === SNAPSHOT_SHOWN,
    setShowIgnored,
    snapshot !== SNAPSHOT_HYDRATING,
  ]
}
