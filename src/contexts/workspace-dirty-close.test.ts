import { describe, expect, it } from "vitest"
import {
  checkDirtyClose,
  pickActiveAfterBulkClose,
} from "./workspace-dirty-close"

interface FakeTab {
  id: string
  title: string
  dirty: boolean
}

const tabs: FakeTab[] = [
  { id: "a", title: "alpha", dirty: true },
  { id: "b", title: "beta", dirty: false },
  { id: "c", title: "gamma", dirty: true },
]

const isDirty = (tab: FakeTab) => tab.dirty

describe("checkDirtyClose", () => {
  it("requires confirmation with the title for a dirty single tab", () => {
    expect(checkDirtyClose(tabs, isDirty, { kind: "one", tabId: "a" })).toEqual(
      { requiresConfirmation: true, dirtyTitle: "alpha" }
    )
  })

  it("closes a clean single tab without confirmation", () => {
    expect(checkDirtyClose(tabs, isDirty, { kind: "one", tabId: "b" })).toEqual(
      { requiresConfirmation: false }
    )
  })

  it("closes a missing tab without confirmation", () => {
    expect(
      checkDirtyClose(tabs, isDirty, { kind: "one", tabId: "zzz" })
    ).toEqual({ requiresConfirmation: false })
  })

  it("requires confirmation for close-others when any closing tab is dirty", () => {
    expect(
      checkDirtyClose(tabs, isDirty, { kind: "others", keepTabId: "a" })
    ).toEqual({ requiresConfirmation: true })
  })

  it("skips confirmation for close-others when closing tabs are all clean", () => {
    // Pure function still reports confirmation if any non-kept tab is dirty.
    // The provider short-circuits when keepTabId is missing (no-op, no dialog).
    expect(
      checkDirtyClose(tabs, isDirty, { kind: "others", keepTabId: "zzz" })
    ).toEqual({ requiresConfirmation: true })
    expect(
      checkDirtyClose(
        tabs.filter((t) => !t.dirty),
        isDirty,
        { kind: "others", keepTabId: "a" }
      )
    ).toEqual({ requiresConfirmation: false })
  })

  it("requires confirmation for close-all when any tab is dirty", () => {
    expect(checkDirtyClose(tabs, isDirty, { kind: "all" })).toEqual({
      requiresConfirmation: true,
    })
    expect(
      checkDirtyClose(
        tabs.filter((t) => !t.dirty),
        isDirty,
        { kind: "all" }
      )
    ).toEqual({ requiresConfirmation: false })
  })
})

describe("pickActiveAfterBulkClose", () => {
  it("activates preferred keep tab after close-others (not last remaining)", () => {
    // remaining [keep, later]; active closed was "later". Without preferred
    // the fallback is last remaining ("later"), but close-others must keep.
    expect(
      pickActiveAfterBulkClose(
        ["keep", "later"],
        "later",
        ["a", "b", "later"],
        "keep"
      )
    ).toBe("keep")
    // Keep already first / only survivor — still preferred.
    expect(
      pickActiveAfterBulkClose(["keep"], "c", ["a", "b", "c"], "keep")
    ).toBe("keep")
  })

  it("falls back to last remaining when preferred is missing", () => {
    expect(pickActiveAfterBulkClose(["a", "b"], "c", ["c"], "zzz")).toBe("b")
  })

  it("keeps current active when it was not closed and no preferred", () => {
    expect(pickActiveAfterBulkClose(["a", "b"], "a", ["b"])).toBe("a")
  })

  it("returns null when nothing remains", () => {
    expect(pickActiveAfterBulkClose([], "a", ["a", "b"])).toBe(null)
  })
})
