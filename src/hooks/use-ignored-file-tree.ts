"use client"

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react"

import { getFileTree } from "@/lib/api"
import { useShowIgnoredFiles } from "@/lib/file-tree-display-prefs"
import type { FileTreeNode } from "@/lib/types"
import { isIgnoreFileName } from "./use-file-tree"
import type { WorkspaceEnvelopeListener } from "./use-workspace-state-store"

const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect

type RefreshReason = "enable" | "background" | "manual"
export type IgnoredTreeErrorReason = "enable" | "manual"

export interface UseIgnoredFileTreeOptions {
  folderPath: string | null
  fallbackTree: FileTreeNode[]
  workspaceSeq: number
  subscribeEnvelopes: (listener: WorkspaceEnvelopeListener) => () => void
  onError?: (reason: IgnoredTreeErrorReason) => void
}

export interface IgnoredFileTreeState {
  tree: FileTreeNode[]
  showIgnored: boolean
  setShowIgnored: (value: boolean) => void
  restored: boolean
  loading: boolean
  refresh: () => Promise<void>
  treeGeneration: number
}

interface ModeState {
  key: string
  generation: number
}

interface CurrentConfig {
  folderPath: string | null
  showIgnored: boolean
  restored: boolean
  generation: number
}

interface OverlayTree {
  folderPath: string
  generation: number
  tree: FileTreeNode[]
}

interface ActiveRefresh {
  folderPath: string
  generation: number
  reason: RefreshReason
  promise: Promise<void>
}

function useModeGeneration(key: string): number {
  const [state, setState] = useState<ModeState>(() => ({
    key,
    generation: 0,
  }))
  if (state.key === key) return state.generation

  const next = { key, generation: state.generation + 1 }
  setState(next)
  return next.generation
}

function queuedReason(
  current: RefreshReason | null,
  next: RefreshReason
): RefreshReason {
  if (current === "manual" || next === "manual") return "manual"
  if (current === "enable" || next === "enable") return "enable"
  return "background"
}

export function shouldRefreshIgnoredTree(
  fsEventKind: string | undefined,
  changedPaths: string[]
): boolean {
  if (changedPaths.length === 0) return true
  if (
    changedPaths.some((path) => {
      const normalized = path.replace(/\\/g, "/")
      const name = normalized.slice(normalized.lastIndexOf("/") + 1)
      return isIgnoreFileName(name)
    })
  ) {
    return true
  }
  if (fsEventKind === "create" || fsEventKind === "remove") return true
  if (fsEventKind === "modify") return false
  return true
}

