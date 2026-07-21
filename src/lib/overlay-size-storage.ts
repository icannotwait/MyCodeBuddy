/**
 * Persist resizable overlay dimensions (width + list max-height).
 * Used by the sub-agent floating card; not the shell sidebar panels.
 */

export const SUB_AGENT_OVERLAY_SIZE_KEY = "workspace:sub-agent-overlay-size"

/** Matches former Tailwind `w-72`. */
export const DEFAULT_OVERLAY_WIDTH = 288
export const MIN_OVERLAY_WIDTH = 224
export const MAX_OVERLAY_WIDTH = 448

/** Matches former Tailwind `max-h-96` on the list body. */
export const DEFAULT_OVERLAY_MAX_HEIGHT = 384
export const MIN_OVERLAY_MAX_HEIGHT = 120
export const MAX_OVERLAY_MAX_HEIGHT = 560

export interface OverlaySize {
  width: number
  maxHeight: number
}

export function clampOverlayWidth(width: number): number {
  if (!Number.isFinite(width)) return DEFAULT_OVERLAY_WIDTH
  return Math.max(MIN_OVERLAY_WIDTH, Math.min(MAX_OVERLAY_WIDTH, width))
}

export function clampOverlayMaxHeight(maxHeight: number): number {
  if (!Number.isFinite(maxHeight)) return DEFAULT_OVERLAY_MAX_HEIGHT
  return Math.max(
    MIN_OVERLAY_MAX_HEIGHT,
    Math.min(MAX_OVERLAY_MAX_HEIGHT, maxHeight)
  )
}

export function clampOverlaySize(size: OverlaySize): OverlaySize {
  return {
    width: clampOverlayWidth(size.width),
    maxHeight: clampOverlayMaxHeight(size.maxHeight),
  }
}

export function defaultOverlaySize(): OverlaySize {
  return {
    width: DEFAULT_OVERLAY_WIDTH,
    maxHeight: DEFAULT_OVERLAY_MAX_HEIGHT,
  }
}

/**
 * Next list max-height while dragging.
 * - Always clamp to min/max constants.
 * - Never grow past full content height (no empty stretch).
 * - When content does not fill the current cap, refuse to grow
 *   (user can only shrink until content hits the edge again).
 */
export function nextOverlayMaxHeight(args: {
  startMaxHeight: number
  deltaY: number
  contentHeight: number
}): number {
  const { startMaxHeight, deltaY, contentHeight } = args
  const start = clampOverlayMaxHeight(startMaxHeight)
  const content = Math.max(0, contentHeight)
  const raw = start + deltaY

  if (raw > start) {
    // Content must already be at (or past) the cap to allow growth.
    if (content < start - 1) {
      return start
    }
    // Cap growth at full content height so the card never pads empty space.
    const capped = Math.min(raw, Math.max(content, start))
    return clampOverlayMaxHeight(capped)
  }

  return clampOverlayMaxHeight(raw)
}

export function loadOverlaySize(storageKey: string): OverlaySize {
  if (typeof window === "undefined") return defaultOverlaySize()

  try {
    const raw = localStorage.getItem(storageKey)
    if (!raw) return defaultOverlaySize()
    const parsed = JSON.parse(raw) as Partial<OverlaySize>
    if (
      typeof parsed.width !== "number" ||
      Number.isNaN(parsed.width) ||
      typeof parsed.maxHeight !== "number" ||
      Number.isNaN(parsed.maxHeight)
    ) {
      return defaultOverlaySize()
    }
    return clampOverlaySize({
      width: parsed.width,
      maxHeight: parsed.maxHeight,
    })
  } catch {
    return defaultOverlaySize()
  }
}

export function saveOverlaySize(storageKey: string, size: OverlaySize): void {
  if (typeof window === "undefined") return

  try {
    localStorage.setItem(
      storageKey,
      JSON.stringify(clampOverlaySize(size))
    )
  } catch {
    /* ignore quota / private mode */
  }
}
