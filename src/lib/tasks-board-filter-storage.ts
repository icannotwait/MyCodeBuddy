"use client"

const BOARD_FILTER_KEY = "workspace:tasks-board-filter"
const VIEW_MODE_KEY = "workspace:tasks-view-mode"
const STATUS_FILTER_KEY = "workspace:tasks-status-filter"

/** Visibility toggles of the tasks board's filter popover. */
export interface TasksBoardFilter {
  showCanceled: boolean
  showArchived: boolean
}

/** Default view: canceled tasks visible, archived ones hidden. */
export const DEFAULT_TASKS_BOARD_FILTER: TasksBoardFilter = {
  showCanceled: true,
  showArchived: false,
}

/** How the tasks page lays its rows out: four-column board, or flat list. */
export type TasksViewMode = "board" | "list"

export const DEFAULT_TASKS_VIEW_MODE: TasksViewMode = "board"

/** The groups the list view can filter on — the board's four columns. Mirrors
 *  `BOARD_COLUMN_IDS` (board-columns.ts), whose test asserts the two agree —
 *  this copy exists so the storage layer stays free of component imports. */
export const TASKS_STATUS_GROUPS = [
  "todo",
  "inProgress",
  "attention",
  "done",
] as const

export type TasksStatusGroup = (typeof TASKS_STATUS_GROUPS)[number]

export function loadTasksBoardFilter(): TasksBoardFilter {
  if (typeof window === "undefined") return DEFAULT_TASKS_BOARD_FILTER
  try {
    const raw = localStorage.getItem(BOARD_FILTER_KEY)
    if (!raw) return DEFAULT_TASKS_BOARD_FILTER
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== "object") return DEFAULT_TASKS_BOARD_FILTER
    const obj = parsed as Record<string, unknown>
    return {
      showCanceled:
        typeof obj.showCanceled === "boolean"
          ? obj.showCanceled
          : DEFAULT_TASKS_BOARD_FILTER.showCanceled,
      showArchived:
        typeof obj.showArchived === "boolean"
          ? obj.showArchived
          : DEFAULT_TASKS_BOARD_FILTER.showArchived,
    }
  } catch {
    return DEFAULT_TASKS_BOARD_FILTER
  }
}

export function saveTasksBoardFilter(filter: TasksBoardFilter): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(BOARD_FILTER_KEY, JSON.stringify(filter))
  } catch {
    /* ignore */
  }
}

export function loadTasksViewMode(): TasksViewMode {
  if (typeof window === "undefined") return DEFAULT_TASKS_VIEW_MODE
  try {
    const raw = localStorage.getItem(VIEW_MODE_KEY)
    return raw === "list" || raw === "board" ? raw : DEFAULT_TASKS_VIEW_MODE
  } catch {
    return DEFAULT_TASKS_VIEW_MODE
  }
}

export function saveTasksViewMode(mode: TasksViewMode): void {
  if (typeof window === "undefined") return
  try {
    localStorage.setItem(VIEW_MODE_KEY, mode)
  } catch {
    /* ignore */
  }
}

/**
 * The list view's status selection. `null` means "every status" — the default,
 * and what anything unrecognized falls back to rather than an empty list.
 *
 * Stored as the bare group id (like the view mode above). That also migrates
 * the entry this key used to hold — a JSON array of individual statuses, from
 * when the filter was a checkbox menu: it matches no group, so it reads back as
 * "every status" and the next save overwrites it.
 */
export function loadTasksStatusFilter(): TasksStatusGroup | null {
  if (typeof window === "undefined") return null
  try {
    const raw = localStorage.getItem(STATUS_FILTER_KEY)
    return TASKS_STATUS_GROUPS.find((group) => group === raw) ?? null
  } catch {
    return null
  }
}

export function saveTasksStatusFilter(group: TasksStatusGroup | null): void {
  if (typeof window === "undefined") return
  try {
    if (group == null) localStorage.removeItem(STATUS_FILTER_KEY)
    else localStorage.setItem(STATUS_FILTER_KEY, group)
  } catch {
    /* ignore */
  }
}
