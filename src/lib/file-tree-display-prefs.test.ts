import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import {
  FILE_TREE_SHOW_IGNORED_CHANGED_EVENT,
  FILE_TREE_SHOW_IGNORED_STORAGE_KEY,
  loadShowIgnoredFiles,
  saveShowIgnoredFiles,
  useShowIgnoredFiles,
} from "./file-tree-display-prefs"

describe("file-tree display preferences", () => {
  beforeEach(() => {
    localStorage.clear()
    vi.restoreAllMocks()
  })

  it("defaults off and accepts only explicit boolean strings", () => {
    expect(loadShowIgnoredFiles()).toBe(false)

    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    expect(loadShowIgnoredFiles()).toBe(true)
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "false")
    expect(loadShowIgnoredFiles()).toBe(false)
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "1")
    expect(loadShowIgnoredFiles()).toBe(false)
    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "TRUE")
    expect(loadShowIgnoredFiles()).toBe(false)
  })

  it("persists both values and emits after an attempted save", () => {
    const listener = vi.fn()
    window.addEventListener(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, listener)

    saveShowIgnoredFiles(true)
    expect(localStorage.getItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY)).toBe(
      "true"
    )
    saveShowIgnoredFiles(false)
    expect(localStorage.getItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY)).toBe(
      "false"
    )
    expect(listener).toHaveBeenCalledTimes(2)

    window.removeEventListener(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, listener)
  })

  it("tolerates unavailable storage and still notifies listeners", () => {
    const listener = vi.fn()
    window.addEventListener(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, listener)
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked")
    })
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("blocked")
    })

    expect(loadShowIgnoredFiles()).toBe(false)
    expect(() => saveShowIgnoredFiles(true)).not.toThrow()
    expect(listener).toHaveBeenCalledTimes(1)

    window.removeEventListener(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, listener)
  })

  it("hydrates and synchronizes same-window consumers", async () => {
    const first = renderHook(() => useShowIgnoredFiles())
    const second = renderHook(() => useShowIgnoredFiles())

    await waitFor(() => expect(first.result.current[2]).toBe(true))
    expect(first.result.current[0]).toBe(false)

    act(() => first.result.current[1](true))
    expect(localStorage.getItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY)).toBe(
      "true"
    )
    await waitFor(() => expect(second.result.current[0]).toBe(true))

    act(() => second.result.current[1](false))
    await waitFor(() => expect(first.result.current[0]).toBe(false))
  })

  it("reloads storage for custom and cross-window events", async () => {
    const { result } = renderHook(() => useShowIgnoredFiles())
    await waitFor(() => expect(result.current[2]).toBe(true))

    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "true")
    act(() => {
      window.dispatchEvent(
        new CustomEvent(FILE_TREE_SHOW_IGNORED_CHANGED_EVENT, {
          detail: false,
        })
      )
    })
    expect(result.current[0]).toBe(true)

    localStorage.setItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY, "false")
    act(() => {
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: FILE_TREE_SHOW_IGNORED_STORAGE_KEY,
          newValue: "false",
        })
      )
    })
    expect(result.current[0]).toBe(false)
  })
})
