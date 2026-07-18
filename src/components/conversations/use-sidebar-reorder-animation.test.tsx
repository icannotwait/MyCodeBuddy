import {
  useCallback,
  useLayoutEffect,
  useRef,
  useState,
  type ReactElement,
} from "react"
import { act, render } from "@testing-library/react"
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
  type Mock,
} from "vitest"

import type { DbConversationSummary } from "@/lib/types"
import type {
  ConversationRow,
  SidebarBucketKey,
  SidebarRow,
} from "./sidebar-conversation-grouping"
import { sidebarRowKey } from "./sidebar-conversation-grouping"
import {
  useSidebarReorderAnimation,
  type SidebarReorderAnimationControls,
} from "./use-sidebar-reorder-animation"

type Tops = Readonly<Record<string, number>>

type AnimateCall = {
  keyframes: Keyframe[]
  options: KeyframeAnimationOptions
  element: HTMLElement
}

type MockAnimation = {
  cancel: Mock
  commitStyles: Mock
  finished: Promise<void>
  onfinish: ((this: Animation, ev: AnimationPlaybackEvent) => void) | null
  playState: string
  resolveFinished: () => void
  rejectFinished: (reason?: unknown) => void
}

const ROW_HEIGHT = 32
const MOVE_DURATION = 230
const FADE_DURATION = 120
const EASING = "cubic-bezier(0.2, 0, 0, 1)"
const FILL = "both"

const animateCalls: AnimateCall[] = []
let animateMock: Mock
let originalAnimate: typeof Element.prototype.animate
let matchMediaMock: Mock
let rafQueue: FrameRequestCallback[] = []
let rafId = 1

function root(
  id: number,
  bucketKey: SidebarBucketKey = "folder:10",
  folderId = 10,
  rootId = id,
  depth = 0
): ConversationRow {
  return {
    kind: "conversation",
    conversation: {
      id,
      agent_type: "claude_code",
      folder_id: folderId,
    } as DbConversationSummary,
    depth,
    rootId,
    bucketKey,
  }
}

function rowsForFolder(roots: ConversationRow[]): SidebarRow[] {
  return [
    { kind: "section", section: "folders", expanded: true, count: 1 },
    { kind: "folder", folderId: 10 },
    ...roots,
  ]
}

const beforeRows = rowsForFolder([root(1), root(2), root(3)])
const afterRows = rowsForFolder([root(3), root(1), root(2)])

/**
 * Content-offset tops (relative to the scrollable content origin).
 * Client top = viewportTop + contentTop - scrollTop + translateY.
 */
const beforeTops: Tops = {
  "section-folders": 0,
  "folder-10": 32,
  "conv-claude_code-1": 100,
  "conv-claude_code-2": 132,
  "conv-claude_code-3": 164,
}

const afterTops: Tops = {
  "section-folders": 0,
  "folder-10": 32,
  "conv-claude_code-3": 100,
  "conv-claude_code-1": 132,
  "conv-claude_code-2": 164,
}

/**
 * Anchor scenario uses the same content offsets. With viewportTop=100 and
 * scrollTop=80, client tops are 120/152/184 so structural headers sit above
 * the fully-visible band and cannot steal the anchor.
 */
const anchorBeforeTops = beforeTops
const anchorAfterTops = afterTops

/** Viewport band that excludes the structural headers above. */
const ANCHOR_VIEWPORT_TOP = 100
const ANCHOR_VIEWPORT_BOTTOM = 300

function keyOf(id: number): string {
  return `conv-claude_code-${id}`
}

function ownedMeta(row: SidebarRow): {
  rootId?: number
  bucketKey?: SidebarBucketKey
} {
  if (row.kind === "conversation" || row.kind === "subsession-loading") {
    return { rootId: row.rootId, bucketKey: row.bucketKey }
  }
  return {}
}

function parseTranslateY(transform: string): number {
  const match = /translateY\((-?\d+(?:\.\d+)?)px\)/.exec(transform)
  return match ? Number(match[1]) : 0
}

function findViewport(el: HTMLElement): HTMLElement | null {
  return el.closest("[data-testid='viewport']") as HTMLElement | null
}

