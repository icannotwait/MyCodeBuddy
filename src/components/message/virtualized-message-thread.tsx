"use client"

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
} from "react"
import type { CSSProperties, ReactNode, RefObject } from "react"
import { Virtualizer, type VirtualizerHandle } from "virtua"
import { useStickToBottomContext } from "use-stick-to-bottom"
import {
  MessageThreadContent,
  type MessageThreadContentProps,
} from "@/components/ai-elements/message-thread"
import { cn } from "@/lib/utils"
import {
  createLiveFooterScrollCoordinator,
  MessageScrollProvider,
  SCROLL_FOLLOW_INTERACTIVE_SELECTOR,
  type LiveFooterScrollCoordinator,
  type MessageScrollContextValue,
} from "@/components/message/message-scroll-context"

interface VirtualizedMessageThreadProps<T> {
  /** Data to virtualise — each entry becomes one virtual row. */
  items: T[]
  /** Stable key for a given item (used as React key). */
  getItemKey: (item: T, index: number) => string
  /** Render the content of one row. */
  renderItem: (item: T, index: number) => ReactNode
  /** Shown when `items` is empty and no live footer is present. */
  emptyState?: ReactNode
  /**
   * Live reply / typing footer rendered inside the shared scroll content but
   * **outside** the Virtua item array. Never included in `items`, keys,
   * navigation indices, or item padding calculations.
   */
  footer?: ReactNode
  /** Extra className on the live footer shell. */
  footerClassName?: string
  /**
   * Hint for the initial height (px) of an unmeasured item.
   * Virtua auto-measures every item once mounted, so this only
   * affects the very first paint — omit it if you don't care.
   */
  itemSize?: number
  /**
   * Pixels of overscan around the viewport (virtua `bufferSize`).
   * Larger values reduce blank flashes during fast scroll on tall rows
   * at the cost of more off-screen reconciliation. @default 800
   *
   * Task 15 Step 7 decision: keep 800 (no measured ≥10% layout+paint P95
   * gain with zero blank frames for 400; see comparison.md).
   */
  bufferSize?: number
  /** Vertical gap between items in px. @default 16 */
  gap?: number
  /** Vertical padding before the first / after the last item. @default 16 */
  padding?: number
  /** Extra className on every item's inner wrapper (the `max-w-3xl` div). */
  className?: string
  /** Extra className on the MessageThreadContent shell. */
  contentClassName?: string
  /** Extra props forwarded to MessageThreadContent. */
  contentProps?: Omit<MessageThreadContentProps, "children" | "className">
  /**
   * Publishes the virtualizer scroll handle to an ancestor so siblings that
   * live outside the `MessageScrollProvider` subtree (e.g. the conversation
   * message navigator) can drive `scrollToIndex`.
   */
  scrollApiRef?: RefObject<MessageScrollContextValue | null>
}

function isAtBottomElement(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= 2
}

