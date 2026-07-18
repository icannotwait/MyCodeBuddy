import { useCallback, useEffect, useLayoutEffect, useMemo, useRef } from "react"

import type { SidebarRow } from "./sidebar-conversation-grouping"
import {
  buildSidebarRootOrderSnapshot,
  detectSidebarActivityReorder,
  selectSidebarAnchor,
  sidebarAnchorScrollDelta,
  sidebarFlipDeltaY,
  type SidebarMeasuredRow,
  type SidebarRootOrderSnapshot,
} from "./sidebar-reorder-animation"

export interface UseSidebarReorderAnimationOptions {
  rows: readonly SidebarRow[]
  activitySequence: number
  activityConversationId: number | null
  viewportEl: HTMLElement | null
  dragging: boolean
}

export interface SidebarReorderAnimationControls {
  handleUserScroll(): void
}

const MOVE_DURATION_MS = 230
const FADE_DURATION_MS = 120
const EASING = "cubic-bezier(0.2, 0, 0, 1)"
const FILL: FillMode = "both"

const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect

interface ActiveAnimation {
  element: HTMLElement
  animation: Animation
}

interface CaptureResult {
  measured: Map<string, SidebarMeasuredRow>
  elements: Map<string, HTMLElement>
}

function prefersReducedMotion(): boolean {
  if (
    typeof window === "undefined" ||
    typeof window.matchMedia !== "function"
  ) {
    return false
  }
  try {
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches
  } catch {
    return false
  }
}

function clearOwnedStyles(element: HTMLElement): void {
  element.style.transform = ""
  element.style.opacity = ""
}

function parseRootId(raw: string | undefined): number | null {
  if (raw === undefined || raw === "") return null
  const value = Number(raw)
  return Number.isFinite(value) ? value : null
}

function captureViewportNodes(viewportEl: HTMLElement): CaptureResult {
  const measured = new Map<string, SidebarMeasuredRow>()
  const elements = new Map<string, HTMLElement>()
  const nodes = viewportEl.querySelectorAll<HTMLElement>(
    "[data-sidebar-row-key]"
  )

  for (const element of nodes) {
    const key = element.dataset.sidebarRowKey
    if (!key) continue
    const rect = element.getBoundingClientRect()
    const rootId = parseRootId(element.dataset.sidebarRootId)
    measured.set(key, {
      key,
      rootId,
      top: rect.top,
      bottom: rect.bottom,
    })
    elements.set(key, element)
  }

  return { measured, elements }
}

function canAnimate(element: HTMLElement): boolean {
  return typeof element.animate === "function"
}

function applyViewportScrollDelta(
  viewport: HTMLElement,
  currentScrollTop: number,
  delta: number
): void {
  viewport.scrollTop = currentScrollTop + delta
}

function tryCommitStyles(animation: Animation): void {
  const withCommit = animation as Animation & { commitStyles?: () => void }
  if (typeof withCommit.commitStyles !== "function") return
  try {
    withCommit.commitStyles()
  } catch {
    // commitStyles may throw when the animation has no effect target.
  }
}

/**
 * Sidebar-local FLIP controller for painted same-bucket root reorders.
 * Owns only transform/opacity and Animation handles on stable descendants
 * marked `data-sidebar-row-key` inside the supplied viewport element.
 */
