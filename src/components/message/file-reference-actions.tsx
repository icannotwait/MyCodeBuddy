"use client"

import type { ReactNode } from "react"
import { useEffect, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"

import { useGrokSessionImageScope } from "@/components/ai-elements/grok-session-image-context"
import { parseLocalFileTarget } from "@/components/ai-elements/link-safety"
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { resolveGrokSessionImage } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { expandHomePath, isHomeRelativePath } from "@/lib/file-open-target"
import {
  isAbsoluteFilePath,
  toAbsoluteFilePath,
  toFolderRelativePath,
} from "@/lib/file-path-display"
import {
  GROK_SESSION_IMAGE_MIME_BY_EXTENSION,
  parseGrokSessionImageRef,
} from "@/lib/markdown/grok-session-image"
import { isLocalDesktop, revealItemInDir } from "@/lib/platform"
import { copyTextFromMenu } from "@/lib/utils"

/** The two path forms offered by the badge's action menu. */
export interface FileReferencePaths {
  /**
   * Absolute filesystem path (slash-normalized), or a `~/`-rooted path — the
   * home directory only resolves through the backend, so the tilde form is
   * kept here and expanded at reveal time.
   */
  absolute: string
  /** Path relative to the active folder; null when the file lives outside it. */
  relative: string | null
}

type GrokMenuGateToken = Readonly<{ identity: string }>

type GrokMenuResolution =
  | { token: GrokMenuGateToken; status: "loading"; paths: null }
  | { token: GrokMenuGateToken | null; status: "failed"; paths: null }
  | {
      token: GrokMenuGateToken
      status: "ready"
      paths: FileReferencePaths
    }

/**
 * Resolve a rendered file reference — a `file://` uri (user-message badge) or a
 * local path (assistant markdown link, already rewritten by
 * remark-file-uri-links) — into its absolute and folder-relative forms.
 *
 * Returns null when the target isn't a local file at all (a web link, an
 * embedded `codeg://` attachment badge) or when a relative target can't be made
 * absolute for lack of an active folder — in those cases the badge shows no
 * action button, since none of the three actions could do anything.
 *
 * Parsing goes through link-safety's {@link parseLocalFileTarget} so the menu
 * resolves the same path a click on the badge would open (line suffixes like
 * `#L10-25` / `:12` are dropped — a path is copied, not a location).
 */
export function resolveFileReferenceTarget(
  rawTarget: string,
  folderPath?: string | null
): FileReferencePaths | null {
  const target = parseLocalFileTarget(rawTarget)
  if (!target) return null

  if (isHomeRelativePath(target.path)) {
    return { absolute: target.path, relative: null }
  }

  const absolute = toAbsoluteFilePath(target.path, folderPath ?? undefined)
  if (!absolute) return null

  // `toFolderRelativePath` returns the absolute path unchanged when the file
  // lives outside the folder — that's "no relative form", not a relative path.
  const relative = folderPath
    ? toFolderRelativePath(absolute, folderPath)
    : null
  return {
    absolute,
    relative: relative && relative !== absolute ? relative : null,
  }
}

/** The reveal label matching the host OS, mirroring the file tree's "Open in". */
export function systemFileManagerLabelKey():
  | "openInFinder"
  | "openInExplorer"
  | "openInFileManager" {
  if (typeof navigator === "undefined") return "openInFileManager"
  const platform = `${navigator.platform} ${navigator.userAgent}`.toLowerCase()
  if (platform.includes("mac")) return "openInFinder"
  if (platform.includes("win")) return "openInExplorer"
  return "openInFileManager"
}

/**
 * The menu body. Mounted only while the menu is open, so a badge sitting in the
 * transcript pays for neither the active-folder subscription, the path
 * resolution, nor a translator.
 */
function FileReferenceActionsMenu({ target }: { target: string }) {
  const t = useTranslations("Folder.chat.fileActions")
  const { activeFolder } = useActiveFolder()
  const folderPath = activeFolder?.path
  const scope = useGrokSessionImageScope()
  const grokRef = scope ? parseGrokSessionImageRef(target) : null
  const canonicalGrokPath = grokRef?.path ?? null
  const grokExtension = grokRef?.extension ?? null
  const grokConversationId = scope?.conversationId ?? null
  const gateIdentity =
    grokConversationId !== null &&
    canonicalGrokPath !== null &&
    grokExtension !== null
      ? `${grokConversationId}\0${target}\0${canonicalGrokPath}`
      : null
  const gateToken = useMemo<GrokMenuGateToken | null>(
    () => (gateIdentity === null ? null : { identity: gateIdentity }),
    [gateIdentity]
  )
  const [grokResolution, setGrokResolution] = useState<GrokMenuResolution>({
    token: null,
    status: "failed",
    paths: null,
  })
  const genericPaths = useMemo(
    () => (gateToken ? null : resolveFileReferenceTarget(target, folderPath)),
    [gateToken, target, folderPath]
  )

  useEffect(() => {
    if (
      !gateToken ||
      grokConversationId === null ||
      canonicalGrokPath === null ||
      grokExtension === null
    ) {
      return
    }
    let cancelled = false
    // Fail closed in the same effect that launches the new keyed request.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setGrokResolution({ token: gateToken, status: "loading", paths: null })
    void resolveGrokSessionImage({
      conversationId: grokConversationId,
      href: target,
      includeData: false,
    }).then(
      (resolution) => {
        if (cancelled) return
        if (
          !resolution ||
          typeof resolution !== "object" ||
          typeof resolution.path !== "string" ||
          !isAbsoluteFilePath(resolution.path) ||
          (resolution.origin !== "session" &&
            resolution.origin !== "workspace") ||
          resolution.mimeType !==
            GROK_SESSION_IMAGE_MIME_BY_EXTENSION[grokExtension]
        ) {
          setGrokResolution({
            token: gateToken,
            status: "failed",
            paths: null,
          })
          return
        }
        setGrokResolution({
          token: gateToken,
          status: "ready",
          paths: {
            absolute: resolution.path,
            relative:
              resolution.origin === "workspace" ? canonicalGrokPath : null,
          },
        })
      },
      () => {
        if (!cancelled) {
          setGrokResolution({
            token: gateToken,
            status: "failed",
            paths: null,
          })
        }
      }
    )
    return () => {
      cancelled = true
    }
  }, [gateToken, grokConversationId, target, canonicalGrokPath, grokExtension])

  const paths = gateToken
    ? grokResolution.token === gateToken && grokResolution.status === "ready"
      ? grokResolution.paths
      : null
    : genericPaths

  const handleReveal = () => {
    if (!paths) return
    void (async () => {
      try {
        const absolute = isHomeRelativePath(paths.absolute)
          ? await expandHomePath(paths.absolute)
          : paths.absolute
        await revealItemInDir(absolute)
      } catch (error) {
        toast.error(t("openFailed"), { description: toErrorMessage(error) })
      }
    })()
  }

  const handleCopy = (value: string | null) => {
    if (!value) return
    // `copyTextFromMenu` defers the write until this menu has closed, so the
    // execCommand fallback still works in non-secure web contexts.
    void copyTextFromMenu(value).then((ok) => {
      if (ok) toast.success(t("pathCopied"))
      else toast.error(t("copyPathFailed"))
    })
  }

  return (
    <>
      {/* `revealItemInDir` is a no-op in web mode and for a desktop window
          bound to a remote workspace, so the row is hidden rather than dead. */}
      {isLocalDesktop() && (
        <ContextMenuItem disabled={!paths} onSelect={handleReveal}>
          {t(systemFileManagerLabelKey())}
        </ContextMenuItem>
      )}
      <ContextMenuItem
        disabled={!paths?.relative}
        onSelect={() => handleCopy(paths?.relative ?? null)}
      >
        {t("copyRelativePath")}
      </ContextMenuItem>
      <ContextMenuItem
        disabled={!paths?.absolute}
        onSelect={() => handleCopy(paths?.absolute ?? null)}
      >
        {t("copyAbsolutePath")}
      </ContextMenuItem>
    </>
  )
}

/**
 * Wraps an inline file badge in the transcript (user messages via
 * PlainTextWithBadges, assistant markdown via MarkdownLink) so that right-
 * clicking the file name opens its actions: reveal in the OS file manager, copy
 * relative path, copy absolute path. A non-file target renders its children
 * untouched, with no menu attached.
 *
 * A context menu (not a hover popover, and not a click target of its own) is
 * what leaves the badge's own behaviour intact: a left click still opens the
 * file in the workspace panel, reading past a file name pops nothing, and Radix
 * gives the long-press gesture on touch, keyboard navigation and Escape for
 * free.
 *
 * The conversation panel wraps the whole transcript in its own context menu
 * (copy text / new conversation / export / …, see
 * conversations/conversation-detail-panel.tsx), so the badge's trigger STOPS the
 * event from bubbling — otherwise both menus open at once. Right-clicking a file
 * name is therefore about that file; right-clicking anywhere else in the
 * transcript still gets the conversation menu.
 */
export function FileReferenceActions({
  target,
  children,
}: {
  target: string
  children: ReactNode
}) {
  // Folder-independent: whether this target is a local file at all. The full
  // resolution (which needs the active folder) happens inside the open menu.
  const scope = useGrokSessionImageScope()
  const grokRef = scope ? parseGrokSessionImageRef(target) : null
  const isLocalFile = useMemo(
    () => parseLocalFileTarget(target) !== null,
    [target]
  )

  if (!isLocalFile && !grokRef) return <>{children}</>

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <span
          data-file-actions=""
          className="inline-flex max-w-full items-center align-middle"
          // Radix's own handler still runs (it is merged onto this same
          // element); only the ancestor conversation-panel trigger is cut off.
          onContextMenu={(event) => event.stopPropagation()}
          // Touch/pen open a context menu from a long press, which is armed on
          // pointerdown — stop that press from arming the ancestor trigger too.
          // Mouse presses keep bubbling, so the panel's selection bookkeeping is
          // untouched.
          onPointerDown={(event) => {
            if (event.pointerType !== "mouse") event.stopPropagation()
          }}
        >
          {children}
        </span>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <FileReferenceActionsMenu target={target} />
      </ContextMenuContent>
    </ContextMenu>
  )
}
