"use client"

import { useLayoutEffect, useRef, useState } from "react"
import { useStickToBottomContext } from "use-stick-to-bottom"
import { SCROLL_FOLLOW_INTERACTIVE_SELECTOR } from "./message-scroll-context"

export function useInitialHistoryScrollEligibility(
  conversationId: number | null
): boolean {
  const [eligible] = useState(() => conversationId != null)
  return eligible
}

export interface InitialHistoryScrollControllerProps {
  pending: boolean
  historyReady: boolean
  hasHistoryRows: boolean
  onFinish: () => void
}

export function InitialHistoryScrollController({
  pending,
  historyReady,
  hasHistoryRows,
  onFinish,
}: InitialHistoryScrollControllerProps) {
  const { contentRef, scrollRef, scrollToBottom, stopScroll } =
    useStickToBottomContext()
  const scrollToBottomRef = useRef(scrollToBottom)
  const stopScrollRef = useRef(stopScroll)
  const onFinishRef = useRef(onFinish)

  useLayoutEffect(() => {
    scrollToBottomRef.current = scrollToBottom
    stopScrollRef.current = stopScroll
    onFinishRef.current = onFinish
  })

  useLayoutEffect(() => {
    if (!pending || !historyReady) return
    if (!hasHistoryRows) {
      onFinishRef.current()
      return
    }

    const viewport = scrollRef.current
    if (!viewport) return

    let disposed = false
    let frameId: number | null = null
    let previousContentHeight: number | null = null
    let previousScrollHeight: number | null = null
    let stableFrames = 0

    const removeListeners = () => {
      viewport.removeEventListener("wheel", cancelForUser)
      viewport.removeEventListener("touchstart", cancelForUser)
      viewport.removeEventListener("pointerdown", onPointerDown)
      viewport.removeEventListener("keydown", onKeyDown)
    }

    const finish = (cancelledByUser: boolean) => {
      if (disposed) return
      disposed = true
      if (frameId != null) {
        cancelAnimationFrame(frameId)
        frameId = null
      }
      removeListeners()
      if (cancelledByUser) stopScrollRef.current()
      onFinishRef.current()
    }

    function cancelForUser() {
      finish(true)
    }

    function onPointerDown(event: PointerEvent) {
      // Treat missing button as primary (jsdom/fireEvent often omits it).
      if ((event.button ?? 0) !== 0 || event.ctrlKey) return
      const target = event.target as Element | null
      if (target?.closest(SCROLL_FOLLOW_INTERACTIVE_SELECTOR)) return
      cancelForUser()
    }

    function onKeyDown(event: KeyboardEvent) {
      if (
        event.key === "PageUp" ||
        event.key === "Home" ||
        event.key === "ArrowUp"
      ) {
        cancelForUser()
      }
    }

    const measure = () => {
      frameId = null
      if (disposed) return
      const content = contentRef.current
      const currentViewport = scrollRef.current
      if (!content || !currentViewport) {
        frameId = requestAnimationFrame(measure)
        return
      }

      const currentContentHeight = content.getBoundingClientRect().height
      const currentScrollHeight = currentViewport.scrollHeight
      if (
        currentContentHeight === previousContentHeight &&
        currentScrollHeight === previousScrollHeight
      ) {
        stableFrames += 1
      } else {
        stableFrames = 0
      }
      previousContentHeight = currentContentHeight
      previousScrollHeight = currentScrollHeight

      if (stableFrames >= 2) {
        void scrollToBottomRef.current({ animation: "instant" })
        finish(false)
        return
      }
      frameId = requestAnimationFrame(measure)
    }

    viewport.addEventListener("wheel", cancelForUser, { passive: true })
    viewport.addEventListener("touchstart", cancelForUser, { passive: true })
    viewport.addEventListener("pointerdown", onPointerDown)
    viewport.addEventListener("keydown", onKeyDown)

    void scrollToBottomRef.current({ animation: "instant" })
    frameId = requestAnimationFrame(measure)

    return () => {
      if (disposed) return
      disposed = true
      if (frameId != null) cancelAnimationFrame(frameId)
      removeListeners()
    }
  }, [contentRef, hasHistoryRows, historyReady, pending, scrollRef])

  return null
}
