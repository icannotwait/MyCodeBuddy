import { beforeEach, describe, expect, it } from "vitest"
import {
  loadTasksStatusFilter,
  loadTasksViewMode,
  saveTasksStatusFilter,
  saveTasksViewMode,
} from "./tasks-board-filter-storage"

const VIEW_MODE_KEY = "workspace:tasks-view-mode"
const STATUS_FILTER_KEY = "workspace:tasks-status-filter"

beforeEach(() => {
  localStorage.clear()
})

describe("tasks view mode storage", () => {
  it("round-trips a stored mode and defaults to the board", () => {
    expect(loadTasksViewMode()).toBe("board")
    saveTasksViewMode("list")
    expect(loadTasksViewMode()).toBe("list")
    saveTasksViewMode("board")
    expect(loadTasksViewMode()).toBe("board")
  })

  it("falls back to the board on a junk entry", () => {
    localStorage.setItem(VIEW_MODE_KEY, "gallery")
    expect(loadTasksViewMode()).toBe("board")
  })
})

describe("tasks status filter storage", () => {
  it("round-trips a group and keeps null meaning every status", () => {
    expect(loadTasksStatusFilter()).toBeNull()
    saveTasksStatusFilter("attention")
    expect(loadTasksStatusFilter()).toBe("attention")
    // Clearing removes the entry rather than storing something that would read
    // back as a filter nobody chose.
    saveTasksStatusFilter(null)
    expect(localStorage.getItem(STATUS_FILTER_KEY)).toBeNull()
    expect(loadTasksStatusFilter()).toBeNull()
  })

  it("falls back to every status on an unknown entry — including the old per-status selection", () => {
    // What this key held while the filter was a checkbox menu. It matches no
    // group, so it degrades to "every status" and the next save overwrites it.
    localStorage.setItem(
      STATUS_FILTER_KEY,
      JSON.stringify(["review", "failed"])
    )
    expect(loadTasksStatusFilter()).toBeNull()

    // A single status id is not a group either — `review` lives in `attention`.
    localStorage.setItem(STATUS_FILTER_KEY, "review")
    expect(loadTasksStatusFilter()).toBeNull()

    localStorage.setItem(STATUS_FILTER_KEY, "{not json")
    expect(loadTasksStatusFilter()).toBeNull()
  })
})
