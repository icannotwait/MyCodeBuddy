"use client"

const FOLDER_EXPANDED_KEY = "workspace:sidebar-folder-expanded"
const SHOW_COMPLETED_KEY = "workspace:sidebar-show-completed"
const SORT_MODE_KEY = "workspace:sidebar-sort-mode"
const SECTION_COLLAPSED_KEY = "workspace:sidebar-section-collapsed"

export type SidebarSortMode = "created" | "updated"

/** Collapsed state of the two top-level sidebar sections. Absent key = expanded
 *  (the default), so a fresh user sees both sections open. */
export interface SidebarSectionCollapsed {
  pinned?: boolean
  folders?: boolean
  chats?: boolean
}

export function loadFolderExpanded(): Record<number, boolean> {
  if (typeof window === "undefined") return {}
  try {
    const raw = localStorage.getItem(FOLDER_EXPANDED_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== "object") return {}
    const result: Record<number, boolean> = {}
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      const id = Number(k)
      if (!Number.isNaN(id) && typeof v === "boolean") {
        result[id] = v
      }
    }
    return result
  } catch {
    return {}
  }
}

export function saveFolderExpanded(state: Record<number, boolean>): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(FOLDER_EXPANDED_KEY, JSON.stringify(state))
  } catch {
    /* ignore */
  }
}

export function loadShowCompleted(): boolean {
  if (typeof window === "undefined") return false
  try {
    const raw = localStorage.getItem(SHOW_COMPLETED_KEY)
    if (raw === "true") return true
  } catch {
    /* ignore */
  }
  return false
}

export function saveShowCompleted(value: boolean): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(SHOW_COMPLETED_KEY, String(value))
  } catch {
    /* ignore */
  }
}

export function loadSortMode(): SidebarSortMode {
  if (typeof window === "undefined") return "created"
  try {
    const raw = localStorage.getItem(SORT_MODE_KEY)
    if (raw === "updated" || raw === "created") return raw
  } catch {
    /* ignore */
  }
  return "created"
}

export function saveSortMode(value: SidebarSortMode): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(SORT_MODE_KEY, value)
  } catch {
    /* ignore */
  }
}

export function loadSectionCollapsed(): SidebarSectionCollapsed {
  if (typeof window === "undefined") return {}
  try {
    const raw = localStorage.getItem(SECTION_COLLAPSED_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== "object") return {}
    const obj = parsed as Record<string, unknown>
    const result: SidebarSectionCollapsed = {}
    if (typeof obj.pinned === "boolean") result.pinned = obj.pinned
    if (typeof obj.folders === "boolean") result.folders = obj.folders
    if (typeof obj.chats === "boolean") result.chats = obj.chats
    return result
  } catch {
    return {}
  }
}

export function saveSectionCollapsed(state: SidebarSectionCollapsed): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(SECTION_COLLAPSED_KEY, JSON.stringify(state))
  } catch {
    /* ignore */
  }
}
