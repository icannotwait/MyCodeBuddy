import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  clearLastActiveContext,
  loadLastActiveContext,
  saveLastActiveContext,
} from "@/lib/last-active-context-storage"

describe("last-active-context-storage", () => {
  beforeEach(() => {
    localStorage.clear()
  })

  afterEach(() => {
    localStorage.clear()
    vi.restoreAllMocks()
  })

  it("round-trips a folder context", () => {
    saveLastActiveContext({ folderId: 7, isChat: false })
    expect(loadLastActiveContext()).toEqual({ folderId: 7, isChat: false })
  })

  it("round-trips a chat context (folderId 0)", () => {
    saveLastActiveContext({ folderId: 0, isChat: true })
    expect(loadLastActiveContext()).toEqual({ folderId: 0, isChat: true })
  })

  it("returns null when nothing is stored", () => {
    expect(loadLastActiveContext()).toBeNull()
  })

  it("clears the stored context", () => {
    saveLastActiveContext({ folderId: 3, isChat: false })
    clearLastActiveContext()
    expect(loadLastActiveContext()).toBeNull()
  })

  it("returns null for malformed JSON", () => {
    localStorage.setItem("codeg:last-active-context:v1", "not json")
    expect(loadLastActiveContext()).toBeNull()
  })

  it("rejects a non-object payload", () => {
    localStorage.setItem("codeg:last-active-context:v1", JSON.stringify(42))
    expect(loadLastActiveContext()).toBeNull()
  })

  it("rejects a payload missing folderId", () => {
    localStorage.setItem(
      "codeg:last-active-context:v1",
      JSON.stringify({ isChat: true })
    )
    expect(loadLastActiveContext()).toBeNull()
  })

  it("rejects a payload with non-boolean isChat", () => {
    localStorage.setItem(
      "codeg:last-active-context:v1",
      JSON.stringify({ folderId: 1, isChat: "yes" })
    )
    expect(loadLastActiveContext()).toBeNull()
  })

  it("swallows storage write failures", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("blocked")
    })
    expect(() =>
      saveLastActiveContext({ folderId: 1, isChat: false })
    ).not.toThrow()
  })
})
