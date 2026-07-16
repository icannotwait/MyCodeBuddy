"use client"

import { createContext, useContext } from "react"
import type { ScrollToIndexOpts } from "virtua"

/**
 * Coordinates stick-to-bottom follow intent for the live footer independently
 * of post-growth `isAtBottom` glitches. One pending RAF; escape cancels
 * immediately; bottom re-entry (scroll or down button) re-enables follow.
 */
export interface LiveFooterScrollCoordinator {
  scheduleFollow(publicationVersion: number): void
  cancelForUserInput(): void
  markAtBottom(): void
  dispose(): void
  /** Test/debug: whether follow intent is currently active. */
  isFollowing(): boolean
}

export type LiveFooterScrollToBottom = (options: {
  animation: "instant"
  preserveScrollPosition: true
}) => void

export interface CreateLiveFooterScrollCoordinatorOptions {
  scrollToBottom: LiveFooterScrollToBottom
  stopScroll: () => void
  /** @default true */
  initiallyFollowing?: boolean
  scheduleFrame?: (callback: FrameRequestCallback) => number
  cancelFrame?: (handle: number) => void
}

/**
 * Create a footer scroll coordinator. Keeps at most one pending animation
 * frame; `scheduleFollow` replaces the pending publication version.
 */
export function createLiveFooterScrollCoordinator(
  options: CreateLiveFooterScrollCoordinatorOptions
): LiveFooterScrollCoordinator {
  const scheduleFrame =
    options.scheduleFrame ??
    ((cb: FrameRequestCallback) => requestAnimationFrame(cb))
  const cancelFrame =
    options.cancelFrame ?? ((id: number) => cancelAnimationFrame(id))

  let followIntent = options.initiallyFollowing ?? true
  let pendingVersion: number | null = null
  let rafId: number | null = null
  let disposed = false

  const flush = () => {
    rafId = null
    if (disposed || !followIntent || pendingVersion == null) {
      pendingVersion = null
      return
    }
    pendingVersion = null
    options.scrollToBottom({
      animation: "instant",
      preserveScrollPosition: true,
    })
  }

  return {
    scheduleFollow(publicationVersion: number) {
      if (disposed) return
      pendingVersion = publicationVersion
      if (rafId == null) {
        rafId = scheduleFrame(flush)
      }
    },
    cancelForUserInput() {
      if (disposed) return
      followIntent = false
      pendingVersion = null
      if (rafId != null) {
        cancelFrame(rafId)
        rafId = null
      }
      options.stopScroll()
    },
    markAtBottom() {
      if (disposed) return
      followIntent = true
    },
    dispose() {
      disposed = true
      followIntent = false
      pendingVersion = null
      if (rafId != null) {
        cancelFrame(rafId)
        rafId = null
      }
    },
    isFollowing() {
      return followIntent && !disposed
    },
  }
}

/** Interactive targets that must not cancel follow intent on pointerdown. */
export const SCROLL_FOLLOW_INTERACTIVE_SELECTOR =
  'a[href],button,input,textarea,select,summary,[contenteditable]:not([contenteditable="false"]),[role="button"],[role="link"],[role="checkbox"],[role="switch"],[role="radio"],[role="tab"],[role="textbox"],[role="menuitem"],[role="option"],[role="combobox"],[role="slider"]'

export interface MessageScrollContextValue {
  scrollToIndex: (index: number, opts?: ScrollToIndexOpts) => void
  /** Present while a live footer is mounted and follow coordination is active. */
  footerScroll?: LiveFooterScrollCoordinator
}

const MessageScrollContext = createContext<MessageScrollContextValue | null>(
  null
)

export const MessageScrollProvider = MessageScrollContext.Provider

export function useMessageScroll(): MessageScrollContextValue | null {
  return useContext(MessageScrollContext)
}
