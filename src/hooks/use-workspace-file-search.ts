"use client"

import { useEffect, useMemo, useRef, useState } from "react"

import type { FlatFileEntry } from "@/hooks/use-file-tree"
import { searchWorkspaceFiles } from "@/lib/api"
import type { WorkspaceFileEntry } from "@/lib/types"
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

function toFlatFileEntry(hit: WorkspaceFileEntry): FlatFileEntry {
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

  useEffect(() => {
    const generation = ++generationRef.current
    if (!enabled || !folderPath) {
      return
    }

    const abort = new AbortController()
    const timer = window.setTimeout(() => {
      const consumerId =
        sessionIdRef.current ?? (sessionIdRef.current = randomUUID())
      void searchWorkspaceFiles(folderPath, query, {
        limit,
        request: { consumerId, generation },
        signal: abort.signal,
      })
        .then((files) => {
          if (generationRef.current !== generation) return
          setSettled({
            tag: requestTag,
            files: files.map(toFlatFileEntry),
          })
        })
        .catch(() => {
          if (generationRef.current !== generation || abort.signal.aborted) {
            return
          }
          setSettled({ tag: requestTag, files: [] })
        })
    }, debounceMs)

    return () => {
      window.clearTimeout(timer)
      if (generationRef.current === generation) {
        generationRef.current += 1
      }
      abort.abort()
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