function installNodeRect(el: HTMLElement, viewportTop: number): void {
  Object.defineProperty(el, "getBoundingClientRect", {
    configurable: true,
    value: () => {
      const contentTop = Number(el.dataset.top ?? "0")
      const height = Number(el.dataset.height ?? String(ROW_HEIGHT))
      const viewport = findViewport(el)
      const scrollTop = viewport?.scrollTop ?? 0
      const visualTop =
        viewportTop +
        contentTop -
        scrollTop +
        parseTranslateY(el.style.transform || "")
      return {
        x: 0,
        y: visualTop,
        top: visualTop,
        bottom: visualTop + height,
        left: 0,
        right: 240,
        width: 240,
        height,
        toJSON() {
          return {}
        },
      } satisfies DOMRect
    },
  })
}

function installViewportRect(el: HTMLElement, top = 0, bottom = 400): void {
  Object.defineProperty(el, "getBoundingClientRect", {
    configurable: true,
    value: () =>
      ({
        x: 0,
        y: top,
        top,
        bottom,
        left: 0,
        right: 280,
        width: 280,
        height: bottom - top,
        toJSON() {
          return {}
        },
      }) satisfies DOMRect,
  })
}

function createMockAnimation(element: HTMLElement): MockAnimation {
  let settled = false
  let resolveFinished!: () => void
  let rejectFinished!: (reason?: unknown) => void
  const finished = new Promise<void>((resolve, reject) => {
    resolveFinished = resolve
    rejectFinished = reject
  })
  // Swallow rejection so cancel-driven Abort does not surface as unhandled.
  void finished.catch(() => {})

  const animation: MockAnimation = {
    cancel: vi.fn(() => {
      element.style.transform = ""
      element.style.opacity = ""
      animation.playState = "idle"
      if (!settled) {
        settled = true
        rejectFinished(new DOMException("Animation cancelled", "AbortError"))
      }
    }),
    commitStyles: vi.fn(() => {
      // Prefer the currently painted transform (mid-flight sample); otherwise
      // freeze the opening keyframe so retarget can still read a visual top.
      if (element.style.transform || element.style.opacity) {
        return
      }
      const lastCall = [...animateCalls]
        .reverse()
        .find((call) => call.element === element)
      const firstFrame = lastCall?.keyframes[0] as
        | { transform?: string; opacity?: string | number }
        | undefined
      if (firstFrame?.transform) {
        element.style.transform = firstFrame.transform
      }
      if (firstFrame?.opacity !== undefined) {
        element.style.opacity = String(firstFrame.opacity)
      }
    }),
    finished,
    onfinish: null,
    playState: "running",
    resolveFinished: () => {
      if (settled) return
      settled = true
      animation.playState = "finished"
      resolveFinished()
    },
    rejectFinished: (reason?: unknown) => {
      if (settled) return
      settled = true
      rejectFinished(reason)
    },
  }
  return animation
}

function finishAnimation(animation: MockAnimation): void {
  animation.resolveFinished()
}

function flushAnimationFrame(): void {
  const queue = rafQueue
  rafQueue = []
  for (const cb of queue) {
    cb(performance.now())
  }
}

function moveDeltaForKey(key: string): number | null {
  const call = [...animateCalls].reverse().find((entry) => {
    if (entry.element.dataset.sidebarRowKey !== key) return false
    const frame = entry.keyframes[0] as { transform?: string }
    return typeof frame.transform === "string"
  })
  if (!call) return null
  const frame = call.keyframes[0] as { transform: string }
  return parseTranslateY(frame.transform)
}

interface HarnessProps {
  rows: readonly SidebarRow[]
  sequence: number
  activityId: number | null
  tops: Tops
  /** Keys present in the painted DOM. Defaults to every key in `tops`. */
  paintedKeys?: ReadonlySet<string>
  dragging?: boolean
  scrollTop?: number
  viewportTop?: number
  viewportBottom?: number
  onControls?: (controls: SidebarReorderAnimationControls) => void
}