function VirtualizedMessageThreadImpl<T>({
  items,
  getItemKey,
  renderItem,
  emptyState,
  footer,
  footerClassName,
  itemSize,
  bufferSize = 800,
  gap = 16,
  padding = 16,
  className,
  contentClassName,
  contentProps,
  scrollApiRef,
}: VirtualizedMessageThreadProps<T>) {
  const { scrollRef, scrollToBottom, stopScroll, isAtBottom } =
    useStickToBottomContext()
  const virtualizerHandleRef = useRef<VirtualizerHandle>(null)
  const footerShellRef = useRef<HTMLDivElement | null>(null)
  const scrollToBottomRef = useRef(scrollToBottom)
  const stopScrollRef = useRef(stopScroll)
  scrollToBottomRef.current = scrollToBottom
  stopScrollRef.current = stopScroll
  const hasFooter = footer != null
  // Seed follow intent once when footer appears (not on every re-render).
  const initialFollowRef = useRef(true)
  if (!hasFooter) {
    initialFollowRef.current = true
  }

  const scrollToIndex = useCallback<MessageScrollContextValue["scrollToIndex"]>(
    (index, opts) => {
      // Indices refer only to historical Virtua items — never the live footer.
      virtualizerHandleRef.current?.scrollToIndex(index, opts)
    },
    []
  )

  // Render-local coordinator owned by the committed Context value — no shared
  // ref written during speculative renders. Depend only on `hasFooter` so
  // isAtBottom churn does not recreate the coordinator every frame.
  const footerCoordinator = useMemo<LiveFooterScrollCoordinator | null>(() => {
    if (!hasFooter) return null
    const el = scrollRef.current
    const initiallyFollowing =
      typeof isAtBottom === "boolean"
        ? isAtBottom
        : el
          ? isAtBottomElement(el)
          : initialFollowRef.current
    return createLiveFooterScrollCoordinator({
      scrollToBottom: (opts) => {
        void scrollToBottomRef.current(opts)
      },
      stopScroll: () => {
        stopScrollRef.current()
      },
      initiallyFollowing,
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps -- seed isAtBottom only when footer mounts
  }, [hasFooter])

  // Dispose the committed coordinator when it is replaced or unmounted.
  useLayoutEffect(() => {
    return () => {
      footerCoordinator?.dispose()
    }
  }, [footerCoordinator])

  // Escape / re-entry listeners on the scroll viewport.
  useEffect(() => {
    if (!footerCoordinator) return
    const el = scrollRef.current
    if (!el) return

    const onWheel = () => {
      footerCoordinator.cancelForUserInput()
    }
    const onTouchStart = () => {
      footerCoordinator.cancelForUserInput()
    }
    const onPointerDown = (e: PointerEvent) => {
      if (e.button !== 0 || e.ctrlKey) return
      const target = e.target as HTMLElement | null
      if (target?.closest(SCROLL_FOLLOW_INTERACTIVE_SELECTOR)) return
      footerCoordinator.cancelForUserInput()
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "PageUp" || e.key === "Home" || e.key === "ArrowUp") {
        footerCoordinator.cancelForUserInput()
      }
    }
    const onScroll = () => {
      if (isAtBottomElement(el)) {
        footerCoordinator.markAtBottom()
      }
    }

    el.addEventListener("wheel", onWheel, { passive: true })
    el.addEventListener("touchstart", onTouchStart, { passive: true })
    el.addEventListener("pointerdown", onPointerDown)
    el.addEventListener("keydown", onKeyDown)
    el.addEventListener("scroll", onScroll, { passive: true })

    return () => {
      el.removeEventListener("wheel", onWheel)
      el.removeEventListener("touchstart", onTouchStart)
      el.removeEventListener("pointerdown", onPointerDown)
      el.removeEventListener("keydown", onKeyDown)
      el.removeEventListener("scroll", onScroll)
    }
  }, [footerCoordinator, scrollRef])

  // Single schedule-owner split:
  // - LiveTranscriptRow (footer) owns publication follow via lastAppliedSeq.
  // - This shell owns only non-seq height growth (tool expand / sealed block)
  //   through one ResizeObserver so we never dual-schedule the same seq.
  useEffect(() => {
    if (!footerCoordinator) return
    const shell = footerShellRef.current
    if (!shell) return
    if (typeof ResizeObserver !== "function") return

    let resizeVersion = 0
    const observer = new ResizeObserver(() => {
      resizeVersion += 1
      footerCoordinator.scheduleFollow(resizeVersion)
    })
    observer.observe(shell)
    return () => observer.disconnect()
  }, [footerCoordinator, footer])

  // Coordinator is a stable field on the context value for this footer epoch —
  // children read it during the same commit without a shared mutable ref.
  const scrollContextValue = useMemo<MessageScrollContextValue>(
    () => ({
      scrollToIndex,
      footerScroll: footerCoordinator ?? undefined,
    }),
    [scrollToIndex, footerCoordinator]
  )

  // Mirror the (stable) scroll handle into the caller-owned ref so a sibling
  // rendered outside this provider can call it. Runs once since the value is
  // referentially stable.
  useEffect(() => {
    if (!scrollApiRef) return
    scrollApiRef.current = scrollContextValue
    return () => {
      scrollApiRef.current = null
    }
  }, [scrollApiRef, scrollContextValue])

  // Make the scroll viewport focusable so the browser's native keyboard
  // scrolling (Arrow keys, PageUp/PageDown, Home/End, Space) works — matching
  // the sidebar conversation list, whose card <button>s are focusable and let
  // the browser scroll their scrollable ancestor. A left-click on
  // non-interactive transcript content focuses the viewport so the keys engage,
  // without stealing focus from interactive controls (links, buttons, inputs)
  // or breaking text selection (focus() doesn't clear a selection).
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    el.tabIndex = 0
    const onPointerDown = (e: PointerEvent) => {
      // Ignore right-click and macOS ctrl-click (both open the context menu).
      if (e.button !== 0 || e.ctrlKey) return
      const target = e.target as HTMLElement | null
      // Don't steal focus from interactive/editable elements — they manage
      // their own focus (some do it in pointerdown). We deliberately do NOT
      // match a bare `[tabindex]` here: the viewport itself has tabIndex=0, so
      // an ancestor match would suppress focusing on every transcript click.
      if (target?.closest(SCROLL_FOLLOW_INTERACTIVE_SELECTOR)) return
      el.focus({ preventScroll: true })
    }
    el.addEventListener("pointerdown", onPointerDown)
    return () => el.removeEventListener("pointerdown", onPointerDown)
  }, [scrollRef])

  // Pre-compute the three possible padding styles so every render reuses
  // the same object references (avoids allocating per-item on each frame).
  // Footer is outside Virtua — last-item bottom padding is unchanged.
  const styles = useMemo(() => {
    const halfGap = gap / 2
    return {
      only: { paddingTop: padding, paddingBottom: padding } as CSSProperties,
      first: { paddingTop: padding, paddingBottom: halfGap } as CSSProperties,
      middle: { paddingTop: halfGap, paddingBottom: halfGap } as CSSProperties,
      last: { paddingTop: halfGap, paddingBottom: padding } as CSSProperties,
    }
  }, [gap, padding])

  const itemStyle = (index: number, total: number) => {
    if (total === 1) return styles.only
    if (index === 0) return styles.first
    if (index === total - 1) return styles.last
    return styles.middle
  }

  const showEmpty = items.length === 0 && footer == null

  return (
    <MessageScrollProvider value={scrollContextValue}>
      <MessageThreadContent
        className={cn("mx-0 max-w-none p-0", contentClassName)}
        scrollClassName="scrollbar-thin overscroll-contain [overflow-anchor:none] outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
        {...contentProps}
      >
        {showEmpty ? (
          (emptyState ?? null)
        ) : (
          <>
            {items.length > 0 ? (
              <Virtualizer
                ref={virtualizerHandleRef}
                scrollRef={
                  scrollRef as unknown as RefObject<HTMLElement | null>
                }
                itemSize={itemSize}
                bufferSize={bufferSize}
              >
                {items.map((item, index) => (
                  <div
                    key={getItemKey(item, index)}
                    style={itemStyle(index, items.length)}
                    data-message-history-row
                  >
                    <div className={cn("mx-auto max-w-3xl px-4", className)}>
                      {renderItem(item, index)}
                    </div>
                  </div>
                ))}
              </Virtualizer>
            ) : null}
            {footer ? (
              <div
                ref={footerShellRef}
                data-message-live-footer
                className={cn(
                  "mx-auto w-full max-w-3xl px-4 pb-4",
                  footerClassName
                )}
              >
                {footer}
              </div>
            ) : null}
          </>
        )}
      </MessageThreadContent>
    </MessageScrollProvider>
  )
}

// Memoized so a cross-tab broadcast render of MessageListView with an
// unchanged `items` reference (see getTimelineTurns memoization) skips the
// per-row React element creation entirely. The streaming tab's `items`
// reference changes every flush, so it re-renders as before. `getItemKey` /
// `renderItem` are stabilized by the caller; default shallow prop comparison
// is sufficient. The `as` cast preserves the generic call signature that
// `memo` would otherwise erase.
export const VirtualizedMessageThread = memo(
  VirtualizedMessageThreadImpl
) as typeof VirtualizedMessageThreadImpl
