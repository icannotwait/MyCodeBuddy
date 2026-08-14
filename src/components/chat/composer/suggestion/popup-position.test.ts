import { describe, expect, it } from "vitest"

import { placeAnchoredPopup } from "./popup-position"

const SIZE = { width: 320, height: 288 }
const VIEWPORT = { width: 1280, height: 800 }
// Defaults the helper uses: margin=8, gap=4.

describe("placeAnchoredPopup", () => {
  it("anchors the left edge at the caret when there is room", () => {
    const pos = placeAnchoredPopup(
      { left: 100, top: 600, bottom: 620 },
      SIZE,
      VIEWPORT
    )
    expect(pos.left).toBe(100)
  })

  it("clamps the left edge so the panel stays inside the right margin", () => {
    // caret far right: 1200 + 320 would overflow the 1280 viewport.
    const pos = placeAnchoredPopup(
      { left: 1200, top: 600, bottom: 620 },
      SIZE,
      VIEWPORT
    )
    // max left = 1280 - 320 - 8 = 952
    expect(pos.left).toBe(952)
  })

  it("clamps the left edge to the left margin for a negative caret x", () => {
    const pos = placeAnchoredPopup(
      { left: -50, top: 600, bottom: 620 },
      SIZE,
      VIEWPORT
    )
    expect(pos.left).toBe(8)
  })

  it("pins to the left margin when the panel is wider than the viewport", () => {
    const pos = placeAnchoredPopup(
      { left: 100, top: 300, bottom: 320 },
      { width: 600, height: 200 },
      { width: 400, height: 800 }
    )
    expect(pos.left).toBe(8)
  })

  it("places the panel above the caret when there is room (composer at bottom)", () => {
    const pos = placeAnchoredPopup(
      { left: 100, top: 600, bottom: 620 },
      SIZE,
      VIEWPORT
    )
    expect(pos.placement).toBe("above")
    // top = caret.top - gap - height = 600 - 4 - 288 = 308
    expect(pos.top).toBe(308)
  })

  it("flips below the caret when there is not enough room above", () => {
    // caret near the top: only 50px above, panel needs 288+4.
    const pos = placeAnchoredPopup(
      { left: 100, top: 50, bottom: 70 },
      SIZE,
      VIEWPORT
    )
    expect(pos.placement).toBe("below")
    // top = caret.bottom + gap = 70 + 4 = 74
    expect(pos.top).toBe(74)
  })

  it("falls back to the side with more room when neither fully fits", () => {
    // Short viewport: nothing fits. caret.top=120 (roomAbove≈112),
    // caret.bottom=140 in a 300-tall viewport (roomBelow≈152) → below wins.
    const pos = placeAnchoredPopup({ left: 100, top: 120, bottom: 140 }, SIZE, {
      width: 1280,
      height: 300,
    })
    expect(pos.placement).toBe("below")
    // top clamps so the panel doesn't leave the viewport bottom:
    // min(140+4, 300-288-8)=min(144,4)=4 → max(8,4)=8
    expect(pos.top).toBe(8)
  })

  it("prefers above when both sides are equally cramped", () => {
    // Symmetric: caret centered in a viewport too short for either side.
    const pos = placeAnchoredPopup({ left: 100, top: 150, bottom: 150 }, SIZE, {
      width: 1280,
      height: 300,
    })
    expect(pos.placement).toBe("above")
  })

  it("hangs below the anchor when asked to prefer that side", () => {
    // The `/` command panel drops out of the composer box: below is the
    // resting look, so it must not flip up just because above also fits.
    const pos = placeAnchoredPopup(
      { left: 100, top: 300, bottom: 340 },
      SIZE,
      VIEWPORT,
      { prefer: "below" }
    )
    expect(pos.placement).toBe("below")
    // top = anchor.bottom + gap = 340 + 4 = 344
    expect(pos.top).toBe(344)
  })

  it("flips a below-preferring panel up when the anchor is near the bottom", () => {
    // The case the dialogs hit: composer low in a short window, so the panel
    // would spill off-screen underneath it.
    const pos = placeAnchoredPopup(
      { left: 100, top: 480, bottom: 520 },
      SIZE,
      VIEWPORT,
      { prefer: "below" }
    )
    // roomBelow = 800 - 520 - 8 = 272, needed = 288 + 4 = 292 → doesn't fit.
    // roomAbove = 480 - 8 = 472 → flips above.
    expect(pos.placement).toBe("above")
    // top = anchor.top - gap - height = 480 - 4 - 288 = 188
    expect(pos.top).toBe(188)
  })

  it("pins to the top-left corner when the caret rect is null", () => {
    const pos = placeAnchoredPopup(null, SIZE, VIEWPORT)
    expect(pos).toEqual({ left: 8, top: 8, placement: "below" })
  })

  it("respects custom margin and gap options", () => {
    const pos = placeAnchoredPopup(
      { left: 100, top: 600, bottom: 620 },
      SIZE,
      VIEWPORT,
      { margin: 16, gap: 10 }
    )
    // above: top = 600 - 10 - 288 = 302
    expect(pos.top).toBe(302)
    expect(pos.left).toBe(100)
  })
})