function Harness({
  rows,
  sequence,
  activityId,
  tops,
  paintedKeys,
  dragging = false,
  scrollTop = 0,
  viewportTop = 0,
  viewportBottom = 400,
  onControls,
}: HarnessProps): ReactElement {
  const viewportRef = useRef<HTMLDivElement | null>(null)
  const [viewportEl, setViewportEl] = useState<HTMLElement | null>(null)

  const setViewport = useCallback((node: HTMLDivElement | null) => {
    viewportRef.current = node
    setViewportEl(node)
  }, [])

  // Keep scrollTop and rect stubs in sync with props before the hook effect.
  useLayoutEffect(() => {
    const el = viewportRef.current
    if (!el) return
    el.scrollTop = scrollTop
    installViewportRect(el, viewportTop, viewportBottom)
    for (const node of el.querySelectorAll<HTMLElement>(
      "[data-sidebar-row-key]"
    )) {
      installNodeRect(node, viewportTop)
    }
  })

  const controls = useSidebarReorderAnimation({
    rows,
    activitySequence: sequence,
    activityConversationId: activityId,
    viewportEl,
    dragging,
  })

  useLayoutEffect(() => {
    onControls?.(controls)
  }, [controls, onControls])

  const visibleKeys = paintedKeys ?? new Set(Object.keys(tops))

  return (
    <div
      ref={setViewport}
      data-testid="viewport"
      style={{ overflow: "auto", height: 400 }}
    >
      {rows.map((row) => {
        const key = sidebarRowKey(row)
        if (!visibleKeys.has(key)) return null
        const top = tops[key]
        if (top === undefined) return null
        const meta = ownedMeta(row)
        return (
          <div
            key={key}
            data-sidebar-row-key={key}
            data-sidebar-root-id={
              meta.rootId !== undefined ? String(meta.rootId) : undefined
            }
            data-sidebar-bucket-key={meta.bucketKey}
            data-top={String(top)}
            data-height={String(ROW_HEIGHT)}
          />
        )
      })}
    </div>
  )
}

beforeEach(() => {
  animateCalls.length = 0
  rafQueue = []
  rafId = 1

  originalAnimate = Element.prototype.animate
  animateMock = vi.fn(function (this: HTMLElement, keyframes, options) {
    animateCalls.push({
      keyframes: keyframes as Keyframe[],
      options: options as KeyframeAnimationOptions,
      element: this,
    })
    return createMockAnimation(this) as unknown as Animation
  })
  Element.prototype.animate = animateMock as typeof Element.prototype.animate

  matchMediaMock = vi.fn((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  }))
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: matchMediaMock,
  })

  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
    rafQueue.push(cb)
    return rafId++
  })
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
    void id
  })
})

afterEach(() => {
  Element.prototype.animate = originalAnimate
  vi.restoreAllMocks()
})

