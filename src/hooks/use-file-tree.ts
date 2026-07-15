"use client"

import { useState, useEffect, useRef, useCallback } from "react"
import { getFileTree } from "@/lib/api"
import type { FileTreeNode } from "@/lib/types"

export interface FlatFileEntry {
  name: string
  /** Relative path from folder root (same as FileTreeNode.path) */
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
export const IGNORE_FILE_NAMES = new Set([
  ".gitignore",
  ".ignore",
  ".rgignore",
])

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
        // Backend `get_file_tree` prunes via .gitignore / .ignore / .rgignore
        // during the walk (ignore crate, same as ripgrep). No second-pass
        // read of ignore files here — that used to dominate large-project cost.
        const tree = await getFileTree(folderPath!, 10)
        const flat = flattenTree(tree).filter((f) => !isIgnoreFileName(f.name))

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
