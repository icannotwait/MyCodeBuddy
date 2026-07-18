import { describe, expect, it } from "vitest"

import {
  backgroundPositionFor,
  IDLE_FLOURISH_MAX_MS,
  IDLE_FLOURISH_MIN_MS,
  IDLE_FLOURISH_OPTIONS,
  PET_FRAME_DURATIONS_MS,
  PET_ONESHOT_KINDS,
  PET_ONESHOT_LOOPS,
  PET_STATE_ROW,
  SPRITE_BACKGROUND_SIZE,
  SPRITE_FRAME_HEIGHT,
  SPRITE_FRAME_WIDTH,
  SPRITE_GRID_COLS,
  SPRITE_GRID_ROWS,
  type PetState,
} from "@/lib/pet/animation"

describe("sprite frame dimensions", () => {
  it("derives frame size from the sheet and grid", () => {
    expect(SPRITE_FRAME_WIDTH).toBe(192)
    expect(SPRITE_FRAME_HEIGHT).toBe(208)
  })

  it("computes the CSS background-size from the grid", () => {
    expect(SPRITE_BACKGROUND_SIZE).toBe("800% 900%")
  })
})

describe("PET_STATE_ROW", () => {
  it("assigns each state a distinct row within the grid", () => {
    const rows = Object.values(PET_STATE_ROW)
    expect(new Set(rows).size).toBe(rows.length)
    for (const row of rows) {
      expect(row).toBeGreaterThanOrEqual(0)
      expect(row).toBeLessThan(SPRITE_GRID_ROWS)
    }
  })
})

describe("PET_FRAME_DURATIONS_MS", () => {
  it("has a duration entry for every state row", () => {
    for (const state of Object.keys(PET_STATE_ROW) as PetState[]) {
      expect(PET_FRAME_DURATIONS_MS[state].length).toBeGreaterThan(0)
    }
  })

  it("never declares more frames than the grid has columns", () => {
    for (const durations of Object.values(PET_FRAME_DURATIONS_MS)) {
      expect(durations.length).toBeLessThanOrEqual(SPRITE_GRID_COLS)
    }
  })

  it("uses positive durations only", () => {
    for (const durations of Object.values(PET_FRAME_DURATIONS_MS)) {
      for (const ms of durations) expect(ms).toBeGreaterThan(0)
    }
  })
})

describe("backgroundPositionFor", () => {
  it("places the first cell at the origin", () => {
    expect(backgroundPositionFor(0, 0)).toBe("0% 0%")
  })

  it("places the last cell at 100% 100%", () => {
    expect(
      backgroundPositionFor(SPRITE_GRID_ROWS - 1, SPRITE_GRID_COLS - 1)
    ).toBe("100% 100%")
  })

  it("interpolates intermediate cells", () => {
    // col 1 of 8 => 1/7, row 1 of 9 => 1/8
    expect(backgroundPositionFor(1, 1)).toBe(
      `${(1 / 7) * 100}% ${(1 / 8) * 100}%`
    )
  })
})

describe("idle flourish config", () => {
  it("has a sensible min/max window", () => {
    expect(IDLE_FLOURISH_MIN_MS).toBeLessThan(IDLE_FLOURISH_MAX_MS)
  })

  it("only offers known states as flourishes", () => {
    for (const state of IDLE_FLOURISH_OPTIONS) {
      expect(PET_STATE_ROW).toHaveProperty(state)
    }
  })
})

describe("one-shot animations", () => {
  it("defines a loop count for every one-shot kind", () => {
    for (const kind of PET_ONESHOT_KINDS) {
      expect(PET_ONESHOT_LOOPS[kind]).toBeGreaterThanOrEqual(1)
    }
  })

  it("one-shot kinds are valid pet states", () => {
    for (const kind of PET_ONESHOT_KINDS) {
      expect(PET_STATE_ROW).toHaveProperty(kind)
    }
  })
})