export function useSidebarReorderAnimation(
  options: UseSidebarReorderAnimationOptions
): SidebarReorderAnimationControls {
  const optionsRef = useRef(options)
  const priorSnapshotRef = useRef<SidebarRootOrderSnapshot | null>(null)
  const priorMeasuredRef = useRef<Map<string, SidebarMeasuredRow> | null>(null)
  const activeAnimationsRef = useRef<ActiveAnimation[]>([])
  const consumedSequenceRef = useRef<number | null>(null)
  const suppressUserScrollRef = useRef(false)
  const suppressRafRef = useRef<number | null>(null)

  useIsomorphicLayoutEffect(() => {
    optionsRef.current = options
  })

  const cancelActiveAnimations = useCallback(() => {
    const active = activeAnimationsRef.current
    activeAnimationsRef.current = []
    for (const { element, animation } of active) {
      try {
        animation.cancel()
      } catch {
        // Ignore cancel failures from already-finished animations.
      }
      clearOwnedStyles(element)
    }
  }, [])

  /**
   * Layout-effect cleanup path: freeze in-flight visual state, optionally
   * capture those visual rectangles as the next First snapshot when animations
   * were active, then cancel and clear controller-owned styles.
   *
   * When no animations are active, priorMeasuredRef already holds the previous
   * layout Last (the correct First for the upcoming commit) and is left alone —
   * remeasuring after React's DOM commit would incorrectly read the new layout.
   */
  const captureFirstFromVisualState = useCallback(
    (viewportEl: HTMLElement | null) => {
      const active = activeAnimationsRef.current
      const hadActive = active.length > 0

      for (const { animation } of active) {
        tryCommitStyles(animation)
      }

      if (hadActive && viewportEl) {
        const { measured } = captureViewportNodes(viewportEl)
        priorMeasuredRef.current = measured
      }

      for (const { element, animation } of active) {
        try {
          animation.cancel()
        } catch {
          // ignore
        }
        clearOwnedStyles(element)
      }
      activeAnimationsRef.current = []
    },
    []
  )

  const markProgrammaticScrollSuppression = useCallback(() => {
    suppressUserScrollRef.current = true
    if (suppressRafRef.current != null) {
      cancelAnimationFrame(suppressRafRef.current)
      suppressRafRef.current = null
    }
    if (typeof requestAnimationFrame === "function") {
      suppressRafRef.current = requestAnimationFrame(() => {
        suppressRafRef.current = null
        suppressUserScrollRef.current = false
      })
    } else {
      suppressUserScrollRef.current = false
    }
  }, [])

  const trackAnimation = useCallback(
    (element: HTMLElement, animation: Animation) => {
      const entry: ActiveAnimation = { element, animation }
      activeAnimationsRef.current.push(entry)
      const cleanup = () => {
        clearOwnedStyles(element)
        activeAnimationsRef.current = activeAnimationsRef.current.filter(
          (item) => item !== entry
        )
      }
      animation.onfinish = cleanup
      void animation.finished.then(cleanup).catch(() => {
        // Cancelled animations reject `finished`; styles are cleared on cancel.
      })
    },
    []
  )

  const runMoveAnimation = useCallback(
    (element: HTMLElement, deltaY: number) => {
      if (!canAnimate(element)) return
      try {
        const animation = element.animate(
          [
            { transform: `translateY(${deltaY}px)` },
            { transform: "translateY(0px)" },
          ],
          {
            duration: MOVE_DURATION_MS,
            easing: EASING,
            fill: FILL,
          }
        )
        trackAnimation(element, animation)
      } catch {
        clearOwnedStyles(element)
      }
    },
    [trackAnimation]
  )

  const runFadeAnimation = useCallback(
    (element: HTMLElement) => {
      if (!canAnimate(element)) return
      try {
        const animation = element.animate([{ opacity: 0 }, { opacity: 1 }], {
          duration: FADE_DURATION_MS,
          easing: EASING,
          fill: FILL,
        })
        trackAnimation(element, animation)
      } catch {
        clearOwnedStyles(element)
      }
    },
    [trackAnimation]
  )

  const handleUserScroll = useCallback(() => {
    if (suppressUserScrollRef.current) return
    cancelActiveAnimations()
    const viewportEl = optionsRef.current.viewportEl
    if (viewportEl) {
      const { measured } = captureViewportNodes(viewportEl)
      priorMeasuredRef.current = measured
      priorSnapshotRef.current = buildSidebarRootOrderSnapshot(
        optionsRef.current.rows
      )
    }
  }, [cancelActiveAnimations])

  useIsomorphicLayoutEffect(() => {
    const {
      rows,
      activitySequence,
      activityConversationId,
      viewportEl,
      dragging,
    } = options

    // Local alias so scroll/DOM mutations are not attributed to hook args.
    const viewport = viewportEl

    if (!viewport) {
      priorSnapshotRef.current = buildSidebarRootOrderSnapshot(rows)
      return () => {
        captureFirstFromVisualState(null)
      }
    }

    // Dragging: cancel/rebase and keep geometry current without animating.
    if (dragging) {
      cancelActiveAnimations()
      const capture = captureViewportNodes(viewport)
      priorMeasuredRef.current = capture.measured
      priorSnapshotRef.current = buildSidebarRootOrderSnapshot(rows)
      if (
        consumedSequenceRef.current === null ||
        activitySequence > consumedSequenceRef.current
      ) {
        consumedSequenceRef.current = activitySequence
      }
      return () => {
        captureFirstFromVisualState(viewport)
      }
    }

    const afterSnapshot = buildSidebarRootOrderSnapshot(rows)
    const lastCapture = captureViewportNodes(viewport)
    let lastMeasured = lastCapture.measured
    const lastElements = new Map(lastCapture.elements)

    const priorSnapshot = priorSnapshotRef.current
    const priorMeasured = priorMeasuredRef.current

    // First observation: seed caches and adopt the current sequence without
    // inventing motion for the initial paint.
    if (priorSnapshot === null || priorMeasured === null) {
      priorSnapshotRef.current = afterSnapshot
      priorMeasuredRef.current = lastMeasured
      consumedSequenceRef.current = activitySequence
      return () => {
        captureFirstFromVisualState(viewport)
      }
    }

    const sequenceAdvanced =
      consumedSequenceRef.current !== null &&
      activitySequence > consumedSequenceRef.current

    if (!sequenceAdvanced) {
      // No new activity: keep geometry cache aligned with the painted tree.
      priorSnapshotRef.current = afterSnapshot
      priorMeasuredRef.current = lastMeasured
      return () => {
        captureFirstFromVisualState(viewport)
      }
    }

    // Always consume the advanced sequence, even when animation is skipped.
    consumedSequenceRef.current = activitySequence

    const activityId = activityConversationId
    const reorder =
      activityId == null
        ? null
        : detectSidebarActivityReorder(priorSnapshot, afterSnapshot, activityId)

    if (!reorder) {
      cancelActiveAnimations()
      priorSnapshotRef.current = afterSnapshot
      priorMeasuredRef.current = lastMeasured
      return () => {
        captureFirstFromVisualState(viewport)
      }
    }

    const reducedMotion = prefersReducedMotion()
    const viewportRect = viewport.getBoundingClientRect()
    const scrollTop = viewport.scrollTop

    // Anchor correction before paint when scrolled away from the absolute top.
    if (scrollTop > 0) {
      const survivingKeys = new Set(lastMeasured.keys())
      const anchor = selectSidebarAnchor(
        priorMeasured,
        survivingKeys,
        viewportRect.top,
        viewportRect.bottom,
        reorder.conversationId
      )
      if (anchor) {
        const afterAnchor = lastMeasured.get(anchor.key)
        if (afterAnchor) {
          const delta = sidebarAnchorScrollDelta(anchor.top, afterAnchor.top)
          if (delta !== 0) {
            applyViewportScrollDelta(viewport, scrollTop, delta)
            markProgrammaticScrollSuppression()
            // Remeasure Last after the scroll correction.
            const remeasured = captureViewportNodes(viewport)
            lastMeasured = remeasured.measured
            for (const [key, element] of remeasured.elements) {
              lastElements.set(key, element)
            }
          }
        }
      }
    }

    if (!reducedMotion) {
      const promotedRootId = reorder.conversationId

      // Shared painted keys: FLIP from First visual top to Last layout top.
      for (const [key, lastRow] of lastMeasured) {
        const firstRow = priorMeasured.get(key)
        if (!firstRow) continue
        const deltaY = sidebarFlipDeltaY(firstRow.top, lastRow.top)
        if (deltaY === 0) continue
        const element = lastElements.get(key)
        if (!element) continue
        runMoveAnimation(element, deltaY)
      }

      // Offscreen promotion: keys of the promoted root present in Last but
      // absent from First fade in (never invent a slide path).
      for (const [key, lastRow] of lastMeasured) {
        if (priorMeasured.has(key)) continue
        if (lastRow.rootId !== promotedRootId) continue
        const element = lastElements.get(key)
        if (!element) continue
        runFadeAnimation(element)
      }
    }

    priorSnapshotRef.current = afterSnapshot
    priorMeasuredRef.current = lastMeasured

    return () => {
      captureFirstFromVisualState(viewport)
    }
  }, [
    options.rows,
    options.activitySequence,
    options.activityConversationId,
    options.viewportEl,
    options.dragging,
    cancelActiveAnimations,
    captureFirstFromVisualState,
    markProgrammaticScrollSuppression,
    runMoveAnimation,
    runFadeAnimation,
  ])

  // Unmount disposal for rAF handles; animation cancel is also in effect cleanup.
  useIsomorphicLayoutEffect(() => {
    return () => {
      if (suppressRafRef.current != null) {
        cancelAnimationFrame(suppressRafRef.current)
        suppressRafRef.current = null
      }
      suppressUserScrollRef.current = false
      cancelActiveAnimations()
    }
  }, [cancelActiveAnimations])

  const controls = useMemo(
    () => ({
      handleUserScroll,
    }),
    [handleUserScroll]
  )

  return controls
}
