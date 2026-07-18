import { useCallback, useRef, useState } from "react"
import { subscribe } from "@/lib/platform"

export type InstallStreamStatus = "idle" | "running" | "success" | "failed"

/**
 * Shape shared by every `app://*-install` progress event emitted by the Rust
 * backend: agent installs, OpenCode plugin installs, Office CLI installs, etc.
 * Concrete events in `@/lib/types` are structurally compatible with this.
 */
export interface InstallStreamEvent {
  task_id: string
  kind: "started" | "log" | "completed" | "failed"
  payload: string
}

interface InstallStreamState {
  status: InstallStreamStatus
  logs: string[]
  error: string | null
}

/**
 * Subscribes to a streaming install topic and reduces its `started`/`log`/
 * `completed`/`failed` events into `{ status, logs, error }`. `start(taskId)`
 * (re)subscribes and filters events to the given task; `reset()` tears the
 * subscription down and returns to the idle state.
 */
export function useInstallStream<E extends InstallStreamEvent>(
  eventName: string
) {
  const [state, setState] = useState<InstallStreamState>({
    status: "idle",
    logs: [],
    error: null,
  })
  const unsubRef = useRef<(() => void) | null>(null)
  // Flipped by reset()/unmount. Guards the gap between awaiting subscribe() and
  // storing its unsubscribe fn: if the panel tore down meanwhile, we unsubscribe
  // immediately instead of leaking the listener.
  const cancelledRef = useRef(false)

  const start = useCallback(
    async (taskId: string) => {
      cancelledRef.current = false
      setState({ status: "running", logs: [], error: null })

      unsubRef.current?.()

      const unsub = await subscribe<E>(eventName, (event) => {
        if (event.task_id !== taskId) return

        switch (event.kind) {
          case "started":
            setState((prev) => ({ ...prev, status: "running" }))
            break
          case "log":
            setState((prev) => ({
              ...prev,
              logs: [...prev.logs, event.payload],
            }))
            break
          case "completed":
            setState((prev) => ({
              ...prev,
              status: "success",
              logs: [...prev.logs, event.payload],
            }))
            unsubRef.current?.()
            break
          case "failed":
            setState((prev) => ({
              ...prev,
              status: "failed",
              error: event.payload,
              logs: [...prev.logs, `ERROR: ${event.payload}`],
            }))
            unsubRef.current?.()
            break
        }
      })

      if (cancelledRef.current) {
        // reset()/unmount ran while subscribe() was resolving — don't leak.
        unsub()
        return
      }
      unsubRef.current = unsub
    },
    [eventName]
  )

  const reset = useCallback(() => {
    cancelledRef.current = true
    unsubRef.current?.()
    unsubRef.current = null
    setState({ status: "idle", logs: [], error: null })
  }, [])

  return { ...state, start, reset }
}
