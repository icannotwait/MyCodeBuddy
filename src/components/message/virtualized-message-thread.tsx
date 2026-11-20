"use client"

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react"
import type { CSSProperties, ReactNode, RefObject } from "react"
import {
  Virtualizer,
  type CustomContainerComponentProps,
  type VirtualizerHandle,
} from "virtua"
import { useStickToBottomContext } from "use-stick-to-bottom"
import { Loader2 } from "lucide-react"
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

/**
 * Scroll offset (px from the top) under which a reverse-infinite-scroll load
 * is triggered. Fired on the downward CROSSING of this threshold (scrolling
 * up), never on absolute position — so the initial instant stick-to-bottom
 * (which starts at offset 0 before the first layout) and short lists that
 * never scroll cannot auto-trigger a cascade of page loads.
 */
const LOAD_OLDER_THRESHOLD_PX = 240

/**
 * How far below the viewport's top edge `onViewportTopChange` looks for the
 * row on top: just inside it, so the row a jump aligned to the top
 * (`scrollToIndex(…, { align: "start" })`) is the one reported.
 */
const VIEWPORT_TOP_PROBE_PX = 4

/** Slack (px) under which the viewport counts as scrolled to the end. */
const VIEWPORT_END_SLACK_PX = 2

/** The loader row's place among the rows virtua draws (see `hasOlder`). */
const LOAD_OLDER_ROW = Symbol("load-older-row")

/** Room (rem, so it follows the app zoom) left above a part of a row that
 *  `revealFromAbove` brings into view, so it does not land on the edge. */
const REVEAL_ROOM_REM = 1

/**
 * The rows `keepMounted` holds near the end of the list: from the first one
 * within the buffer above the viewport to the last — or none, while the end is
 * more than a viewport below the view.
 *
 * Scrolling down, virtua drops the rows above the viewport and mounts them
 * again once the scroll settles. A row that mounts at another height than it
 * settles at — a clamped message before its toggle is measured, an image
 * still loading — then resizes above the view twice, and the scroll
 * correction for the pair can land a pixel off at a fractional zoom. Near the
 * bottom the stick-to-bottom lock slides that pixel back down, which drops the
 * rows again: the view shook up and down for as long as it rested there. Kept
 * mounted near the end, the rows are not mounted again. Exported for unit
 * tests.
 */
export function rowsKeptNearEnd(view: {
  offset: number
  viewportSize: number
  scrollSize: number
  bufferSize: number
  findItemIndex: (offset: number) => number
}): number | null {
  if (view.offset + view.viewportSize * 2 < view.scrollSize) return null
  return view.findItemIndex(Math.max(0, view.offset - view.bufferSize))
}

/**
 * The virtualizer's container, without the `pointer-events: none` virtua puts
 * on it while the list scrolls. The property is inherited, so turning it on as
 * a scroll starts and off as it ends restyles everything the thread has drawn —
 * in a reply with an opened timeline of hundreds of rows, a frame of hundreds
 * of milliseconds each way. What it buys — rows under the pointer neither
 * hover nor take a click while the list moves — is what any page does anyway.
 */
function ThreadContainer({
  style,
  children,
  ref,
}: CustomContainerComponentProps) {
  return (
    <div ref={ref} style={{ ...style, pointerEvents: undefined }}>
      {children}
    </div>
  )
}

/** What `onViewportTopChange` reports. */
export interface ViewportTop {
  /** The `items` index of the row at the viewport's top. */
  index: number
  /** The viewport is scrolled to the end of the list. */
  atEnd: boolean
}

/**
 * Keys the browser scrolls a focused viewport with. Modifiers don't matter:
 * Cmd+ArrowDown and Ctrl+End jump to the end, Shift+Space pages back up.
 */
