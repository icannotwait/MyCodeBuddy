import { describe, it, expect } from "vitest"
import {
  DEFAULT_LOOP_NAV,
  loopNavToSearch,
  navCloseSpace,
  navGotoIssue,
  navOpenSpace,
  navSelectIssue,
  navSetTab,
  parseLoopNav,
  type LoopNav,
} from "./loop-nav"

describe("loop-nav model", () => {
  it("parses a full nav from search", () => {
    // A fully-populated, invariant-consistent nav: settings only coexists with
    // an issue on the issues tab (non-default tab parsing is covered by the
    // round-trip + normalization tests below).
    const nav = parseLoopNav(
      "?loops=1&space=3&issue=7&tab=issues&artifact=12&settings=1"
    )
    expect(nav).toEqual<LoopNav>({
      loops: true,
      space: 3,
      issue: 7,
      tab: "issues",
      artifact: 12,
      settings: true,
    })
  })

  it("falls back to defaults for absent/invalid params", () => {
    expect(parseLoopNav("")).toEqual(DEFAULT_LOOP_NAV)
    expect(parseLoopNav("?space=0&issue=-1&tab=nope&artifact=x")).toEqual(
      DEFAULT_LOOP_NAV
    )
  })

  it("round-trips a nav through search", () => {
    const nav: LoopNav = {
      loops: true,
      space: 5,
      issue: null,
      tab: "inbox",
      artifact: null,
      settings: false,
    }
    expect(parseLoopNav(loopNavToSearch(nav, ""))).toEqual(nav)
  })

  it("omits defaults to keep the URL clean", () => {
    expect(loopNavToSearch(DEFAULT_LOOP_NAV, "")).toBe("")
    expect(loopNavToSearch({ ...DEFAULT_LOOP_NAV, loops: true }, "")).toBe(
      "?loops=1"
    )
  })

  it("preserves foreign params and clears stale loop params", () => {
    const out = loopNavToSearch(
      { ...DEFAULT_LOOP_NAV, loops: true, space: 2 },
      "?theme=dark&space=99&tab=memory"
    )
    const p = new URLSearchParams(out)
    expect(p.get("theme")).toBe("dark")
    expect(p.get("space")).toBe("2")
    expect(p.get("tab")).toBeNull() // default tab dropped
  })

  it("normalizes impossible direct-URL combinations on parse", () => {
    // issue / artifact without a space → dropped
    expect(parseLoopNav("?loops=1&issue=7")).toMatchObject({
      space: null,
      issue: null,
    })
    expect(parseLoopNav("?loops=1&artifact=5").artifact).toBeNull()
    // settings only survives on the issues tab WITH an issue selected
    expect(parseLoopNav("?loops=1&space=2&tab=inbox&settings=1").settings).toBe(
      false
    )
    expect(
      parseLoopNav("?loops=1&space=2&tab=issues&settings=1").settings
    ).toBe(false) // no issue
    expect(
      parseLoopNav("?loops=1&space=2&issue=7&tab=issues&settings=1").settings
    ).toBe(true)
  })
})

describe("loop-nav transitions (cascade invariants)", () => {
  const seeded: LoopNav = {
    loops: true,
    space: 2,
    issue: 7,
    tab: "issues",
    artifact: 12,
    settings: true,
  }

  it("opening a different space clears issue/artifact/settings", () => {
    expect(navOpenSpace(seeded, 5)).toEqual({
      ...seeded,
      space: 5,
      issue: null,
      artifact: null,
      settings: false,
    })
  })

  it("re-opening the same space is a no-op (keeps children)", () => {
    expect(navOpenSpace(seeded, 2)).toBe(seeded)
  })

  it("closing a space clears all children", () => {
    expect(navCloseSpace(seeded)).toEqual({
      ...seeded,
      space: null,
      issue: null,
      artifact: null,
      settings: false,
    })
  })

  it("changing the issue clears the artifact + settings", () => {
    expect(navSelectIssue(seeded, 9)).toEqual({
      ...seeded,
      issue: 9,
      artifact: null,
      settings: false,
    })
  })

  it("gotoIssue jumps cross-space and resets children", () => {
    expect(navGotoIssue(DEFAULT_LOOP_NAV, 3, 8)).toEqual({
      loops: true,
      space: 3,
      issue: 8,
      tab: "issues",
      artifact: null,
      settings: false,
    })
  })

  it("leaving the issues tab drops the settings flag", () => {
    expect(navSetTab(seeded, "inbox").settings).toBe(false)
    expect(navSetTab(seeded, "issues").settings).toBe(true)
  })
})
