"use client"

import { useEffect, useMemo, useRef, useState } from "react"

import type { FlatFileEntry } from "@/hooks/use-file-tree"
import {
  cancelWorkspaceFileSearch,
  searchWorkspaceFiles,
  type WorkspaceFileSearchIdentity,
} from "@/lib/api"
import { randomUUID } from "@/lib/utils"

export interface UseWorkspaceFileSearchOptions {
  folderPath: string
  query: string
  enabled: boolean
  limit?: number
  debounceMs?: number
}

export interface WorkspaceFileSearchState {
  files: FlatFileEntry[]
  loading: boolean
}

interface SettledResult {
  tag: object
  files: FlatFileEntry[]
}

interface ActiveRequest {
  generation: number
  identity: WorkspaceFileSearchIdentity
}

function cancelSilently(identity: WorkspaceFileSearchIdentity): void {
  void cancelWorkspaceFileSearch(identity).catch(() => undefined)
}

function toFlatFileEntry(hit: {
  name: string
  path: string
  kind: string
}): FlatFileEntry {
  const kind: "file" | "dir" = hit.kind === "dir" ? "dir" : "file"
  return {
    name: hit.name,
    relativePath: hit.path,
    kind,
    lowerPath: hit.path.toLowerCase(),
    lowerName: hit.name.toLowerCase(),
  }
}

export function useWorkspaceFileSearch({
  folderPath,
  query,
  enabled,
  limit = 100,
  debounceMs = 200,
}: UseWorkspaceFileSearchOptions): WorkspaceFileSearchState {
  const requestTag = useMemo(
    () => ({ folderPath, query, limit, enabled }),
    [enabled, folderPath, limit, query]
  )
  const [settled, setSettled] = useState<SettledResult | null>(null)
  const sessionIdRef = useRef<string | null>(null)
  const generationRef = useRef(0)
  const activeRequestRef = useRef<ActiveRequest | null>(null)

  useEffect(() => {
    const generation = ++generationRef.current
    if (!enabled || !folderPath) {
      return
    }

    const timer = window.setTimeout(() => {
      const searchSessionId =
        sessionIdRef.current ?? (sessionIdRef.current = randomUUID())
      const identity: WorkspaceFileSearchIdentity = {
        searchSessionId,
        requestId: randomUUID(),
      }
      activeRequestRef.current = { generation, identity }

      void searchWorkspaceFiles(folderPath, query, limit, identity)
        .then((searchResult) => {
          if (generationRef.current !== generation) return
          setSettled({
            tag: requestTag,
            files: searchResult.files.map(toFlatFileEntry),
          })
        })
        .catch(() => {
          if (generationRef.current !== generation) return
          setSettled({ tag: requestTag, files: [] })
        })
        .finally(() => {
          if (activeRequestRef.current?.generation === generation) {
            activeRequestRef.current = null
          }
        })
    }, debounceMs)

    return () => {
      window.clearTimeout(timer)
      if (generationRef.current === generation) {
        generationRef.current += 1
      }
      const active = activeRequestRef.current
      if (active?.generation === generation) {
        activeRequestRef.current = null
        cancelSilently(active.identity)
      }
    }
  }, [debounceMs, enabled, folderPath, limit, query, requestTag])

  if (!enabled || !folderPath || settled?.tag !== requestTag) {
    return {
      files: [],
      loading: Boolean(enabled && folderPath),
    }
  }

  return { files: settled.files, loading: false }
}