export function useIgnoredFileTree({
  folderPath,
  fallbackTree,
  workspaceSeq,
  subscribeEnvelopes,
  onError,
}: UseIgnoredFileTreeOptions): IgnoredFileTreeState {
  const [showIgnored, setShowIgnored, restored] = useShowIgnoredFiles()
  const modeKey = JSON.stringify([folderPath, showIgnored])
  const treeGeneration = useModeGeneration(modeKey)
  const [overlay, setOverlay] = useState<OverlayTree | null>(null)

  const mountedRef = useRef(true)
  const configRef = useRef<CurrentConfig | null>(null)
  const overlayRef = useRef<OverlayTree | null>(null)
  const activeRef = useRef<ActiveRefresh | null>(null)
  const queuedReasonRef = useRef<RefreshReason | null>(null)
  const lastEnvelopeSeqRef = useRef(workspaceSeq)
  const seqBaselineRef = useRef(workspaceSeq)
  const workspaceSeqRef = useRef(workspaceSeq)
  const callbacksRef = useRef({ setShowIgnored, onError })

  useIsomorphicLayoutEffect(() => {
    workspaceSeqRef.current = workspaceSeq
  }, [workspaceSeq])

  useIsomorphicLayoutEffect(() => {
    callbacksRef.current = { setShowIgnored, onError }
  }, [onError, setShowIgnored])

  useIsomorphicLayoutEffect(() => {
    configRef.current = {
      folderPath,
      showIgnored,
      restored,
      generation: treeGeneration,
    }
    queuedReasonRef.current = null
    seqBaselineRef.current = workspaceSeqRef.current
    lastEnvelopeSeqRef.current = workspaceSeqRef.current
  }, [folderPath, restored, showIgnored, treeGeneration])

  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
      configRef.current = null
      queuedReasonRef.current = null
    }
  }, [])

  const runRefresh = useCallback(function refresh(
    reason: RefreshReason
  ): Promise<void> {
    const config = configRef.current
    if (
      !mountedRef.current ||
      !config?.restored ||
      !config.showIgnored ||
      !config.folderPath
    ) {
      return Promise.resolve()
    }

    const active = activeRef.current
    if (
      active?.generation === config.generation &&
      active.folderPath === config.folderPath
    ) {
      if (!(active.reason === "enable" && reason === "enable")) {
        queuedReasonRef.current = queuedReason(
          queuedReasonRef.current,
          reason
        )
      }
      return active.promise
    }

    const requestFolder = config.folderPath
    const requestGeneration = config.generation
    const hadSuccessfulTree =
      overlayRef.current?.folderPath === requestFolder &&
      overlayRef.current.generation === requestGeneration

    const promise = getFileTree(requestFolder, 2, true)
      .then((tree) => {
        const current = configRef.current
        if (
          !mountedRef.current ||
          !current?.showIgnored ||
          current.folderPath !== requestFolder ||
          current.generation !== requestGeneration
        ) {
          return
        }
        const next = {
          folderPath: requestFolder,
          generation: requestGeneration,
          tree,
        }
        overlayRef.current = next
        setOverlay(next)
      })
      .catch(() => {
        const current = configRef.current
        if (
          !mountedRef.current ||
          !current?.showIgnored ||
          current.folderPath !== requestFolder ||
          current.generation !== requestGeneration
        ) {
          return
        }
        if (reason === "enable" && !hadSuccessfulTree) {
          callbacksRef.current.setShowIgnored(false)
          callbacksRef.current.onError?.("enable")
        } else if (reason === "manual") {
          callbacksRef.current.onError?.("manual")
        }
      })
      .finally(() => {
        if (activeRef.current?.promise !== promise) return
        activeRef.current = null
        const nextReason = queuedReasonRef.current
        queuedReasonRef.current = null
        const current = configRef.current
        if (
          nextReason &&
          mountedRef.current &&
          current?.showIgnored &&
          current.folderPath === requestFolder &&
          current.generation === requestGeneration
        ) {
          void refresh(nextReason)
        }
      })

    activeRef.current = {
      folderPath: requestFolder,
      generation: requestGeneration,
      reason,
      promise,
    }
    return promise
  }, [])

  useEffect(() => {
    if (!restored || !showIgnored || !folderPath) return
    void runRefresh("enable")
  }, [folderPath, restored, runRefresh, showIgnored, treeGeneration])

  useEffect(() => {
    if (!restored || !showIgnored || !folderPath) return
    return subscribeEnvelopes(
      ({ seq, fs_event_kind: fsEventKind, changed_paths: changedPaths }) => {
        lastEnvelopeSeqRef.current = Math.max(lastEnvelopeSeqRef.current, seq)
        if (shouldRefreshIgnoredTree(fsEventKind, changedPaths)) {
          void runRefresh("background")
        }
      }
    )
  }, [
    folderPath,
    restored,
    runRefresh,
    showIgnored,
    subscribeEnvelopes,
    treeGeneration,
  ])

  useEffect(() => {
    const config = configRef.current
    if (
      !config?.restored ||
      !config.showIgnored ||
      !config.folderPath ||
      config.generation !== treeGeneration ||
      workspaceSeq <= seqBaselineRef.current
    ) {
      return
    }

    const hasMatchingEnvelope = workspaceSeq <= lastEnvelopeSeqRef.current
    seqBaselineRef.current = workspaceSeq
    if (!hasMatchingEnvelope) {
      lastEnvelopeSeqRef.current = workspaceSeq
      void runRefresh("background")
    }
  }, [folderPath, restored, runRefresh, showIgnored, treeGeneration, workspaceSeq])

  const refresh = useCallback(() => runRefresh("manual"), [runRefresh])
  const hasCurrentOverlay =
    overlay?.folderPath === folderPath &&
    overlay.generation === treeGeneration

  return {
    tree: showIgnored && hasCurrentOverlay ? overlay.tree : fallbackTree,
    showIgnored,
    setShowIgnored,
    restored,
    loading: Boolean(restored && showIgnored && folderPath && !hasCurrentOverlay),
    refresh,
    treeGeneration,
  }
}
