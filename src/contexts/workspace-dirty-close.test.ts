import { describe, expect, it } from "vitest"
import { checkDirtyClose } from "./workspace-dirty-close"

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
