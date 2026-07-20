"use client"

import { useState, useEffect, useRef, useCallback } from "react"
import { listWorkspaceFiles } from "@/lib/api"
import type { FileTreeNode, WorkspaceFileEntry } from "@/lib/types"

export interface FlatFileEntry {
  name: string
  /** Relative path from folder root (same as WorkspaceFileEntry.path) */
  relativePath: string
  kind: "file" | "dir"
  /** Pre-computed lowercase relativePath for filtering */
  lowerPath: string
  /** Pre-computed lowercase name for filtering */
  lowerName: string
}

/**
 * Ignore files with gitignore-compatible syntax. The backend walk already
 * prunes entries matched by these files (same set ripgrep uses by default);
 * the frontend only uses the names to hide the ignore files themselves from
 * `@` mentions / command-palette file search.
 */
export const IGNORE_FILE_NAMES = new Set([".gitignore", ".ignore", ".rgignore"])

export function isIgnoreFileName(name: string): boolean {
  return IGNORE_FILE_NAMES.has(name)
}

export function flattenTree(nodes: FileTreeNode[]): FlatFileEntry[] {
  const entries: FlatFileEntry[] = []
  function walk(node: FileTreeNode) {
    entries.push({
      name: node.name,
      relativePath: node.path,
      kind: node.kind,
      lowerPath: node.path.toLowerCase(),
      lowerName: node.name.toLowerCase(),
    })
    if (node.kind === "dir" && node.children) {
      for (const child of node.children) {
        walk(child)
      }
    }
  }
  for (const node of nodes) {
    walk(node)
  }
  return entries
}

/** Check whether any ancestor directory of `path` is in `ignoredDirs`. */
export function hasIgnoredAncestor(
  path: string,
  ignoredDirs: Set<string>
): boolean {
  let idx = path.indexOf("/")
  while (idx !== -1) {
    if (ignoredDirs.has(path.slice(0, idx))) return true
    idx = path.indexOf("/", idx + 1)
  }
  return false
}

export function finishLazyLoad(
  inFlight: Map<string, number>,
  path: string,
  generation: number
): void {
  if (inFlight.get(path) === generation) {
    inFlight.delete(path)
  }
}

export function advanceLazyLoadGeneration(
  inFlight: Map<string, number>,
  currentGeneration: number
): number {
  inFlight.clear()
  return currentGeneration + 1
}
interface UseFileTreeOptions {
  folderPath: string | undefined
  enabled: boolean
}

interface UseFileTreeResult {
  allFiles: FlatFileEntry[]
  loading: boolean
  loaded: boolean
  /** Clear cached data so the next `enabled=true` triggers a fresh load. */
  reset: () => void
}

/**
 * Loads a flat, gitignore-aware listing of every file/dir under `folderPath`
 * (lazily, when `enabled`) for in-memory file search — shared by the search
 * dialog and the composer `@`-mention picker.
 *
 * Discovery and gitignore filtering run on the backend (`list_workspace_files`),
 * which prunes ignored directories *during* the walk and applies no depth cap,
 * so deeply nested files are reachable while `node_modules`/`target`/… are never
 * descended. The result is cached per folder path; a folder switch keeps showing
 * the previous list until the new one loads (`loaded` gates that transition).
 */
export function useFileTree({
  folderPath,
  enabled,
}: UseFileTreeOptions): UseFileTreeResult {
  const [allFiles, setAllFiles] = useState<FlatFileEntry[]>([])
  const [loading, setLoading] = useState(false)
  const loadedForPathRef = useRef<string | null>(null)

  useEffect(() => {
    if (!enabled || !folderPath) return
    if (loadedForPathRef.current === folderPath) return

    let canceled = false
    setLoading(true)

    async function load() {
      try {
        const files: WorkspaceFileEntry[] = await listWorkspaceFiles(
          folderPath!
        )
        const flat: FlatFileEntry[] = files
          .filter((f) => !isIgnoreFileName(f.name))
          .map((f) => ({
            name: f.name,
            relativePath: f.path,
            kind: f.kind,
            lowerPath: f.path.toLowerCase(),
            lowerName: f.name.toLowerCase(),
          }))

        if (!canceled) {
          setAllFiles(flat)
          loadedForPathRef.current = folderPath!
        }
      } catch {
        if (!canceled) setAllFiles([])
      } finally {
        if (!canceled) setLoading(false)
      }
    }

    void load()
    return () => {
      canceled = true
    }
  }, [enabled, folderPath])

  const reset = useCallback(() => {
    loadedForPathRef.current = null
    setAllFiles([])
  }, [])

  return {
    allFiles,
    loading,
    loaded: loadedForPathRef.current === folderPath,
    reset,
  }
}
