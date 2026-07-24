import { describe, expect, it } from "vitest"
import type { TerminalTab } from "@/contexts/terminal-context"
import { findLiveCloseTargets } from "./terminal-close-guard"

function tab(id: string): TerminalTab {
  return { id, folderId: 1, title: `Terminal ${id}`, workingDir: "/w" }
}

const tabs = [tab("a"), tab("b"), tab("c")]

describe("findLiveCloseTargets", () => {
  it("returns the single tab when it is live", () => {
    expect(
      findLiveCloseTargets(tabs, new Set(), { kind: "one", tabId: "a" })
    ).toEqual([tabs[0]])
  })

  it("returns nothing for an exited tab", () => {
    expect(
      findLiveCloseTargets(tabs, new Set(["a"]), { kind: "one", tabId: "a" })
    ).toEqual([])
  })

  it("returns nothing for a missing tab", () => {
    expect(
      findLiveCloseTargets(tabs, new Set(), { kind: "one", tabId: "zzz" })
    ).toEqual([])
  })

  it("targets all but the kept tab for close-others, skipping exited", () => {
    expect(
      findLiveCloseTargets(tabs, new Set(["b"]), {
        kind: "others",
        keepTabId: "a",
      })
    ).toEqual([tabs[2]])
  })

  it("targets every live tab for close-all", () => {
    expect(
      findLiveCloseTargets(tabs, new Set(["a", "c"]), { kind: "all" })
    ).toEqual([tabs[1]])
  })
})