describe("useSidebarReorderAnimation", () => {
  it("animates painted displaced rows for 230 ms with the fixed easing", () => {
    const { rerender } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    // Promoted root 3: first.top 164 → last.top 100 ⇒ deltaY +64
    expect(animateMock).toHaveBeenCalledWith(
      [{ transform: "translateY(64px)" }, { transform: "translateY(0px)" }],
      {
        duration: MOVE_DURATION,
        easing: EASING,
        fill: FILL,
      }
    )

    // Displaced roots 1 and 2: deltaY −32
    const moveCalls = animateCalls.filter((call) => {
      const frame = call.keyframes[0] as { transform?: string }
      return typeof frame.transform === "string"
    })
    const deltas = moveCalls.map((call) => {
      const frame = call.keyframes[0] as { transform: string }
      return parseTranslateY(frame.transform)
    })
    expect(deltas).toEqual(expect.arrayContaining([64, -32, -32]))
    expect(
      moveCalls.every(
        (call) =>
          call.options.duration === MOVE_DURATION &&
          call.options.easing === EASING &&
          call.options.fill === FILL
      )
    ).toBe(true)
  })

  it("fades a promoted root absent from First but mounted in Last", () => {
    // Root 3 was offscreen (not painted) before promotion; now mounted at top.
    const paintedBefore = new Set([
      "section-folders",
      "folder-10",
      "conv-claude_code-1",
      "conv-claude_code-2",
    ])
    const paintedAfter = new Set([
      "section-folders",
      "folder-10",
      "conv-claude_code-3",
      "conv-claude_code-1",
      "conv-claude_code-2",
    ])

    const { rerender } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
        paintedKeys={paintedBefore}
      />
    )

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={afterTops}
        paintedKeys={paintedAfter}
      />
    )

    const fadeCalls = animateCalls.filter((call) => {
      const frame = call.keyframes[0] as { opacity?: string | number }
      return frame.opacity !== undefined
    })
    expect(fadeCalls).toHaveLength(1)
    expect(fadeCalls[0]?.keyframes).toEqual([{ opacity: 0 }, { opacity: 1 }])
    expect(fadeCalls[0]?.options).toMatchObject({
      duration: FADE_DURATION,
      easing: EASING,
      fill: FILL,
    })

    // Promoted root must not also receive a translate animation.
    const promotedEl = fadeCalls[0]?.element
    expect(promotedEl?.dataset.sidebarRowKey).toBe(keyOf(3))
    const translateOnPromoted = animateCalls.filter(
      (call) =>
        call.element === promotedEl &&
        (call.keyframes[0] as { transform?: string }).transform !== undefined
    )
    expect(translateOnPromoted).toHaveLength(0)
  })

  it("adds the anchor delta to scrollTop and uses corrected client coordinates", () => {
    let viewport: HTMLElement | null = null
    const scrollTopReads: number[] = []

    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={anchorBeforeTops}
        scrollTop={80}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
      />
    )
    viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    expect(viewport.scrollTop).toBe(80)

    // Track scrollTop at the moment animate is invoked.
    animateMock.mockImplementation(function (
      this: HTMLElement,
      keyframes,
      options
    ) {
      scrollTopReads.push(viewport!.scrollTop)
      animateCalls.push({
        keyframes: keyframes as Keyframe[],
        options: options as KeyframeAnimationOptions,
        element: this,
      })
      return createMockAnimation(this) as unknown as Animation
    })

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={anchorAfterTops}
        scrollTop={80}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
      />
    )

    // Anchor key conv-2: client 152 → 184 ⇒ +32 applied before WAAPI.
    // After correction, content tops shift by -scrollTop so anchor FLIP is 0
    // and the promoted row uses the corrected last client top (88, not 120).
    expect(viewport.scrollTop).toBe(112)
    expect(scrollTopReads.length).toBeGreaterThan(0)
    expect(scrollTopReads.every((value) => value === 112)).toBe(true)

    expect(moveDeltaForKey(keyOf(2))).toBeNull()
    // first client 184, last client after scroll 112: 100+100-112=88 → +96
    expect(moveDeltaForKey(keyOf(3))).toBe(96)
  })

  it("does not correct scrollTop when already at the absolute top", () => {
    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={anchorBeforeTops}
        scrollTop={0}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
      />
    )

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={anchorAfterTops}
        scrollTop={0}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
      />
    )

    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    expect(viewport.scrollTop).toBe(0)
    expect(animateMock).toHaveBeenCalled()
  })

  it("ignores the programmatic anchor scroll until the next animation frame", () => {
    let controls: SidebarReorderAnimationControls | null = null
    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={anchorBeforeTops}
        scrollTop={80}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
        onControls={(c) => {
          controls = c
        }}
      />
    )

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={anchorAfterTops}
        scrollTop={80}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
        onControls={(c) => {
          controls = c
        }}
      />
    )

    expect(animateMock).toHaveBeenCalled()
    const callsAfterReorder = animateMock.mock.calls.length

    // Programmatic suppression still active — user scroll must not cancel.
    act(() => {
      controls!.handleUserScroll()
    })
    expect(animateMock.mock.calls.length).toBe(callsAfterReorder)

    act(() => {
      flushAnimationFrame()
    })

    // Now a real user scroll cancels.
    const cancelSpies = animateMock.mock.results.map((result) => {
      const anim = result.value as MockAnimation
      return anim.cancel
    })
    act(() => {
      controls!.handleUserScroll()
    })
    expect(cancelSpies.some((spy) => spy.mock.calls.length > 0)).toBe(true)

    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    for (const node of viewport.querySelectorAll<HTMLElement>(
      "[data-sidebar-row-key]"
    )) {
      expect(node.style.transform).toBe("")
      expect(node.style.opacity).toBe("")
    }
  })

  it("cancels active animations and clears transforms on a real user scroll", () => {
    let controls: SidebarReorderAnimationControls | null = null
    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
        onControls={(c) => {
          controls = c
        }}
      />
    )

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={afterTops}
        onControls={(c) => {
          controls = c
        }}
      />
    )

    // No anchor (scrollTop 0) → no suppression frame; user scroll cancels now.
    const cancelSpies = animateMock.mock.results.map(
      (result) => (result.value as MockAnimation).cancel
    )
    expect(cancelSpies.length).toBeGreaterThan(0)

    act(() => {
      controls!.handleUserScroll()
    })

    expect(cancelSpies.every((spy) => spy.mock.calls.length >= 1)).toBe(true)

    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    for (const node of viewport.querySelectorAll<HTMLElement>(
      "[data-sidebar-row-key]"
    )) {
      expect(node.style.transform).toBe("")
      expect(node.style.opacity).toBe("")
    }
  })

  it("retargets a second eligible sequence from a sampled mid-flight visual top", () => {
    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const firstWave = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )
    expect(firstWave.length).toBeGreaterThan(0)
    const firstWaveCount = animateMock.mock.calls.length

    // Nontrivial mid-flight: layout last of root 3 is 100; opening keyframe was
    // +64 (visual 164). Paint an intermediate translateY(40) → visual 140,
    // which is neither the pre-reorder layout top (164) nor the post-commit
    // second-sequence layout top (132).
    const promoted = container.querySelector(
      `[data-sidebar-row-key="${keyOf(3)}"]`
    ) as HTMLElement
    expect(promoted).toBeTruthy()
    promoted.style.transform = "translateY(40px)"

    act(() => {
      flushAnimationFrame()
    })

    // Second promotion: root 2 rises above 3 and 1.
    const secondAfterRows = rowsForFolder([root(2), root(3), root(1)])
    const secondTops: Tops = {
      "section-folders": 0,
      "folder-10": 32,
      "conv-claude_code-2": 100,
      "conv-claude_code-3": 132,
      "conv-claude_code-1": 164,
    }

    rerender(
      <Harness
        rows={secondAfterRows}
        sequence={2}
        activityId={2}
        tops={secondTops}
      />
    )

    for (const anim of firstWave) {
      expect(anim.commitStyles).toHaveBeenCalled()
      expect(anim.cancel).toHaveBeenCalled()
    }

    // New wave started; old handles were cancelled (no stack of dual actives).
    expect(animateMock.mock.calls.length).toBeGreaterThan(firstWaveCount)

    // Sampled First visual top 140 → Last layout 132 ⇒ translateY(8px).
    expect(moveDeltaForKey(keyOf(3))).toBe(8)
  })

  it("does not cancel an active wave on same-sequence equivalent rows rerender", () => {
    const { rerender } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const firstWave = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )
    expect(firstWave.length).toBeGreaterThan(0)
    const callsAfterWave = animateMock.mock.calls.length

    // New array identity, same structure/order, same sequence.
    const equivalentRows = rowsForFolder([root(3), root(1), root(2)])
    rerender(
      <Harness
        rows={equivalentRows}
        sequence={1}
        activityId={3}
        tops={afterTops}
      />
    )

    expect(firstWave.every((anim) => anim.cancel.mock.calls.length === 0)).toBe(
      true
    )
    expect(animateMock.mock.calls.length).toBe(callsAfterWave)
  })

  it("cancels when dragging becomes true", () => {
    const { rerender } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const active = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={afterTops}
        dragging
      />
    )

    expect(active.every((anim) => anim.cancel.mock.calls.length >= 1)).toBe(
      true
    )
  })

  it("cancels on an ineligible structure change and consumes the sequence", () => {
    const { rerender } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const active = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )
    const callsAfterEligible = animateMock.mock.calls.length

    // Membership change → ineligible (root 2 removed).
    const ineligibleRows = rowsForFolder([root(3), root(1)])
    const ineligibleTops: Tops = {
      "section-folders": 0,
      "folder-10": 32,
      "conv-claude_code-3": 100,
      "conv-claude_code-1": 132,
    }

    rerender(
      <Harness
        rows={ineligibleRows}
        sequence={2}
        activityId={3}
        tops={ineligibleTops}
      />
    )

    expect(active.every((anim) => anim.cancel.mock.calls.length >= 1)).toBe(
      true
    )
    // No additional move/fade from the ineligible sequence.
    expect(animateMock.mock.calls.length).toBe(callsAfterEligible)

    // A later identical-looking eligible reorder with a new sequence must not
    // be blocked by a stale unconsumed signal — sequence 2 was consumed.
    const baseRows = rowsForFolder([root(1), root(3)])
    const baseTops: Tops = {
      "section-folders": 0,
      "folder-10": 32,
      "conv-claude_code-1": 100,
      "conv-claude_code-3": 132,
    }
    rerender(
      <Harness rows={baseRows} sequence={2} activityId={null} tops={baseTops} />
    )
    const callsBefore = animateMock.mock.calls.length
    const promoteRows = rowsForFolder([root(3), root(1)])
    const promoteTops: Tops = {
      "section-folders": 0,
      "folder-10": 32,
      "conv-claude_code-3": 100,
      "conv-claude_code-1": 132,
    }
    rerender(
      <Harness
        rows={promoteRows}
        sequence={3}
        activityId={3}
        tops={promoteTops}
      />
    )
    expect(animateMock.mock.calls.length).toBeGreaterThan(callsBefore)
  })

  it("cancels and clears owned styles on unmount", () => {
    const { rerender, unmount, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const active = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )
    const nodes = [
      ...container.querySelectorAll<HTMLElement>("[data-sidebar-row-key]"),
    ]

    unmount()

    expect(active.every((anim) => anim.cancel.mock.calls.length >= 1)).toBe(
      true
    )
    for (const node of nodes) {
      expect(node.style.transform).toBe("")
      expect(node.style.opacity).toBe("")
    }
  })

  it("skips WAAPI under reduced motion but still applies anchor correction", () => {
    matchMediaMock.mockImplementation((query: string) => ({
      matches: query.includes("prefers-reduced-motion"),
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }))

    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={anchorBeforeTops}
        scrollTop={80}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
      />
    )

    rerender(
      <Harness
        rows={afterRows}
        sequence={1}
        activityId={3}
        tops={anchorAfterTops}
        scrollTop={80}
        viewportTop={ANCHOR_VIEWPORT_TOP}
        viewportBottom={ANCHOR_VIEWPORT_BOTTOM}
      />
    )

    expect(animateMock).not.toHaveBeenCalled()
    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    expect(viewport.scrollTop).toBe(112)
  })

  it("degrades to final layout when Element.animate is missing", () => {
    // @ts-expect-error — intentional fallback path
    Element.prototype.animate = undefined

    expect(() => {
      const { rerender } = render(
        <Harness
          rows={beforeRows}
          sequence={0}
          activityId={null}
          tops={beforeTops}
        />
      )
      rerender(
        <Harness
          rows={afterRows}
          sequence={1}
          activityId={3}
          tops={afterTops}
        />
      )
    }).not.toThrow()

    expect(animateCalls).toHaveLength(0)
  })

  it("degrades when element.animate throws without leaking styles or handles", () => {
    animateMock.mockImplementation(function (this: HTMLElement) {
      this.style.transform = "translateY(99px)"
      this.style.opacity = "0.5"
      throw new Error("WAAPI unavailable")
    })

    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    expect(() => {
      rerender(
        <Harness
          rows={afterRows}
          sequence={1}
          activityId={3}
          tops={afterTops}
        />
      )
    }).not.toThrow()

    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    for (const node of viewport.querySelectorAll<HTMLElement>(
      "[data-sidebar-row-key]"
    )) {
      expect(node.style.transform).toBe("")
      expect(node.style.opacity).toBe("")
    }
    // No successfully tracked handles: mock never returned an Animation.
    expect(
      animateMock.mock.results.every(
        (result) => result.type === "throw" || result.value == null
      )
    ).toBe(true)
  })

  it("returns a stable handleUserScroll and controls object identity", () => {
    const seen: SidebarReorderAnimationControls[] = []
    const { rerender } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
        onControls={(c) => {
          seen.push(c)
        }}
      />
    )

    rerender(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
        onControls={(c) => {
          seen.push(c)
        }}
      />
    )

    expect(seen.length).toBeGreaterThanOrEqual(2)
    expect(seen[0]).toBe(seen[1])
    expect(seen[0]?.handleUserScroll).toBe(seen[1]?.handleUserScroll)
  })

  it("clears owned styles when a move animation finishes via onfinish path helper", () => {
    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const animations = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )
    act(() => {
      for (const anim of animations) {
        finishAnimation(anim)
      }
    })

    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    for (const node of viewport.querySelectorAll<HTMLElement>(
      "[data-sidebar-row-key]"
    )) {
      expect(node.style.transform).toBe("")
      expect(node.style.opacity).toBe("")
    }
  })

  it("clears owned styles when the finished promise resolves", async () => {
    const { rerender, container } = render(
      <Harness
        rows={beforeRows}
        sequence={0}
        activityId={null}
        tops={beforeTops}
      />
    )

    rerender(
      <Harness rows={afterRows} sequence={1} activityId={3} tops={afterTops} />
    )

    const animations = animateMock.mock.results.map(
      (result) => result.value as MockAnimation
    )
    expect(animations.length).toBeGreaterThan(0)

    // Resolve finished promises only — do not call onfinish. Production must
    // clean up from the promise path.
    await act(async () => {
      for (const anim of animations) {
        anim.resolveFinished()
      }
      await Promise.resolve()
      await Promise.resolve()
    })

    const viewport = container.querySelector(
      '[data-testid="viewport"]'
    ) as HTMLElement
    for (const node of viewport.querySelectorAll<HTMLElement>(
      "[data-sidebar-row-key]"
    )) {
      expect(node.style.transform).toBe("")
      expect(node.style.opacity).toBe("")
    }
  })
})
