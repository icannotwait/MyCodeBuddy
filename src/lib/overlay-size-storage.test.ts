import { beforeEach, describe, expect, it } from "vitest"

import {
  DEFAULT_OVERLAY_MAX_HEIGHT,
  DEFAULT_OVERLAY_WIDTH,
  MAX_OVERLAY_MAX_HEIGHT,
  MAX_OVERLAY_WIDTH,
  MIN_OVERLAY_MAX_HEIGHT,
  MIN_OVERLAY_WIDTH,
  clampOverlayMaxHeight,
  clampOverlayWidth,
  loadOverlaySize,
  nextOverlayMaxHeight,
  saveOverlaySize,
} from "./overlay-size-storage"

const KEY = "test:sub-agent-overlay-size"

describe("overlay-size-storage", () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it("clamps width and maxHeight to configured ranges", () => {
    expect(clampOverlayWidth(10)).toBe(MIN_OVERLAY_WIDTH)
    expect(clampOverlayWidth(9999)).toBe(MAX_OVERLAY_WIDTH)
    expect(clampOverlayMaxHeight(1)).toBe(MIN_OVERLAY_MAX_HEIGHT)
    expect(clampOverlayMaxHeight(9999)).toBe(MAX_OVERLAY_MAX_HEIGHT)
  })

  it("returns defaults when storage is empty or invalid", () => {
    expect(loadOverlaySize(KEY)).toEqual({
      width: DEFAULT_OVERLAY_WIDTH,
      maxHeight: DEFAULT_OVERLAY_MAX_HEIGHT,
    })
    localStorage.setItem(KEY, "{not json")
    expect(loadOverlaySize(KEY)).toEqual({
      width: DEFAULT_OVERLAY_WIDTH,
      maxHeight: DEFAULT_OVERLAY_MAX_HEIGHT,
    })
    localStorage.setItem(KEY, JSON.stringify({ width: 300 }))
    expect(loadOverlaySize(KEY)).toEqual({
      width: DEFAULT_OVERLAY_WIDTH,
      maxHeight: DEFAULT_OVERLAY_MAX_HEIGHT,
    })
  })

  it("round-trips a valid size through localStorage", () => {
    saveOverlaySize(KEY, { width: 360, maxHeight: 420 })
    expect(loadOverlaySize(KEY)).toEqual({ width: 360, maxHeight: 420 })
  })

  it("refuses to grow maxHeight when content is shorter than the cap", () => {
    expect(
      nextOverlayMaxHeight({
        startMaxHeight: 384,
        deltaY: 80,
        contentHeight: 200,
      })
    ).toBe(384)
  })

  it("allows growth only up to content height when content hits the cap", () => {
    expect(
      nextOverlayMaxHeight({
        startMaxHeight: 300,
        deltaY: 120,
        contentHeight: 360,
      })
    ).toBe(360)

    expect(
      nextOverlayMaxHeight({
        startMaxHeight: 300,
        deltaY: 40,
        contentHeight: 500,
      })
    ).toBe(340)
  })

  it("always allows shrinking maxHeight down to the minimum", () => {
    expect(
      nextOverlayMaxHeight({
        startMaxHeight: 384,
        deltaY: -200,
        contentHeight: 100,
      })
    ).toBe(184)

    expect(
      nextOverlayMaxHeight({
        startMaxHeight: 384,
        deltaY: -400,
        contentHeight: 50,
      })
    ).toBe(MIN_OVERLAY_MAX_HEIGHT)
  })
})
