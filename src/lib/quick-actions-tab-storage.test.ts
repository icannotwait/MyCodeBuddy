import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  loadQuickActionsTab,
  saveQuickActionsTab,
} from "@/lib/quick-actions-tab-storage"

const KEY = "workspace:quick-actions-tab"

describe("quick-actions-tab-storage", () => {
  beforeEach(() => {
    localStorage.clear()
  })

  afterEach(() => {
    localStorage.clear()
    vi.restoreAllMocks()
  })

  it("defaults to 'coding' when nothing is stored", () => {
    expect(loadQuickActionsTab()).toBe("coding")
  })

  it.each(["office", "coding", "research"] as const)(
    "round-trips the %s tab",
    (tab) => {
      saveQuickActionsTab(tab)
      expect(loadQuickActionsTab()).toBe(tab)
    }
  )

  it("falls back to 'coding' for a polluted value", () => {
    localStorage.setItem(KEY, "garbage")
    expect(loadQuickActionsTab()).toBe("coding")
  })

  it("swallows storage read failures and returns the default", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked")
    })
    expect(loadQuickActionsTab()).toBe("coding")
  })

  it("swallows storage write failures", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota")
    })
    expect(() => saveQuickActionsTab("research")).not.toThrow()
  })
})
