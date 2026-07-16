/**
 * Schedule non-urgent work during browser idle time, with a hard timeout and a
 * WKWebView-safe setTimeout fallback (requestIdleCallback is absent there).
 *
 * Returns a dispose function that cancels the scheduled callback if it has not
 * already run.
 */
export function scheduleIdleWork(
  callback: () => void,
  options: { timeoutMs: number }
): () => void {
  let active = true
  const run = () => {
    if (!active) return
    active = false
    callback()
  }
  if (typeof window.requestIdleCallback === "function") {
    const handle = window.requestIdleCallback(run, {
      timeout: options.timeoutMs,
    })
    return () => {
      active = false
      if (typeof window.cancelIdleCallback === "function") {
        window.cancelIdleCallback(handle)
      }
    }
  }
  const handle = window.setTimeout(run, options.timeoutMs)
  return () => {
    active = false
    window.clearTimeout(handle)
  }
}
