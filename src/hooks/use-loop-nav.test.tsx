import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it } from "vitest"

import { useLoopNav } from "./use-loop-nav"

beforeEach(() => {
  window.history.replaceState({}, "", "/workspace")
})

describe("useLoopNav", () => {
  it("reads the initial nav from the URL", () => {
    window.history.replaceState({}, "", "/workspace?loops=1&space=4&tab=inbox")
    const { result } = renderHook(() => useLoopNav())
    expect(result.current.nav.loops).toBe(true)
    expect(result.current.nav.space).toBe(4)
    expect(result.current.nav.tab).toBe("inbox")
  })

  it("openSpace writes the URL and updates the snapshot", () => {
    const { result } = renderHook(() => useLoopNav())
    act(() => result.current.toggleLoops())
    act(() => result.current.openSpace(2))
    expect(result.current.nav).toMatchObject({ loops: true, space: 2 })
    expect(window.location.search).toBe("?loops=1&space=2")
  })

  it("selectIssue clears a stale artifact + settings (cascade through the hook)", () => {
    const { result } = renderHook(() => useLoopNav())
    act(() => result.current.openSpace(2))
    act(() => result.current.selectIssue(7))
    act(() => result.current.openArtifact(12))
    act(() => result.current.openSettings())
    act(() => result.current.selectIssue(9))
    expect(result.current.nav).toMatchObject({
      space: 2,
      issue: 9,
      artifact: null,
      settings: false,
    })
  })

  it("syncs sibling hook instances through the custom event", () => {
    const a = renderHook(() => useLoopNav())
    const b = renderHook(() => useLoopNav())
    act(() => a.result.current.toggleLoops())
    expect(b.result.current.nav.loops).toBe(true)
  })
})
