"use client"

import { useCallback, useEffect, useRef, useState } from "react"

import { useLoopRealtime } from "@/components/loops/loop-realtime-context"
import type { LoopChanged } from "@/lib/types"

/**
 * Fetch a loop resource and keep it live: refetch whenever a relevant
 * `loop://changed` event arrives (coalesced by the provider) or the transport
 * reconnects. Robust by construction:
 *
 * - a sequence guard drops stale responses, so a slow earlier fetch can never
 *   overwrite a newer one;
 * - a failed fetch keeps the previous data and just clears the first-load
 *   spinner — the UI never blanks on a transient error;
 * - `fetcher` and `match` are read through refs, so the single subscription set
 *   up on mount always invokes the latest closures without re-subscribing.
 *
 * Scope must come from `deps` (stable inputs like a space or issue id), NEVER
 * from the async `data`: deriving the match scope from fetched data reintroduces
 * the "filter by a value that changes under you" bug class this layer exists to
 * eliminate.
 */
export function useLoopResource<T>(
  fetcher: () => Promise<T>,
  opts: {
    match: (e: LoopChanged) => boolean
    initial: T
    deps?: ReadonlyArray<unknown>
  }
): { data: T; loading: boolean; refetch: () => void } {
  const { register } = useLoopRealtime()
  const [data, setData] = useState<T>(opts.initial)
  const [loading, setLoading] = useState(true)
  const seq = useRef(0)
  const fetcherRef = useRef(fetcher)
  const matchRef = useRef(opts.match)
  useEffect(() => {
    fetcherRef.current = fetcher
  })
  useEffect(() => {
    matchRef.current = opts.match
  })

  const refetch = useCallback(() => {
    const my = ++seq.current
    fetcherRef
      .current()
      .then((d) => {
        if (my === seq.current) {
          setData(d)
          setLoading(false)
        }
      })
      .catch(() => {
        // Keep the last good data; only clear the first-load spinner.
        if (my === seq.current) setLoading(false)
      })
  }, [])

  const deps = opts.deps ?? []
  // Initial fetch + refetch when the stable scope changes. `refetch` is stable;
  // deps are spread by VALUE so the effect re-runs only when a scope value
  // changes — not on every render (a caller's `[issueId]` literal is a fresh
  // array each render). The spread is what exhaustive-deps flags here.
  useEffect(() => {
    refetch()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refetch, ...deps])

  // One registration for this resource's lifetime; the invalidator reads the
  // latest `match` via ref. A `null` batch (reconnect) always refetches.
  useEffect(
    () =>
      register((events) => {
        if (events === null || events.some((e) => matchRef.current(e))) {
          refetch()
        }
      }),
    [register, refetch]
  )

  return { data, loading, refetch }
}
