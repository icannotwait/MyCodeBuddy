"use client"

import type { ReactNode } from "react"
import { useCallback, useEffect, useMemo, useRef } from "react"
import { useTranslations } from "next-intl"
import { openUrl } from "@/lib/platform"
import { getActiveRemoteConnectionId, isDesktop } from "@/lib/transport"
import { toErrorMessage } from "@/lib/app-error"
import type { LinkSafetyConfig, LinkSafetyModalProps } from "streamdown"
import { toast } from "sonner"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { useWorkspaceContext } from "@/contexts/workspace-context"
import { cn } from "@/lib/utils"

interface LocalFileTarget {
  path: string
  line: number | null
}

const WINDOWS_ABSOLUTE_PATH = /^[a-zA-Z]:[\\/]/
const URL_SCHEME = /^[a-zA-Z][a-zA-Z\d+\-.]*:/
const ALLOWED_EXTERNAL_PROTOCOLS = new Set([
  "http:",
  "https:",
  "mailto:",
  "tel:",
])

function normalizeSlashPath(path: string): string {
  return path.replace(/\\/g, "/")
}

/** Strip leading slash before Windows drive letter: /C:/foo → C:/foo */
function stripLeadingSlashOnWindows(p: string): string {
  if (p.startsWith("/") && WINDOWS_ABSOLUTE_PATH.test(p.slice(1))) {
    return p.slice(1)
  }
  return p
}

