/**
 * Shared ref-counted 1s ticker for running delegation cards.
 *
 * Interest (`retainRunningTicker`) starts/stops a single module-level interval.
 * Subscriptions (`subscribeRunningTicker` + `getRunningTickerVersion`) power
 * `useSyncExternalStore` so mounted running cards recompute `elapsedMs` on
 * each tick without each card owning a timer.
 *
 * StrictMode-safe: each retain returns its own release; double-mount
 * (retain → release → retain) keeps the refcount correct; release is
 * idempotent so a double cleanup cannot drive the count negative.
 *
 * Interval runs only while retain-count > 0. Terminal / non-running cards
 * must not call retain (see Task 7 eligibility: lifecycleStatus === "running"
 * && valid startedAt).
 */

type Listener = () => void

const TICK_MS = 1_000

let retainCount = 0
let timer: ReturnType<typeof setInterval> | null = null
let version = 0
const listeners = new Set<Listener>()

function notify(): void {
  for (const listener of listeners) {
    listener()
  }
}

function startTimer(): void {
  if (timer !== null) return
  timer = setInterval(() => {
    version += 1
    notify()
  }, TICK_MS)
}

function stopTimer(): void {
  if (timer === null) return
  clearInterval(timer)
  timer = null
}

/**
 * Register interest in the shared ticker. The first retain starts the 1s
 * interval; the last release stops it. Returns a release function that is
 * safe to call more than once.
 */
export function retainRunningTicker(): () => void {
  retainCount += 1
  if (retainCount === 1) {
    startTimer()
  }

  let released = false
  return () => {
    if (released) return
    released = true
    retainCount = Math.max(0, retainCount - 1)
    if (retainCount === 0) {
      stopTimer()
    }
  }
}

/**
 * Subscribe to tick notifications. Does **not** start the interval by itself
 * — pair with `retainRunningTicker` while a running card is mounted.
 * Suitable as the `subscribe` argument to `useSyncExternalStore`.
 */
export function subscribeRunningTicker(listener: Listener): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/**
 * Monotonic version bumped on every tick. Suitable as the `getSnapshot`
 * argument to `useSyncExternalStore`; consumers recompute `Date.now()` (or
 * equivalent) when the version changes.
 */
export function getRunningTickerVersion(): number {
  return version
}
