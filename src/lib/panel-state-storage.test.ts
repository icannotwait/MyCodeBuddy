import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  loadPersistedPanelState,
  savePersistedPanelState,
} from "@/lib/panel-state-storage"

const KEY = "test:panel"

describe("panel-state-storage", () => {
  beforeEach(() => {
    localStorage.clear()
  })

  afterEach(() => {
    localStorage.clear()
    vi.restoreAllMocks()
  })

  it("round-trips a valid panel state", () => {
    savePersistedPanelState(KEY, { isOpen: true, width: 320 })
    expect(loadPersistedPanelState(KEY)).toEqual({ isOpen: true, width: 320 })
  })

  it("returns null when nothing is stored", () => {
    expect(loadPersistedPanelState(KEY)).toBeNull()
  })

  it("returns null for malformed JSON", () => {
    localStorage.setItem(KEY, "{not json")
    expect(loadPersistedPanelState(KEY)).toBeNull()
  })

  it("rejects a payload with a non-boolean isOpen", () => {
    localStorage.setItem(KEY, JSON.stringify({ isOpen: "yes", width: 10 }))
    expect(loadPersistedPanelState(KEY)).toBeNull()
  })

  it("rejects a payload with a non-numeric width", () => {
    localStorage.setItem(KEY, JSON.stringify({ isOpen: true, width: "wide" }))
    expect(loadPersistedPanelState(KEY)).toBeNull()
  })

  it("rejects a NaN width", () => {
    localStorage.setItem(KEY, JSON.stringify({ isOpen: true, width: null }))
    expect(loadPersistedPanelState(KEY)).toBeNull()
  })

  it("swallows storage write failures", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded")
    })
    expect(() =>
      savePersistedPanelState(KEY, { isOpen: false, width: 0 })
    ).not.toThrow()
  })
})
