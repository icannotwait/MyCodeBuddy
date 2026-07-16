"use client"

import { useSyncExternalStore } from "react"
import {
  clearHighlightCaches,
  getHighlightCacheStats,
} from "@/components/ai-elements/code-block"
import {
  clearCompletedStreamingPartitions,
  getCompletedStreamingPartitionsStats,
} from "@/lib/markdown/incremental-stream-blocks"
import type {
  DesktopDeliveryCapabilities,
  StreamingPerformanceFlags,
} from "@/lib/types"

export type { DesktopDeliveryCapabilities, StreamingPerformanceFlags }

/** Content-free combined cache stats for soak / memory assertions. */
export interface StreamingPerformanceCacheStats {
  markdownEntries: number
  markdownBytes: number
  highlightEntries: number
  highlightBytes: number
}

/**
 * Drop completed Markdown partitions and highlight caches only.
 * Active live-transcript / canonical state is left intact.
 */
export function resetStreamingPerformanceCaches(): void {
  clearCompletedStreamingPartitions()
  clearHighlightCaches()
}

/** Content-free snapshot of completed streaming caches. */
export function getStreamingPerformanceCacheStats(): StreamingPerformanceCacheStats {
  const markdown = getCompletedStreamingPartitionsStats()
  const highlight = getHighlightCacheStats()
  return {
    markdownEntries: markdown.size,
    markdownBytes: markdown.totalWeight,
    highlightEntries: highlight.entries,
    highlightBytes: highlight.bytes,
  }
}

let memoryPressureCleanup: (() => void) | null = null

/**
 * Register `onmemorypressure` when the capability exists (clears completed
 * caches only). No-op when unsupported. Idempotent; returns disposer.
 */
export function installStreamingPerformanceMemoryPressureHandler(): () => void {
  if (memoryPressureCleanup) return memoryPressureCleanup
  if (typeof window === "undefined" || !("onmemorypressure" in window)) {
    memoryPressureCleanup = () => {
      memoryPressureCleanup = null
    }
    return memoryPressureCleanup
  }
  const onPressure = () => {
    resetStreamingPerformanceCaches()
  }
  window.addEventListener("memorypressure", onPressure)
  memoryPressureCleanup = () => {
    window.removeEventListener("memorypressure", onPressure)
    memoryPressureCleanup = null
  }
  return memoryPressureCleanup
}

/**
 * Collapse invalid flag combinations so dependents never outrank prerequisites:
 * - batching requires mode === "batched"
 * - incremental transcript requires batching
 * - deferred rich content requires incremental transcript
 */
export function normalizeStreamingPerformanceCapabilities(
  value: DesktopDeliveryCapabilities
): DesktopDeliveryCapabilities {
  const batching =
    value.mode === "batched" && value.flags.desktop_acp_event_batching
  const incremental = batching && value.flags.incremental_live_transcript
  return {
    ...value,
    flags: {
      desktop_acp_event_batching: batching,
      incremental_live_transcript: incremental,
      deferred_streaming_rich_content:
        incremental && value.flags.deferred_streaming_rich_content,
    },
  }
}

let snapshot: DesktopDeliveryCapabilities | null = null
const listeners = new Set<() => void>()

function capabilitiesEqual(
  a: DesktopDeliveryCapabilities,
  b: DesktopDeliveryCapabilities
): boolean {
  return (
    a.mode === b.mode &&
    a.perf_replay_available === b.perf_replay_available &&
    a.failure_event === b.failure_event &&
    a.flags.desktop_acp_event_batching === b.flags.desktop_acp_event_batching &&
    a.flags.incremental_live_transcript ===
      b.flags.incremental_live_transcript &&
    a.flags.deferred_streaming_rich_content ===
      b.flags.deferred_streaming_rich_content
  )
}

/**
 * Install the process-local immutable capability snapshot.
 * Idempotent for the same normalized value; rejects a second different
 * startup snapshot (mode must never hot-switch).
 */
export function initializeStreamingPerformanceConfig(
  value: DesktopDeliveryCapabilities
): DesktopDeliveryCapabilities {
  const next = normalizeStreamingPerformanceCapabilities(value)
  if (snapshot !== null) {
    if (capabilitiesEqual(snapshot, next)) {
      return snapshot
    }
    throw new Error(
      "streaming performance config already initialized with a different snapshot"
    )
  }
  snapshot = next
  installStreamingPerformanceMemoryPressureHandler()
  for (const listener of listeners) listener()
  return snapshot
}

/** Current snapshot, or `null` before initialization. */
export function getStreamingPerformanceConfig(): DesktopDeliveryCapabilities | null {
  return snapshot
}

/** Subscribe to snapshot installation (React `useSyncExternalStore`). */
export function subscribeStreamingPerformanceConfig(
  onStoreChange: () => void
): () => void {
  listeners.add(onStoreChange)
  return () => {
    listeners.delete(onStoreChange)
  }
}

function getFlagSnapshot(flag: keyof StreamingPerformanceFlags): boolean {
  return snapshot?.flags[flag] ?? false
}

function getServerFlagSnapshot(): boolean {
  return false
}

/**
 * Narrow React hook for one streaming-performance flag.
 * Defaults to `false` before init / on the server.
 */
export function useStreamingPerformanceFlag(
  flag: keyof StreamingPerformanceFlags
): boolean {
  return useSyncExternalStore(
    subscribeStreamingPerformanceConfig,
    () => getFlagSnapshot(flag),
    getServerFlagSnapshot
  )
}

/**
 * Test-only: clear the process-local snapshot so subsequent inits can run.
 * @internal
 */
export function __resetStreamingPerformanceConfigForTests(): void {
  if (process.env.NODE_ENV !== "test") return
  snapshot = null
  memoryPressureCleanup?.()
  memoryPressureCleanup = null
}