const SCROLL_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
])

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
   * Optional chrome above the virtualized rows (e.g. "Load earlier messages").
   * Outside Virtua so it does not affect item indices / nav.
   */
  header?: ReactNode
  /** Extra className on the header shell. */
  headerClassName?: string
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
  /** Extra className on every item's inner wrapper (the `chat-content-w` div). */
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
  /**
   * Reverse infinite scroll: older items exist above `items[0]`. Renders a
   * loader row above the list and arms the near-top trigger. The row is also
   * a button, so a window shorter than the viewport (nothing to scroll) can
   * still page in history.
   */
  hasOlder?: boolean
  /** An older-page request is in flight (spinner state on the loader row). */
  isLoadingOlder?: boolean
  /** Called on the near-top crossing / loader click to page in history. */
  onLoadOlder?: () => void
  /** Loader-row labels (i18n: idle button text / loading text). */
  loadOlderLabel?: string
  loadingOlderLabel?: string
  /**
   * Explicit prepend signal driving virtua's `shift`: bumped by the data
   * layer on every render whose `items` gained prepended history, scoped by
   * `prependScopeKey` (a conversation id) so a scope switch — a different
   * conversation's list replacing this one wholesale — never reads as a
   * prepend. An explicit signal, NOT a key heuristic: a window starting
   * mid-way through a consecutive-assistant run merges with prepended turns
   * into one render item whose key changes, so "did the old first key
   * survive" is false on exactly the render that must shift.
   */
  prependEpoch?: number
  prependScopeKey?: string | number
  /**
   * Told which row sits at the top of the viewport whenever that changes, and
   * once on being passed — the message navigator highlights the message the
   * transcript shows. Measured a frame after a scroll or an items change, so
   * a fling costs a lookup per frame at most; omit it and nothing is measured.
   */
  onViewportTopChange?: (top: ViewportTop) => void
}

/**
 * `shift` decision for one render: same scope, epoch advanced. Exported for
 * unit tests.
 */