function decodeUriSafely(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

function parseLineValue(raw: string | undefined): number | null {
  if (!raw) return null
  const line = Number.parseInt(raw, 10)
  if (!Number.isFinite(line) || line <= 0) return null
  return line
}

function parseHashLine(hash: string): number | null {
  const normalized = hash.startsWith("#") ? hash.slice(1) : hash
  if (!normalized) return null
  return (
    parseLineValue(normalized.match(/^L(\d+)$/i)?.[1]) ??
    parseLineValue(normalized.match(/^line=(\d+)$/i)?.[1]) ??
    parseLineValue(normalized.match(/^(\d+)$/)?.[1])
  )
}

function splitPathAndLine(rawPath: string): LocalFileTarget {
  const trimmed = rawPath.trim()
  const match = trimmed.match(/^(.*):(\d+)(?::\d+)?$/)
  if (!match) {
    return { path: trimmed, line: null }
  }

  const maybePath = match[1]
  if (!maybePath || maybePath.endsWith("://")) {
    return { path: trimmed, line: null }
  }

  const line = parseLineValue(match[2])
  if (!line) {
    return { path: trimmed, line: null }
  }

  return { path: maybePath, line }
}

function isLocalPathLike(path: string): boolean {
  return (
    path.startsWith("/") ||
    path.startsWith("./") ||
    path.startsWith("../") ||
    path.startsWith("~/") ||
    WINDOWS_ABSOLUTE_PATH.test(path)
  )
}

function parseLocalFileTarget(rawUrl: string): LocalFileTarget | null {
  const trimmed = rawUrl.trim()
  if (!trimmed) return null

  if (trimmed.toLowerCase().startsWith("file://")) {
    try {
      const parsed = new URL(trimmed)
      const rawPathname = decodeUriSafely(parsed.pathname)
      const normalizedPathname = stripLeadingSlashOnWindows(rawPathname)
      const pathAndLine = splitPathAndLine(normalizedPathname)
      if (!pathAndLine.path) return null
      return {
        path: normalizeSlashPath(pathAndLine.path),
        line: parseHashLine(parsed.hash) ?? pathAndLine.line,
      }
    } catch {
      return null
    }
  }

  if (URL_SCHEME.test(trimmed) && !WINDOWS_ABSOLUTE_PATH.test(trimmed)) {
    return null
  }

  // Split on raw # / ? before decoding so encoded `%23` / `%3F` inside the
  // path don't get promoted to fragment/query separators (which would point
  // the file opener at the wrong file).
  const hashIndex = trimmed.indexOf("#")
  const rawHash = hashIndex >= 0 ? trimmed.slice(hashIndex) : ""
  const beforeHash = hashIndex >= 0 ? trimmed.slice(0, hashIndex) : trimmed
  const queryIndex = beforeHash.indexOf("?")
  const rawPathPart =
    queryIndex >= 0 ? beforeHash.slice(0, queryIndex) : beforeHash
  const decodedPath = decodeUriSafely(rawPathPart)
  const pathAndLine = splitPathAndLine(decodedPath)
  const normalizedPath = stripLeadingSlashOnWindows(pathAndLine.path)
  if (!isLocalPathLike(normalizedPath)) return null

  return {
    path: normalizeSlashPath(normalizedPath),
    line: parseHashLine(rawHash) ?? pathAndLine.line,
  }
}

function parseExternalUrl(rawUrl: string): URL | null {
  const trimmed = rawUrl.trim()
  if (!trimmed) return null

  if (trimmed.startsWith("//")) {
    try {
      return new URL(trimmed, window.location.href)
    } catch {
      return null
    }
  }

  if (!URL_SCHEME.test(trimmed) || WINDOWS_ABSOLUTE_PATH.test(trimmed)) {
    return null
  }

  try {
    return new URL(trimmed)
  } catch {
    return null
  }
}

function isAllowedExternalUrl(rawUrl: string): boolean {
  const parsed = parseExternalUrl(rawUrl)
  return parsed
    ? ALLOWED_EXTERNAL_PROTOCOLS.has(parsed.protocol.toLowerCase())
    : false
}

function shouldLetStreamdownOpenExternalUrl(rawUrl: string): boolean {
  if (parseLocalFileTarget(rawUrl)) return false
  if (!isAllowedExternalUrl(rawUrl)) return false
  return !isDesktop() || getActiveRemoteConnectionId() !== null
}

function toWorkspaceRelativePath(
  path: string,
  workspacePath: string
): string | null {
  const normalizedPath = normalizeSlashPath(path)
  const normalizedWorkspace = normalizeSlashPath(workspacePath).replace(
    /\/+$/,
    ""
  )
  if (!normalizedPath || !normalizedWorkspace) return null

  if (!normalizedPath.startsWith("/") && !WINDOWS_ABSOLUTE_PATH.test(path)) {
    return normalizedPath.replace(/^\.\/+/, "")
  }

  const isWindows = WINDOWS_ABSOLUTE_PATH.test(normalizedWorkspace)
  const pathForCompare = isWindows
    ? normalizedPath.toLowerCase()
    : normalizedPath
  const workspaceForCompare = isWindows
    ? normalizedWorkspace.toLowerCase()
    : normalizedWorkspace

  if (pathForCompare === workspaceForCompare) return null
  if (!pathForCompare.startsWith(`${workspaceForCompare}/`)) return null

  return normalizedPath.slice(normalizedWorkspace.length + 1)
}

function DirectLinkOpen({
  url,
  isOpen,
  onClose,
  onAction,
}: LinkSafetyModalProps & {
  onAction: (url: string) => Promise<void>
}) {
  const openingUrlRef = useRef<string | null>(null)

  useEffect(() => {
    if (!isOpen) {
      openingUrlRef.current = null
      return
    }
    if (openingUrlRef.current === url) return

    let cancelled = false
    openingUrlRef.current = url
    void onAction(url).finally(() => {
      if (!cancelled) onClose()
    })

    return () => {
      cancelled = true
    }
  }, [isOpen, onAction, onClose, url])

  return null
}

function useOpenLinkOrFile() {
  const t = useTranslations("Folder.chat.linkSafety")
  const { activeFolder: folder } = useActiveFolder()
  const folderPath = folder?.path
  const { openFilePreview } = useWorkspaceContext()

  return useCallback(
    async (url: string) => {
      const localTarget = parseLocalFileTarget(url)
      if (localTarget) {
        if (!folderPath) {
          toast.error(t("errorCannotOpen"), {
            description: t("errorNoWorkspace"),
          })
          return
        }

        const relativePath = toWorkspaceRelativePath(
          localTarget.path,
          folderPath
        )
        if (!relativePath) {
          toast.error(t("errorCannotOpen"), {
            description: t("errorOutsideWorkspace"),
          })
          return
        }

        try {
          await openFilePreview(relativePath, {
            line: localTarget.line ?? undefined,
          })
        } catch (error) {
          toast.error(t("errorFailedOpen"), {
            description: toErrorMessage(error),
          })
        }
        return
      }

      if (!isAllowedExternalUrl(url)) {
        toast.error(t("errorFailedLink"), {
          description: t("errorUnsupportedLinkProtocol"),
        })
        return
      }

      try {
        await openUrl(url)
      } catch (error) {
        toast.error(t("errorFailedLink"), {
          description: toErrorMessage(error),
        })
      }
    },
    [folderPath, openFilePreview, t]
  )
}

export function useStreamdownLinkSafety(): LinkSafetyConfig {
  const handleOpenTarget = useOpenLinkOrFile()

  const handleLinkCheck = useCallback(
    (url: string) => shouldLetStreamdownOpenExternalUrl(url),
    []
  )

  const renderModal = useCallback(
    (props: LinkSafetyModalProps) => (
      <DirectLinkOpen {...props} onAction={handleOpenTarget} />
    ),
    [handleOpenTarget]
  )

  return useMemo(
    () => ({
      enabled: true,
      onLinkCheck: handleLinkCheck,
      renderModal,
    }),
    [handleLinkCheck, renderModal]
  )
}

/**
 * Resolve a tool-call file path (which may be absolute, workspace-relative, or
 * a bare relative path) into something `openFilePreview` can consume. Falls
 * back to the raw input when no other heuristic matches so the opener can
 * still surface a useful error toast.
 */
function resolveToolFilePath(
  rawPath: string,
  workspacePath: string | null
): string | null {
  const normalized = normalizeSlashPath(rawPath.trim())
  if (!normalized) return null

  const isAbsolute =
    normalized.startsWith("/") || WINDOWS_ABSOLUTE_PATH.test(normalized)
  if (isAbsolute) {
    if (!workspacePath) return null
    return toWorkspaceRelativePath(normalized, workspacePath)
  }

  return normalized.replace(/^\.\/+/, "")
}

/**
 * Clickable file-path label that routes the file into the workspace file panel.
 */
export function FilePathLink({
  filePath,
  line,
  className,
  title,
  children,
}: {
  filePath: string
  line?: number | null
  className?: string
  title?: string
  children: ReactNode
}) {
  const t = useTranslations("Folder.chat.linkSafety")
  const { activeFolder: folder } = useActiveFolder()
  const folderPath = folder?.path ?? null
  const { openFilePreview } = useWorkspaceContext()
  const openingRef = useRef(false)

  const handleOpen = useCallback(() => {
    if (openingRef.current) return
    if (!folderPath) {
      toast.error(t("errorCannotOpen"), {
        description: t("errorNoWorkspace"),
      })
      return
    }
    const relativePath = resolveToolFilePath(filePath, folderPath)
    if (!relativePath) {
      toast.error(t("errorCannotOpen"), {
        description: t("errorOutsideWorkspace"),
      })
      return
    }

    openingRef.current = true
    void openFilePreview(relativePath, {
      line: line ?? undefined,
    })
      .catch((error) => {
        toast.error(t("errorFailedOpen"), {
          description: toErrorMessage(error),
        })
      })
      .finally(() => {
        openingRef.current = false
      })
  }, [filePath, folderPath, line, openFilePreview, t])

  return (
    <span className={cn("block min-w-0", className)}>
      <button
        type="button"
        title={title ?? filePath}
        className="max-w-full cursor-pointer truncate text-left align-bottom hover:underline focus-visible:underline focus-visible:outline-none"
        onClick={(e) => {
          e.stopPropagation()
          handleOpen()
        }}
      >
        {children}
      </button>
    </span>
  )
}
