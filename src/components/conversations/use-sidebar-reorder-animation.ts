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

function cloneMeasuredMap(
  source: ReadonlyMap<string, SidebarMeasuredRow>
): Map<string, SidebarMeasuredRow> {
  return new Map(
    [...source.entries()].map(([key, row]) => [
      key,
      {
        key: row.key,
        rootId: row.rootId,
        top: row.top,
        bottom: row.bottom,
      },
    ])
  )
}

function sameStringOrder(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false
  }
  return true
}

function sameNumberOrder(a: readonly number[], b: readonly number[]): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false
  }
  return true
}

/**
 * True when two snapshots describe the same structural keys, root order, and
 * per-root block membership — an "equivalent" same-sequence rows identity.
 */
function sidebarSnapshotsEquivalent(
  a: SidebarRootOrderSnapshot,
  b: SidebarRootOrderSnapshot
): boolean {
  if (!sameStringOrder(a.structuralRowKeys, b.structuralRowKeys)) return false
  if (a.bucketByRoot.size !== b.bucketByRoot.size) return false
  for (const [rootId, bucket] of a.bucketByRoot) {
    if (b.bucketByRoot.get(rootId) !== bucket) return false
  }
  if (a.rootsByBucket.size !== b.rootsByBucket.size) return false
  for (const [bucket, rootsA] of a.rootsByBucket) {
    const rootsB = b.rootsByBucket.get(bucket)
    if (!rootsB || !sameNumberOrder(rootsA, rootsB)) return false
  }
  if (a.blockRowKeysByRoot.size !== b.blockRowKeysByRoot.size) return false
  for (const [rootId, keysA] of a.blockRowKeysByRoot) {
    const keysB = b.blockRowKeysByRoot.get(rootId)
    if (!keysB || !sameStringOrder(keysA, keysB)) return false
  }
  return true
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
 *
 * In-flight waves are preserved across equivalent same-sequence rerenders.
 * Visual First for retarget comes from an active rAF sampler cache so the
 * first frame of a new wave matches the last painted frame (no snap).
 */
export function useSidebarReorderAnimation(
  options: UseSidebarReorderAnimationOptions
): SidebarReorderAnimationControls {
  const optionsRef = useRef(options)

  const priorSnapshotRef = useRef<SidebarRootOrderSnapshot | null>(null)
  const priorMeasuredRef = useRef<Map<string, SidebarMeasuredRow> | null>(null)
  const visualCacheRef = useRef<Map<string, SidebarMeasuredRow> | null>(null)
  const activeAnimationsRef = useRef<ActiveAnimation[]>([])
  const consumedSequenceRef = useRef<number | null>(null)
  const suppressUserScrollRef = useRef(false)
  const suppressRafRef = useRef<number | null>(null)
  const samplerRafRef = useRef<number | null>(null)
  const viewportElRef = useRef<HTMLElement | null>(options.viewportEl)

  // Keep latest options in a ref so scroll handlers / sampler ticks never close
  // over a stale render without forcing effect cleanup on every identity change.
  useIsomorphicLayoutEffect(() => {
    optionsRef.current = options
  })

  const stopVisualSampler = useCallback(() => {
    if (samplerRafRef.current != null) {
      cancelAnimationFrame(samplerRafRef.current)
      samplerRafRef.current = null
    }
  }, [])

  const sampleVisualCache = useCallback((viewport: HTMLElement) => {
    const { measured } = captureViewportNodes(viewport)
    visualCacheRef.current = measured
    return measured
  }, [])

  const startVisualSampler = useCallback(
    (viewport: HTMLElement) => {
      stopVisualSampler()
      sampleVisualCache(viewport)
      if (typeof requestAnimationFrame !== "function") return

      const tick = () => {
        if (activeAnimationsRef.current.length === 0) {
          samplerRafRef.current = null
          return
        }
        const liveViewport = optionsRef.current.viewportEl
        if (!liveViewport) {
          samplerRafRef.current = null
          return
        }
        sampleVisualCache(liveViewport)
        samplerRafRef.current = requestAnimationFrame(tick)
      }
      samplerRafRef.current = requestAnimationFrame(tick)
    },
    [sampleVisualCache, stopVisualSampler]
  )

  const cancelActiveAnimations = useCallback(() => {
    const active = activeAnimationsRef.current
    activeAnimationsRef.current = []
    stopVisualSampler()
    visualCacheRef.current = null
    for (const { element, animation } of active) {
      try {
        animation.cancel()
      } catch {
        // Ignore cancel failures from already-finished animations.
      }
      clearOwnedStyles(element)
    }
  }, [stopVisualSampler])

  /**
   * Freeze and tear down the current wave, preferring the rAF visual cache as
   * the next First snapshot. Returns that First map (or null when idle).
   */
  const retargetFreezeActiveWave = useCallback(
    (viewport: HTMLElement): Map<string, SidebarMeasuredRow> | null => {
      const active = activeAnimationsRef.current
      if (active.length === 0 && visualCacheRef.current == null) {
        return null
      }

      // Copy the last painted sample before commit/cancel mutates styles.
      const cached =
        visualCacheRef.current != null
          ? cloneMeasuredMap(visualCacheRef.current)
          : null

      for (const { animation } of active) {
        tryCommitStyles(animation)
      }

      // Fallback: if the sampler never ran, measure after commitStyles while
      // freezes still hold mid-flight transforms.
      const first =
        cached ??
        (active.length > 0
          ? cloneMeasuredMap(captureViewportNodes(viewport).measured)
          : null)

      for (const { element, animation } of active) {
        try {
          animation.cancel()
        } catch {
          // ignore
        }
        clearOwnedStyles(element)
      }
      activeAnimationsRef.current = []
      stopVisualSampler()
      visualCacheRef.current = null
      return first
    },
    [stopVisualSampler]
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

      // Single completion path: `finished` resolves on natural end and rejects
      // on cancel. Clear styles/handles exactly once when still tracked.
      void animation.finished
        .then(() => {
          if (!activeAnimationsRef.current.includes(entry)) return
          clearOwnedStyles(element)
          activeAnimationsRef.current = activeAnimationsRef.current.filter(
            (item) => item !== entry
          )
          if (activeAnimationsRef.current.length === 0) {
            stopVisualSampler()
            visualCacheRef.current = null
          }
        })
        .catch(() => {
          // External cancel (or other rejection) may leave the entry tracked when
          // our own cancelActiveAnimations path did not run. Clear ownership.
          if (!activeAnimationsRef.current.includes(entry)) return
          clearOwnedStyles(element)
          activeAnimationsRef.current = activeAnimationsRef.current.filter(
            (item) => item !== entry
          )
          if (activeAnimationsRef.current.length === 0) {
            stopVisualSampler()
            visualCacheRef.current = null
          }
        })
    },
    [stopVisualSampler]
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

  const rebaseToCurrent = useCallback(
    (viewport: HTMLElement, rows: readonly SidebarRow[]) => {
      cancelActiveAnimations()
      const capture = captureViewportNodes(viewport)
      priorMeasuredRef.current = capture.measured
      priorSnapshotRef.current = buildSidebarRootOrderSnapshot(rows)
    },
    [cancelActiveAnimations]
  )

  const handleUserScroll = useCallback(() => {
    if (suppressUserScrollRef.current) return
    const viewportEl = optionsRef.current.viewportEl
    if (viewportEl) {
      rebaseToCurrent(viewportEl, optionsRef.current.rows)
    } else {
      cancelActiveAnimations()
    }
  }, [cancelActiveAnimations, rebaseToCurrent])

  useIsomorphicLayoutEffect(() => {
    const {
      rows,
      activitySequence,
      activityConversationId,
      viewportEl,
      dragging,
    } = options

    // Viewport replacement: cancel any wave bound to the previous element.
    if (viewportElRef.current !== viewportEl) {
      if (viewportElRef.current != null && activeAnimationsRef.current.length) {
        cancelActiveAnimations()
      }
      stopVisualSampler()
      visualCacheRef.current = null
      priorMeasuredRef.current = null
      priorSnapshotRef.current = null
      viewportElRef.current = viewportEl
    }

    if (!viewportEl) {
      priorSnapshotRef.current = buildSidebarRootOrderSnapshot(rows)
      // Do not cancel via effect cleanup — unmount / explicit paths own that.
      return
    }

    // Dragging: cancel/rebase and keep geometry current without animating.
    if (dragging) {
      rebaseToCurrent(viewportEl, rows)
      if (
        consumedSequenceRef.current === null ||
        activitySequence > consumedSequenceRef.current
      ) {
        consumedSequenceRef.current = activitySequence
      }
      return
    }

    const afterSnapshot = buildSidebarRootOrderSnapshot(rows)
    let lastCapture = captureViewportNodes(viewportEl)
    let lastMeasured = lastCapture.measured
    const lastElements = new Map(lastCapture.elements)

    const priorSnapshot = priorSnapshotRef.current
    let priorMeasured = priorMeasuredRef.current

    // First observation: seed caches and adopt the current sequence without
    // inventing motion for the initial paint.
    if (priorSnapshot === null || priorMeasured === null) {
      priorSnapshotRef.current = afterSnapshot
      priorMeasuredRef.current = lastMeasured
      consumedSequenceRef.current = activitySequence
      return
    }

    // Store reset (or any sequence regression below the last consumed value):
    // cancel the wave, reseed geometry/snapshot, and adopt the lower sequence
    // so subsequent 1+ activity events can animate again.
    if (
      consumedSequenceRef.current !== null &&
      activitySequence < consumedSequenceRef.current
    ) {
      rebaseToCurrent(viewportEl, rows)
      consumedSequenceRef.current = activitySequence
      return
    }

    const sequenceAdvanced =
      consumedSequenceRef.current !== null &&
      activitySequence > consumedSequenceRef.current

    if (!sequenceAdvanced) {
      // Equivalent structure/order: keep any active 230/120 ms wave running.
      if (sidebarSnapshotsEquivalent(priorSnapshot, afterSnapshot)) {
        priorSnapshotRef.current = afterSnapshot
        if (activeAnimationsRef.current.length === 0) {
          priorMeasuredRef.current = lastMeasured
        }
        return
      }

      // Real non-activity structural or order change: cancel and rebase.
      rebaseToCurrent(viewportEl, rows)
      if (
        consumedSequenceRef.current === null ||
        activitySequence > consumedSequenceRef.current
      ) {
        consumedSequenceRef.current = activitySequence
      }
      return
    }

    // Always consume the advanced sequence, even when animation is skipped.
    consumedSequenceRef.current = activitySequence

    // Advanced sequence with equivalent structure/order (e.g. prompt-start ack
    // after optimistic promotion already placed the root on top): preserve the
    // active wave exactly like the same-sequence equivalent path. Rebase only
    // for actual structural/order change without an eligible reorder.
    if (sidebarSnapshotsEquivalent(priorSnapshot, afterSnapshot)) {
      priorSnapshotRef.current = afterSnapshot
      if (activeAnimationsRef.current.length === 0) {
        priorMeasuredRef.current = lastMeasured
      }
      return
    }

    const activityId = activityConversationId
    const reorder =
      activityId == null
        ? null
        : detectSidebarActivityReorder(priorSnapshot, afterSnapshot, activityId)

    if (!reorder) {
      rebaseToCurrent(viewportEl, rows)
      return
    }

    // Retarget: use last painted visual cache as First, then clear the old wave.
    const frozenFirst = retargetFreezeActiveWave(viewportEl)
    if (frozenFirst) {
      priorMeasured = frozenFirst
      priorMeasuredRef.current = frozenFirst
      // Remeasure pure layout Last after styles were cleared.
      lastCapture = captureViewportNodes(viewportEl)
      lastMeasured = lastCapture.measured
      lastElements.clear()
      for (const [key, element] of lastCapture.elements) {
        lastElements.set(key, element)
      }
    }

    const reducedMotion = prefersReducedMotion()
    const viewportRect = viewportEl.getBoundingClientRect()
    const scrollTop = viewportEl.scrollTop

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
            applyViewportScrollDelta(viewportEl, scrollTop, delta)
            markProgrammaticScrollSuppression()
            // Remeasure Last after the scroll correction (client tops shift).
            const remeasured = captureViewportNodes(viewportEl)
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

      if (activeAnimationsRef.current.length > 0) {
        startVisualSampler(viewportEl)
      }
    }

    priorSnapshotRef.current = afterSnapshot
    priorMeasuredRef.current = lastMeasured

    // Intentionally no cleanup cancel: equivalent same-sequence rerenders must
    // leave the active wave running. Cancel only via explicit paths.
  }, [
    options.rows,
    options.activitySequence,
    options.activityConversationId,
    options.viewportEl,
    options.dragging,
    cancelActiveAnimations,
    markProgrammaticScrollSuppression,
    rebaseToCurrent,
    retargetFreezeActiveWave,
    runMoveAnimation,
    runFadeAnimation,
    startVisualSampler,
    stopVisualSampler,
  ])

  // Unmount disposal only — empty deps so dependency churn cannot cancel.
  useIsomorphicLayoutEffect(() => {
    return () => {
      if (suppressRafRef.current != null) {
        cancelAnimationFrame(suppressRafRef.current)
        suppressRafRef.current = null
      }
      suppressUserScrollRef.current = false
      stopVisualSampler()
      visualCacheRef.current = null
      const active = activeAnimationsRef.current
      activeAnimationsRef.current = []
      for (const { element, animation } of active) {
        try {
          animation.cancel()
        } catch {
          // ignore
        }
        clearOwnedStyles(element)
      }
    }
  }, [stopVisualSampler])

  const controls = useMemo(
    () => ({
      handleUserScroll,
    }),
    [handleUserScroll]
  )

  return controls
}