export function shouldShiftForPrepend(
  prev: { scope: string | number | undefined; epoch: number },
  next: { scope: string | number | undefined; epoch: number }
): boolean {
  return prev.scope === next.scope && next.epoch !== prev.epoch
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
  header,
  headerClassName,
  itemSize,
  bufferSize = 800,
  gap = 16,
  padding = 16,
  className,
  contentClassName,
  contentProps,
  scrollApiRef,
  hasOlder = false,
  isLoadingOlder = false,
  onLoadOlder,
  loadOlderLabel,
  loadingOlderLabel,
  prependEpoch = 0,
  prependScopeKey,
  onViewportTopChange,
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

  // The loader row occupies virtua index 0 when present, shifting every data
  // item's virtual index by one — account for it in scrollToIndex so nav
  // targets (computed over `items`) keep landing on the right row. Mirrored
  // into a ref post-commit (see the sync effect below); scrollToIndex only
  // runs from user interactions, so it always reads a committed value.
  const rowOffset = hasOlder ? 1 : 0
  const rowOffsetRef = useRef(rowOffset)

  const scrollToIndex = useCallback<MessageScrollContextValue["scrollToIndex"]>(
    (index, opts) => {
      const handle = virtualizerHandleRef.current
      if (!handle) return
      const target = index + rowOffsetRef.current
      // Indices refer only to historical Virtua items — never the live footer.
      // A jump up to a message leaves the bottom, so it releases the
      // stick-to-bottom lock the way the user's own scroll would (see
      // `EscapeLockOnUserScroll`). Left engaged, the lock scrolls back down
      // on the next content growth, and it discards scroll events while a
      // resize settles, so it never sees the jump: one made as content
      // changes — the navigator paging history in, a reply streaming — was
      // dragged straight back to the bottom. A jump that can't move up (to
      // the message the view already rests on) keeps the lock, so a reply
      // still streaming in stays followed.
      if (handle.getItemOffset(target) < handle.scrollOffset) stopScroll()
      handle.scrollToIndex(target, opts)
    },
    [stopScroll]
  )
  // Up to a part of a row that is above the view — the header of a reply
  // folding from the foot of its steps — with a little room above it. Like
  // the jump above, it leaves the bottom and lets go of the lock first.
  const revealFromAbove = useCallback<
    MessageScrollContextValue["revealFromAbove"]
  >(
    (element) => {
      const port = scrollRef.current
      if (!port) return
      const top =
        element.getBoundingClientRect().top - port.getBoundingClientRect().top
      // In view already, however close to the edge: left where it is.
      if (top >= 0) return
      const rem =
        Number.parseFloat(
          getComputedStyle(port.ownerDocument.documentElement).fontSize
        ) || 16
      stopScroll()
      port.scrollTop += top - REVEAL_ROOM_REM * rem
    },
    [scrollRef, stopScroll]
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
      revealFromAbove,
    }),
    [scrollToIndex, footerCoordinator, revealFromAbove]
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

  // virtua's `shift` for reverse infinite scroll, derived from the explicit
  // prepend signal (see the prop doc — key heuristics fail under cross-page
  // assistant-run merges). Derived-state-during-render pattern (not a ref —
  // refs must not be read in render): when the signal moves, re-render
  // immediately so the COMMITTED render of the prepended children carries
  // `shift=true` and virtua anchors the viewport to the end instead of
  // jumping. The stored flag persists only across renders with an identical
  // signal (subsequent renders reuse the same children or replace them via a
  // signal-less update, where shift must read false again once items move).
  const [prependMark, setPrependMark] = useState<{
    scope: string | number | undefined
    epoch: number
    items: T[]
    shift: boolean
  }>({ scope: prependScopeKey, epoch: prependEpoch, items, shift: false })
  if (
    prependMark.items !== items ||
    prependMark.epoch !== prependEpoch ||
    prependMark.scope !== prependScopeKey
  ) {
    setPrependMark({
      scope: prependScopeKey,
      epoch: prependEpoch,
      items,
      shift: shouldShiftForPrepend(
        { scope: prependMark.scope, epoch: prependMark.epoch },
        { scope: prependScopeKey, epoch: prependEpoch }
      ),
    })
  }
  const shift =
    prependMark.items === items &&
    prependMark.epoch === prependEpoch &&
    prependMark.scope === prependScopeKey
      ? prependMark.shift
      : false

  // Near-top trigger, armed only on the downward CROSSING of the threshold —
  // see LOAD_OLDER_THRESHOLD_PX. Prop values are mirrored into a ref
  // post-commit so the scroll handler stays referentially stable across
  // streaming re-renders; scroll events only fire after commit.
  const loadOlderStateRef = useRef({ hasOlder, isLoadingOlder, onLoadOlder })
  const viewportTopListenerRef = useRef(onViewportTopChange)
  const itemsRef = useRef({ items, getItemKey })
  useEffect(() => {
    rowOffsetRef.current = rowOffset
    loadOlderStateRef.current = { hasOlder, isLoadingOlder, onLoadOlder }
    viewportTopListenerRef.current = onViewportTopChange
    itemsRef.current = { items, getItemKey }
  })

  // The row at the viewport's top, reported when it changes. Read a frame
  // after whatever moved it, when virtua has laid the rows out.
  const lastViewportTopRef = useRef<ViewportTop | null>(null)
  const viewportTopFrameRef = useRef<number | null>(null)
  const measureViewportTop = useCallback(() => {
    viewportTopFrameRef.current = null
    const report = viewportTopListenerRef.current
    const handle = virtualizerHandleRef.current
    if (!report || !handle) return
    const offset = handle.scrollOffset
    const index = Math.max(
      0,
      handle.findItemIndex(offset + VIEWPORT_TOP_PROBE_PX) -
        rowOffsetRef.current
    )
    const atEnd =
      offset + handle.viewportSize >= handle.scrollSize - VIEWPORT_END_SLACK_PX
    const last = lastViewportTopRef.current
    if (last && last.index === index && last.atEnd === atEnd) return
    lastViewportTopRef.current = { index, atEnd }
    report({ index, atEnd })
  }, [])
  const scheduleViewportTop = useCallback(() => {
    if (viewportTopFrameRef.current != null) return
    if (!viewportTopListenerRef.current) return
    viewportTopFrameRef.current = requestAnimationFrame(measureViewportTop)
  }, [measureViewportTop])
  useEffect(
    () => () => {
      if (viewportTopFrameRef.current != null) {
        cancelAnimationFrame(viewportTopFrameRef.current)
        viewportTopFrameRef.current = null
      }
    },
    []
  )
  // A new listener hears where the view is without waiting for a scroll.
  useEffect(() => {
    lastViewportTopRef.current = null
    if (onViewportTopChange) scheduleViewportTop()
  }, [onViewportTopChange, scheduleViewportTop])
  // Rows can move under a still viewport: history paged in above, a reply
  // growing below.
  useEffect(() => {
    scheduleViewportTop()
  }, [items, scheduleViewportTop])

  // The first item `keepMounted` holds (see `rowsKeptNearEnd`), by key: an
  // index would point elsewhere once history is paged in above it, or another
  // conversation's items replace these.
  const [keepFromKey, setKeepFromKey] = useState<string | null>(null)
  const keepMounted = useMemo(() => {
    if (keepFromKey === null) return undefined
    for (let index = items.length - 1; index >= 0; index--) {
      if (getItemKey(items[index]!, index) !== keepFromKey) continue
      const rows: number[] = []
      for (let row = index; row < items.length; row++) {
        rows.push(row + rowOffset)
      }
      return rows
    }
    return undefined
  }, [keepFromKey, items, getItemKey, rowOffset])

  // Where the kept rows start with the view at `offset`.
  const updateKeptRows = useCallback(
    (offset: number) => {
      const handle = virtualizerHandleRef.current
      if (!handle) return
      const row = rowsKeptNearEnd({
        offset,
        viewportSize: handle.viewportSize,
        scrollSize: handle.scrollSize,
        bufferSize,
        findItemIndex: (at) => handle.findItemIndex(at),
      })
      const { items, getItemKey } = itemsRef.current
      const index = row === null ? -1 : Math.max(0, row - rowOffsetRef.current)
      const item = items[index]
      const key = item === undefined ? null : getItemKey(item, index)
      setKeepFromKey((current) => (current === key ? current : key))
    },
    [bufferSize]
  )
  // Rows appended under a still view move the end away from it. Read a frame
  // later, once virtua has laid the rows out, like the viewport top.
  useEffect(() => {
    const frame = requestAnimationFrame(() =>
      updateKeptRows(virtualizerHandleRef.current?.scrollOffset ?? 0)
    )
    return () => cancelAnimationFrame(frame)
  }, [items, updateKeptRows])

  const prevScrollOffsetRef = useRef<number | null>(null)
  const handleScroll = useCallback(
    (offset: number) => {
      scheduleViewportTop()
      updateKeptRows(offset)
      const prev = prevScrollOffsetRef.current
      prevScrollOffsetRef.current = offset
      const s = loadOlderStateRef.current
      if (!s.hasOlder || s.isLoadingOlder || !s.onLoadOlder) return
      if (
        offset < LOAD_OLDER_THRESHOLD_PX &&
        prev !== null &&
        prev >= LOAD_OLDER_THRESHOLD_PX
      ) {
        s.onLoadOlder()
      }
    },
    [scheduleViewportTop, updateKeptRows]
  )

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
    let blurCheck: number | undefined
    const clearPointerFocus = () => {
      window.clearTimeout(blurCheck)
      el.removeAttribute("data-focus-origin")
    }
    const onPointerDown = (e: PointerEvent) => {
      // Ignore right-click and macOS ctrl-click (both open the context menu).
      if (e.button !== 0 || e.ctrlKey) return
      const target = e.target as HTMLElement | null
      // Don't steal focus from interactive/editable elements — they manage
      // their own focus (some do it in pointerdown). We deliberately do NOT
      // match a bare `[tabindex]` here: the viewport itself has tabIndex=0, so
      // an ancestor match would suppress focusing on every transcript click.
      if (target?.closest(SCROLL_FOLLOW_INTERACTIVE_SELECTOR)) return
      el.setAttribute("data-focus-origin", "pointer")
      el.focus({ preventScroll: true })
    }
    // The pointer-origin marker only hides the ring for the click that
    // focused the viewport. Once the user scrolls it with the keyboard, drop
    // the marker so the ring reappears — keeping the keyboard focus indicator
    // visible per WCAG 2.4.7. Other keys keep it: Chromium already matches
    // :focus-visible after that script focus, so clearing on any key ringed
    // the whole transcript on a mere Esc, or on Ctrl/Cmd+C to copy a
    // selection. Tabbing in still shows the ring: that focus never sets it.
    const onKeyDown = (e: KeyboardEvent) => {
      if (SCROLL_KEYS.has(e.key)) clearPointerFocus()
    }
    // Otherwise the marker holds until focus really leaves the viewport. A
    // blur alone isn't that: the whole window losing focus (switch apps, then
    // resize the window to come back) blurs the viewport too, yet it stays the
    // document's active element and the browser re-focuses it on the way
    // back — without the marker, the ring appeared on return. So look after
    // the blur settles: only a viewport that is no longer active drops it.
    const onBlur = () => {
      window.clearTimeout(blurCheck)
      blurCheck = window.setTimeout(() => {
        if (document.activeElement !== el) clearPointerFocus()
      })
    }
    // Focus can also move on while the window is away (a script focuses
    // another control). The viewport gets no second blur for that — its blur
    // already went out with the window's — so look again when the window
    // comes back, or Tabbing back in later would bring no ring.
    const onWindowFocus = () => {
      if (document.activeElement !== el) clearPointerFocus()
    }
    el.addEventListener("pointerdown", onPointerDown)
    el.addEventListener("keydown", onKeyDown)
    el.addEventListener("blur", onBlur)
    window.addEventListener("focus", onWindowFocus)
    return () => {
      el.removeEventListener("pointerdown", onPointerDown)
      el.removeEventListener("keydown", onKeyDown)
      el.removeEventListener("blur", onBlur)
      window.removeEventListener("focus", onWindowFocus)
      clearPointerFocus()
    }
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
  // The rows virtua draws, the loader first when there is one. Handed over as
  // data with a function to draw one, so only the rows in view are drawn on a
  // render — a reply streaming into a long thread re-renders the thread on
  // every batch, and drawing every row of it each time only for virtua to
  // drop all but the few on screen cost a pass over the whole thread.
  const rows = useMemo<(T | typeof LOAD_OLDER_ROW)[]>(
    () => (hasOlder ? [LOAD_OLDER_ROW, ...items] : items),
    [hasOlder, items]
  )
  const total = items.length
  const renderRow = (row: T | typeof LOAD_OLDER_ROW, index: number) => {
    if (row === LOAD_OLDER_ROW) {
      return (
        <div key="load-older-row" style={styles.first}>
          <div className={cn("mx-auto chat-content-w px-4", className)}>
            <button
              type="button"
              onClick={isLoadingOlder ? undefined : onLoadOlder}
              disabled={isLoadingOlder}
              className="mx-auto flex items-center gap-2 rounded-full border border-border bg-muted/40 px-3 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted disabled:cursor-default disabled:hover:bg-muted/40"
            >
              {isLoadingOlder ? (
                <>
                  <Loader2 className="h-3 w-3 animate-spin" />
                  {loadingOlderLabel}
                </>
              ) : (
                loadOlderLabel
              )}
            </button>
          </div>
        </div>
      )
    }
    const itemIndex = index - rowOffset
    return (
      <div
        key={getItemKey(row, itemIndex)}
        style={itemStyle(itemIndex, total)}
        // A long reply's steps, drawn a stretch at a time inside the row,
        // leave keeping the view still to virtua once the row is above it
        // (see `useTimelineWindow`).
        data-thread-row=""
        data-message-history-row=""
      >
        <div className={cn("mx-auto chat-content-w px-4", className)}>
          {renderItem(row, itemIndex)}
        </div>
      </div>
    )
  }

  return (
    <MessageScrollProvider value={scrollContextValue}>
      <MessageThreadContent
        className={cn("mx-0 max-w-none p-0", contentClassName)}
        scrollClassName="chat-scroll-port scrollbar-thin overscroll-contain [overflow-anchor:none] outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset data-[focus-origin=pointer]:focus-visible:ring-0"
        {...contentProps}
      >
        {showEmpty ? (
          (emptyState ?? null)
        ) : (
          <>
            {header ? (
              <div
                data-message-history-header
                className={cn(
                  "mx-auto min-h-14 w-full chat-content-w px-4 pt-4",
                  headerClassName
                )}
              >
                {header}
              </div>
            ) : null}
            {items.length > 0 || hasOlder ? (
              <Virtualizer
                ref={virtualizerHandleRef}
                as={ThreadContainer}
                scrollRef={
                  scrollRef as unknown as RefObject<HTMLElement | null>
                }
                itemSize={itemSize}
                bufferSize={bufferSize}
                shift={shift}
                keepMounted={keepMounted}
                onScroll={handleScroll}
                data={rows}
              >
                {renderRow}
              </Virtualizer>
            ) : null}
            {footer ? (
              <div
                ref={footerShellRef}
                data-message-live-footer
                className={cn(
                  "mx-auto w-full chat-content-w px-4 pb-4",
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
