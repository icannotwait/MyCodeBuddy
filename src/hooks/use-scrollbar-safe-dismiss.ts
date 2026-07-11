"use client"

import { useCallback, useRef } from "react"

// A Radix `onPointerDownOutside` event: a `CustomEvent` carrying the original
// pointer event. Kept structural so this hook stays agnostic to which Radix
// surface (Popover / DropdownMenu) it guards.
type PointerDownOutsideEvent = CustomEvent<{ originalEvent: PointerEvent }>

/**
 * Keeps a Radix `DismissableLayer` surface (Popover / DropdownMenu content) open
 * while the user drags the content's own native scrollbar.
 *
 * WHY: grabbing a native scrollbar fires a `pointerdown` that WebKit routes to
 * the document root rather than the scroll container, so Radix reads it as an
 * "outside" interaction and dismisses the layer the instant you touch the bar
 * (the OpenCode model list scrollbar could not be dragged — it just closed the
 * menu). The pointer coordinates, however, land squarely inside the content's
 * box — the scrollbar gutter sits within it — so we `preventDefault()` whenever
 * the pointer-down originates within that box, and let genuine outside clicks
 * (and Escape, which is a separate handler) dismiss as usual.
 *
 * Wire `contentRef` to the content element and pass `onPointerDownOutside` to it.
 */
export function useScrollbarSafeDismiss<
  T extends HTMLElement = HTMLDivElement,
>() {
  const contentRef = useRef<T>(null)

  const onPointerDownOutside = useCallback((event: PointerDownOutsideEvent) => {
    const content = contentRef.current
    if (!content) return
    const { clientX, clientY } = event.detail.originalEvent
    const rect = content.getBoundingClientRect()
    const insideBox =
      clientX >= rect.left &&
      clientX <= rect.right &&
      clientY >= rect.top &&
      clientY <= rect.bottom
    if (insideBox) event.preventDefault()
  }, [])

  return { contentRef, onPointerDownOutside }
}
