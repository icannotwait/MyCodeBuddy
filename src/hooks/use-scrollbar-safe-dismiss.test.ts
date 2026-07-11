import { renderHook } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"

import { useScrollbarSafeDismiss } from "@/hooks/use-scrollbar-safe-dismiss"

// A content box spanning x∈[100,300], y∈[50,450] — the popover's rect, with its
// native scrollbar gutter living at the right edge (x≈292–300).
const RECT = { left: 100, right: 300, top: 50, bottom: 450 } as DOMRect

function mountWithRect(rect: DOMRect | null) {
  const { result } = renderHook(() => useScrollbarSafeDismiss())
  result.current.contentRef.current = (rect
    ? { getBoundingClientRect: () => rect }
    : null) as unknown as HTMLDivElement
  return result
}

function firePointerDownOutside(
  handler: (event: CustomEvent<{ originalEvent: PointerEvent }>) => void,
  clientX: number,
  clientY: number
) {
  const preventDefault = vi.fn()
  handler({
    detail: { originalEvent: { clientX, clientY } as PointerEvent },
    preventDefault,
  } as unknown as CustomEvent<{ originalEvent: PointerEvent }>)
  return preventDefault
}

describe("useScrollbarSafeDismiss", () => {
  it("prevents dismissal when the pointer-down lands on the scrollbar gutter (inside the box)", () => {
    const result = mountWithRect(RECT)
    // Right-edge gutter where the native scrollbar sits.
    const preventDefault = firePointerDownOutside(
      result.current.onPointerDownOutside,
      296,
      220
    )
    expect(preventDefault).toHaveBeenCalledTimes(1)
  })

  it("allows dismissal for a genuine click outside the box", () => {
    const result = mountWithRect(RECT)
    const preventDefault = firePointerDownOutside(
      result.current.onPointerDownOutside,
      500,
      220
    )
    expect(preventDefault).not.toHaveBeenCalled()
  })

  it("treats the box edges as inside (inclusive bounds)", () => {
    const result = mountWithRect(RECT)
    for (const [x, y] of [
      [100, 50],
      [300, 450],
      [300, 50],
    ] as const) {
      const preventDefault = firePointerDownOutside(
        result.current.onPointerDownOutside,
        x,
        y
      )
      expect(preventDefault).toHaveBeenCalledTimes(1)
    }
  })

  it("allows dismissal one pixel past the right/bottom edges", () => {
    const result = mountWithRect(RECT)
    expect(
      firePointerDownOutside(result.current.onPointerDownOutside, 301, 220)
    ).not.toHaveBeenCalled()
    expect(
      firePointerDownOutside(result.current.onPointerDownOutside, 200, 451)
    ).not.toHaveBeenCalled()
  })

  it("no-ops (never throws) before the content ref is attached", () => {
    const result = mountWithRect(null)
    const preventDefault = firePointerDownOutside(
      result.current.onPointerDownOutside,
      200,
      220
    )
    expect(preventDefault).not.toHaveBeenCalled()
  })
})
