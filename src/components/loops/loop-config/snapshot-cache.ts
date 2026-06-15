"use client"

import { useCallback, useEffect, useRef, useState } from "react"

import { describeAgentOptions } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentOptionsSnapshot, AgentType } from "@/lib/types"

interface CachedSnapshot {
  snapshot: AgentOptionsSnapshot
  ts: number
}
const SNAPSHOT_TTL_MS = 30_000
const snapshotCache = new Map<AgentType, CachedSnapshot>()

function readCache(agent: AgentType): AgentOptionsSnapshot | null {
  const entry = snapshotCache.get(agent)
  if (!entry) return null
  if (Date.now() - entry.ts > SNAPSHOT_TTL_MS) {
    snapshotCache.delete(agent)
    return null
  }
  return entry.snapshot
}

/** TEST ONLY: clear the module-scope probe cache between tests so a probed
 *  agent in one test can't suppress the probe assertion in the next. */
export function __resetSnapshotCacheForTests(): void {
  snapshotCache.clear()
}

/** Live probe of an agent's modes/config options (30s module-scope cache),
 *  mirroring the delegation-settings panel so the config the user picks matches
 *  what the engine will pass when it spawns the agent. */
export function useAgentOptions(agent: AgentType): {
  snapshot: AgentOptionsSnapshot | null
  loading: boolean
  error: string | null
} {
  const [snapshot, setSnapshot] = useState<AgentOptionsSnapshot | null>(() =>
    readCache(agent)
  )
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const reqIdRef = useRef(0)

  const load = useCallback(async (a: AgentType) => {
    const cached = readCache(a)
    if (cached) {
      setSnapshot(cached)
      setError(null)
      setLoading(false)
      return
    }
    const reqId = ++reqIdRef.current
    setLoading(true)
    setError(null)
    setSnapshot(null)
    try {
      const fresh = await describeAgentOptions(a)
      if (reqIdRef.current !== reqId) return
      snapshotCache.set(a, { snapshot: fresh, ts: Date.now() })
      setSnapshot(fresh)
    } catch (err) {
      if (reqIdRef.current !== reqId) return
      setError(toErrorMessage(err))
    } finally {
      if (reqIdRef.current === reqId) setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load(agent)
  }, [agent, load])

  return { snapshot, loading, error }
}
