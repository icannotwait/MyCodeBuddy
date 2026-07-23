import { beforeEach, describe, expect, it } from "vitest"
import {
  clearDelegatedChildTabIntent,
  consumeDelegatedChildTabIntent,
  peekDelegatedChildTabIntent,
  resetDelegatedChildTabIntents,
  setDelegatedChildTabIntent,
} from "@/lib/delegated-child-tab-intent"

describe("delegated-child-tab-intent", () => {
  beforeEach(() => {
    resetDelegatedChildTabIntents()
  })

  it("stores and peeks intent", () => {
    setDelegatedChildTabIntent(42, {
      focusTurnAnchor: "turn-1",
      kickoffTask: "do the work",
      liveOwnsActiveTurn: true,
    })
    expect(peekDelegatedChildTabIntent(42)).toEqual({
      focusTurnAnchor: "turn-1",
      kickoffTask: "do the work",
      liveOwnsActiveTurn: true,
    })
  })

  it("consume is one-shot", () => {
    setDelegatedChildTabIntent(7, {
      focusTurnAnchor: "a",
      kickoffTask: null,
      liveOwnsActiveTurn: true,
    })
    expect(consumeDelegatedChildTabIntent(7)?.focusTurnAnchor).toBe("a")
    expect(consumeDelegatedChildTabIntent(7)).toBeNull()
    expect(peekDelegatedChildTabIntent(7)).toBeNull()
  })

  it("clear removes without requiring consume", () => {
    setDelegatedChildTabIntent(3, {
      focusTurnAnchor: null,
      kickoffTask: "x",
      liveOwnsActiveTurn: false,
    })
    clearDelegatedChildTabIntent(3)
    expect(peekDelegatedChildTabIntent(3)).toBeNull()
  })

  it("ignores non-positive conversation ids", () => {
    setDelegatedChildTabIntent(0, {
      focusTurnAnchor: "x",
      kickoffTask: null,
      liveOwnsActiveTurn: true,
    })
    expect(peekDelegatedChildTabIntent(0)).toBeNull()
  })
})
