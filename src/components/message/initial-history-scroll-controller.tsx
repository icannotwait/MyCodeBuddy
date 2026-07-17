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
  /** One-shot guard: survives StrictMode effect replay; not set before readiness. */
  const finishedRef = useRef(false)

  useLayoutEffect(() => {
    scrollToBottomRef.current = scrollToBottom
    stopScrollRef.current = stopScroll
    onFinishRef.current = onFinish
  })

  useLayoutEffect(() => {
    if (!pending || !historyReady) return
    if (!hasHistoryRows) {
      if (finishedRef.current) return
      finishedRef.current = true
      onFinishRef.current()
      return
    }

    let disposed = false
    let started = false
    let frameId: number | null = null
    let previousContentHeight: number | null = null
    let previousScrollHeight: number | null = null
    let stableFrames = 0
    let viewport: HTMLElement | null = null

    const removeListeners = () => {
      if (!viewport) return
      viewport.removeEventListener("wheel", cancelForUser)
      viewport.removeEventListener("touchstart", cancelForUser)
      viewport.removeEventListener("pointerdown", onPointerDown)
      viewport.removeEventListener("keydown", onKeyDown)
    }

    const finish = (cancelledByUser: boolean) => {
      if (disposed || finishedRef.current) return
      disposed = true
      finishedRef.current = true
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
      const target = event.target
      if (
        target instanceof Element &&
        target.closest(SCROLL_FOLLOW_INTERACTIVE_SELECTOR)
      ) {
        return
      }
      // Non-Element targets (e.g. Text) cancel normally.
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

    const beginWithViewport = (el: HTMLElement) => {
      viewport = el
      viewport.addEventListener("wheel", cancelForUser, { passive: true })
      viewport.addEventListener("touchstart", cancelForUser, { passive: true })
      viewport.addEventListener("pointerdown", onPointerDown)
      viewport.addEventListener("keydown", onKeyDown)

      void scrollToBottomRef.current({ animation: "instant" })
      frameId = requestAnimationFrame(measure)
    }

    /** Sync when viewport exists; otherwise cancelable RAF until it attaches. */
    const start = () => {
      frameId = null
      if (disposed || finishedRef.current || started) return
      const el = scrollRef.current
      if (!el) {
        frameId = requestAnimationFrame(start)
        return
      }
      started = true
      beginWithViewport(el)
    }

    start()

    return () => {
      if (disposed) return
      disposed = true
      if (frameId != null) cancelAnimationFrame(frameId)
      removeListeners()
    }
  }, [contentRef, hasHistoryRows, historyReady, pending, scrollRef])

  return null
}
