"use client"

/**
 * One subscription to the coarse `loop://changed` event for the entire loop UI.
 *
 * Every loop surface used to own its own `subscribe()` (via `useLoopChanged`) and
 * its own fetch-on-event plumbing; opening a space mounted a dozen independent
 * subscriptions, each firing a full refetch on every event. This provider
 * subscribes once, coalesces a burst of events into a single animation frame,
 * and fans the batch out to every registered consumer. A consumer (see
 * `useLoopResource`) refetches only if a batched event is relevant to it.
 *
 * Reconnect resync: the WebSocket broadcaster drops events while no client is
 * listening, so anything fired during a disconnect window is lost. On transport
 * reconnect we invalidate ALL consumers (the `null` batch) so the UI re-syncs to
 * authoritative state — without this, a network blip silently desynchronizes
 * every open loop view until the next manual navigation.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  type ReactNode,
} from "react"

import { onTransportReconnect, subscribe } from "@/lib/platform"
import { LOOP_CHANGED_EVENT, type LoopChanged } from "@/lib/types"

/**
 * Called once per animation frame with the batch of loop events that arrived in
 * that frame, or `null` after a transport reconnect ("invalidate everything").
 * Passing the whole batch (not one call per event) is what collapses a burst
 * into a single refetch: a consumer checks whether ANY batched event matches.
 */
type Invalidator = (events: LoopChanged[] | null) => void

interface LoopRealtimeValue {
  register: (inv: Invalidator) => () => void
}

const Ctx = createContext<LoopRealtimeValue | null>(null)

// requestAnimationFrame is read at call time (not captured at module load) so
// tests can stub it, and so we degrade to a macrotask where rAF is absent.
function scheduleFrame(cb: () => void): number {
  if (typeof requestAnimationFrame !== "undefined") {
    return requestAnimationFrame(() => cb())
  }
  return setTimeout(cb, 0) as unknown as number
}
function cancelFrame(id: number): void {
  if (typeof cancelAnimationFrame !== "undefined") cancelAnimationFrame(id)
  else clearTimeout(id)
}

export function LoopRealtimeProvider({ children }: { children: ReactNode }) {
  const invalidators = useRef(new Set<Invalidator>())
  const pending = useRef<LoopChanged[] | "all" | null>(null)
  const frame = useRef<number | null>(null)

  const flush = useCallback(() => {
    frame.current = null
    const batch = pending.current
    pending.current = null
    if (batch === "all") {
      invalidators.current.forEach((inv) => inv(null))
      return
    }
    if (!batch) return
    invalidators.current.forEach((inv) => inv(batch))
  }, [])

  const schedule = useCallback(
    (event: LoopChanged | "all") => {
      // A reconnect ("all") subsumes any individual events queued this frame.
      if (event === "all") {
        pending.current = "all"
      } else if (pending.current !== "all") {
        const arr = pending.current ?? []
        arr.push(event)
        pending.current = arr
      }
      if (frame.current == null) frame.current = scheduleFrame(flush)
    },
    [flush]
  )

  useEffect(() => {
    let disposed = false
    let unsub: (() => void) | undefined
    void subscribe<LoopChanged>(LOOP_CHANGED_EVENT, (event) => {
      if (!disposed) schedule(event)
    }).then((fn) => {
      if (disposed) fn()
      else unsub = fn
    })
    // Desktop IPC has no disconnect window, so this is null there (no-op).
    const offReconnect = onTransportReconnect(() => schedule("all"))
    return () => {
      disposed = true
      unsub?.()
      offReconnect?.()
      if (frame.current != null) {
        cancelFrame(frame.current)
        frame.current = null
      }
    }
  }, [schedule])

  const register = useCallback((inv: Invalidator) => {
    invalidators.current.add(inv)
    return () => {
      invalidators.current.delete(inv)
    }
  }, [])

  const value = useMemo<LoopRealtimeValue>(() => ({ register }), [register])

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useLoopRealtime(): LoopRealtimeValue {
  const ctx = useContext(Ctx)
  if (!ctx) {
    throw new Error("useLoopRealtime must be used within LoopRealtimeProvider")
  }
  return ctx
}
