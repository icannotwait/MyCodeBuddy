"use client"

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { useActiveFolder } from "@/contexts/active-folder-context"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { buildFileTabId } from "@/lib/file-tab-id"
import {
  gitDiff,
  gitDiffWithBranch,
  gitIsTracked,
  gitShowDiff,
  gitShowFile,
  readFileBase64,
  readFileForEdit,
  readFilePreview,
  saveFileContent,
  saveFileCopy,
} from "@/lib/api"
import type { FileEditContent } from "@/lib/types"
import {
  expandHomePath,
  findOwningFolder,
  isHomeRelativePath,
  joinRootRel,
  normalizeAbsPath,
  splitAbsPath,
} from "@/lib/file-open-target"
import { isAbsoluteFilePath } from "@/lib/file-path-display"
import {
  isHtmlPreviewable,
  isImageFile,
  isOfficePreviewable,
  languageFromPath,
} from "@/lib/language-detect"
import { toErrorMessage } from "@/lib/app-error"
import {
  buildSuggestedTranslationName,
  buildTranslationTabId,
  type DocumentTranslateFormat,
  type TranslationTransientMeta,
} from "@/lib/document-translate"
import {
  HIDDEN_TAB_CONTENT_BUDGET_CHARS,
  selectTabsToUnload,
} from "@/lib/file-tab-memory"
import { useWorkspaceStateStore } from "@/hooks/use-workspace-state-store"
import {
  useOpenFileTabsWatch,
  type WorkspaceExternalConflict,
} from "@/hooks/use-open-file-tabs-watch"
import { useOfficeAutoPreview } from "@/lib/office-preview-prefs"

export type WorkspaceMode = "conversation" | "fusion"
export type WorkspacePane = "conversation" | "files"
export type { TranslationTransientMeta, DocumentTranslateFormat }

type FileWorkspaceTabKind = "file" | "diff" | "rich-diff"
type FileSaveState = "idle" | "saving" | "error"
type LineEnding = "lf" | "crlf" | "mixed" | "none"

export interface FileWorkspaceTab {
  id: string
  kind: FileWorkspaceTabKind
  // Repo context for git-scoped diff tabs (working/branch/commit/session
  // diffs are repository operations and need the repo root). Plain file
  // tabs are folder-free: folderId is ALWAYS null and `path` holds the
  // file's absolute normalized path — reads/writes derive (dirname,
  // basename), and folder association (watching, git gutter, preview
  // roots) is derived from the path on demand, never stored.
  folderId: number | null
  title: string
  description: string | null
  path: string | null
  language: string
  content: string
  loading: boolean
  originalContent?: string
  modifiedContent?: string
  gitBaseContent?: string
  savedContent?: string
  isDirty?: boolean
  etag?: string | null
  mtimeMs?: number | null
  readonly?: boolean
  lineEnding?: LineEnding
  saveState?: FileSaveState
  saveError?: string | null
  // True iff an external change to this tab's path was observed by the
  // workspace watcher while the tab was inactive or otherwise not yet
  // resolved against disk. Cleared by any successful content reload.
  stale?: boolean
  // True after at least one successful content settle for this tab. Cold
  // open failures (never true) remove the tab; warm failures keep content.
  hasLoadedSuccessfully: boolean
  /**
   * Pathless in-memory result tabs (document translation). Not disk-watched;
   * pinned against content eviction until the user closes the tab.
   */
  transient?: TranslationTransientMeta
}

export type OpenFileOptions = {
  line?: number
  reload?: boolean
  folderId?: number
  /** default true for openFilePreview; false for office auto-preview */
  maximizeOnSuccess?: boolean
}

/** Settle outcome for openFilePreview (Save-as chaining and callers). */
export type OpenFileSettleResult =
  | { ok: true; tabId: string }
  | { ok: false; reason: "resolve" | "load" | "closed" | "stale" }

// The provider value is split across three contexts so high-frequency
// fileTabs churn (per-keystroke content updates, watcher-driven reloads)
// only re-renders components that actually read tab data. Action-only
// consumers on the conversation render path (message nav, artifacts,
// links, search) subscribe to WorkspaceActionsContext, whose value is
// stable for the provider's lifetime; layout chrome subscribes to
// WorkspaceViewContext, which only changes on mode/pane/maximize flips.
interface WorkspaceActionsValue {
  setActivePane: (pane: WorkspacePane) => void
  activateConversationPane: () => void
  activateFilePane: () => void
  switchFileTab: (tabId: string) => void
  closeFileTab: (tabId: string) => void
  closeOtherFileTabs: (tabId: string) => void
  closeAllFileTabs: () => void
  reorderFileTabs: (tabs: FileWorkspaceTab[]) => void
  // Open a file tab. Accepts absolute paths, `~/` paths (expanded via the
  // backend home dir), and paths relative to a folder root. `folderId` is
  // ONLY a resolution base for relative paths (defaults to the active
  // folder); once the path is absolute it plays no further role — the tab
  // is identified by the absolute path alone.
  openFilePreview: (
    path: string,
    options?: OpenFileOptions
  ) => Promise<OpenFileSettleResult>
  // Refetch the open tab matching the absolute `path` without changing
  // activeFileTabId. No-op when no tab matches or when the tab has unsaved
  // local edits (use markTabsStale for that case).
  reloadOpenFileBackground: (path: string) => Promise<void>
  // Write prefetched file content into the open tab matching the absolute
  // `path` without issuing a second readFileForEdit. Used by the
  // change-detection watcher whose resolver has already paid for the read —
  // avoids the I/O double when many tabs are affected by a single workspace
  // event. Skips dirty tabs and tabs that aren't open.
  applyExternalReload: (path: string, fetched: FileEditContent) => Promise<void>
  // Flip stale=true on the tab matching the absolute `path`. Activating a
  // stale tab forces a refetch (clean) or triggers conflict resolution
  // (dirty).
  markTabsStale: (path: string) => void
  // Mark a clean open tab as load-failed, replacing its body with the
  // supplied error message and routing it into the editor's error state.
  // No-op when no tab matches OR when the tab is dirty — unsaved edits
  // must never be silently clobbered. Used by the watcher when a workspace
  // event reports a path whose disk read fails (external delete, locked,
  // permission revoked, …), so the user is never shown a stale buffer that
  // no longer corresponds to disk.
  rejectFileTab: (path: string, errorMessage: string) => void
  consumePendingFileReveal: (requestId: number) => void
  openWorkingTreeDiff: (
    path?: string,
    options?: {
      mode?: "auto" | "unified" | "overview"
      folderId?: number
    }
  ) => Promise<void>
  openBranchDiff: (
    branch: string,
    path?: string,
    options?: { mode?: "default" | "overview"; folderId?: number }
  ) => Promise<void>
  openCommitDiff: (
    commit: string,
    path?: string,
    message?: string,
    options?: { folderId?: number }
  ) => Promise<void>
  openSessionFileDiff: (
    filePath: string,
    diffContent: string,
    groupLabel: string,
    options?: { folderId?: number }
  ) => void
  openExternalConflictDiff: (
    filePath: string,
    diskContent: string,
    unsavedContent: string
  ) => void
  updateActiveFileContent: (content: string) => void
  saveActiveFile: (options?: { force?: boolean }) => Promise<boolean>
  reloadActiveFile: () => Promise<void>
  toggleFileTabPreview: (tabId: string) => void
  toggleFilesMaximized: () => void
  /**
   * Bump the per-source translate request generation and return the new gen.
   * Call at click time (with the content snapshot) before the async API.
   */
  beginTranslateRequest: (sourceTabId: string) => number
  /**
   * Insert a transient readonly translation result tab when `requestGen`
   * still matches the latest gen for `sourceTabId`. Returns the new tab id,
   * or null when the result is stale (newer request, or provider unmounted).
   * Closing the source tab does not cancel an in-flight result.
   */
  openTranslationResultTab: (input: {
    sourceTabId: string
    requestGen: number
    content: string
    locale: string
    format: DocumentTranslateFormat
    sourcePath: string | null
    sourceContentHash: string
    sourceTitle: string
  }) => string | null
}

interface WorkspaceViewValue {
  mode: WorkspaceMode
  activePane: WorkspacePane
  filesMaximized: boolean
}

interface WorkspaceFileTabsValue {
  fileTabs: FileWorkspaceTab[]
  activeFileTabId: string | null
  activeFileTab: FileWorkspaceTab | null
  activeFilePath: string | null
  previewFileTabIds: Set<string>
  pendingFileReveal: {
    requestId: number
    // Absolute normalized path — compared against the active tab's path.
    path: string
    line: number
  } | null
}

type WorkspaceContextValue = WorkspaceActionsValue &
  WorkspaceViewValue &
  WorkspaceFileTabsValue

// External disk-vs-buffer conflicts, isolated from the high-frequency
// fileTabs slice so the always-mounted conflict dialog costs nothing while
// idle. Conflicts queue FIFO (multi-folder divergences can land together);
// the head is surfaced one at a time.
interface WorkspaceExternalConflictValue {
  // Head of the conflict queue, or null when there is nothing to resolve.
  externalConflict: WorkspaceExternalConflict | null
  // "Compare": open a disk-vs-unsaved rich diff tab (uses the LATEST
  // buffer content when the tab is still open) and dequeue.
  compareExternalConflict: () => void
  // "Reload": discard the buffer and refetch from disk; clears the shown
  // signature so a subsequent identical divergence prompts again.
  reloadExternalConflict: () => void
  // "Save as copy": write the unsaved buffer next to the original.
  // Resolves with the saved path (dequeues) or throws on failure (the
  // conflict stays queued so the user can retry).
  saveExternalConflictCopy: () => Promise<string>
  // Close the dialog without resolving; the signature stays recorded so
  // the same divergence does not immediately re-prompt.
  dismissExternalConflict: () => void
}

const WorkspaceActionsContext = createContext<WorkspaceActionsValue | null>(
  null
)
const WorkspaceViewContext = createContext<WorkspaceViewValue | null>(null)
const WorkspaceFileTabsContext = createContext<WorkspaceFileTabsValue | null>(
  null
)
const WorkspaceExternalConflictContext =
  createContext<WorkspaceExternalConflictValue | null>(null)

// Queue/dedup key for one file's divergence — the absolute normalized
// path IS the identity, matching the tab id model.
function conflictKey(path: string): string {
  return normalizeAbsPath(path)
}

// One-shot save-echo records: our own saveFileContent writes come back as
// watcher change events; suppress exactly one event per save (etag match,
// clean tab, short TTL) so an autosave before a tab switch doesn't flag
// the tab stale and force a pointless reload on switch-back.
const SELF_WRITE_ECHO_TTL_MS = 5_000

function normalizePath(path: string): string {
  return path.replace(/\\/g, "/")
}

function fileName(path: string): string {
  return path.split("/").pop() || path
}

function isDirtyFileTab(tab: FileWorkspaceTab): boolean {
  return tab.kind === "file" && Boolean(tab.isDirty)
}

// Share one string instance when the git base equals the working copy —
// the common case for files without uncommitted changes. Halves the
// retained text per clean tracked file.
function dedupeGitBase(
  content: string,
  gitBaseContent: string | undefined
): string | undefined {
  return gitBaseContent === content ? content : gitBaseContent
}

// Re-exported for existing consumers; the implementation lives in
// lib/language-detect so the tab watcher can use it without a runtime
// import cycle back into this module.
export { isImageFile } from "@/lib/language-detect"

const IMAGE_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  svg: "image/svg+xml",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
}

function loadingTab(
  id: string,
  folderId: number | null,
  kind: FileWorkspaceTabKind,
  title: string,
  description: string | null,
  path: string | null,
  language: string
): FileWorkspaceTab {
  return {
    id,
    kind,
    folderId,
    title,
    description,
    path,
    language,
    content: "",
    loading: true,
    savedContent: "",
    isDirty: false,
    etag: null,
    mtimeMs: null,
    readonly: kind !== "file",
    lineEnding: "none",
    saveState: "idle",
    saveError: null,
    hasLoadedSuccessfully: false,
  }
}

type LoadDecision = { kind: "skip" } | { kind: "fetch"; gen: number }

async function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  timeoutMessage: string
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null
  const timeoutPromise = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(timeoutMessage))
    }, timeoutMs)
  })

  try {
    return await Promise.race([promise, timeoutPromise])
  } finally {
    if (timer) clearTimeout(timer)
  }
}

/**
 * Soft side-read for rich-diff openers: a rejection becomes an empty-side
 * fallback with `ok: false`. One-side failure still yields a usable empty
 * panel; both-side failure is a cold open failure.
 */
function softSideRead<T>(
  promise: Promise<T>,
  fallback: T
): Promise<{ ok: true; value: T } | { ok: false; value: T; error: unknown }> {
  return promise.then(
    (value) => ({ ok: true as const, value }),
    (error: unknown) => ({ ok: false as const, value: fallback, error })
  )
}

interface WorkspaceProviderProps {
  children: ReactNode
}

export function WorkspaceProvider({ children }: WorkspaceProviderProps) {
  const t = useTranslations("Folder.workspaceContext")
  const tFiles = useTranslations("Folder.fileWorkspace")
  const { activeFolder } = useActiveFolder()
  // Reactive: `useOpenFileTabsWatch` re-derives its per-root FS subscriptions
  // when the registered-folder set changes. Low-frequency (open/close folder).
  const allFolders = useAppWorkspaceStore((s) => s.allFolders)
  const folderPath = activeFolder?.path
  const [activePane, setActivePaneState] =
    useState<WorkspacePane>("conversation")
  const [fileTabs, setFileTabs] = useState<FileWorkspaceTab[]>([])
  const [activeFileTabId, setActiveFileTabId] = useState<string | null>(null)
  const [pendingFileReveal, setPendingFileReveal] = useState<{
    requestId: number
    path: string
    line: number
  } | null>(null)
  const [previewFileTabIds, setPreviewFileTabIds] = useState<Set<string>>(
    new Set()
  )
  const [filesMaximized, setFilesMaximized] = useState(false)
  // FIFO queue of unresolved disk-vs-buffer divergences (head is shown by
  // the always-mounted conflict dialog). Isolated state: never flows into
  // the fileTabs slice, so idle cost is zero.
  const [externalConflictQueue, setExternalConflictQueue] = useState<
    WorkspaceExternalConflict[]
  >([])
  const externalConflictQueueRef = useRef<WorkspaceExternalConflict[]>([])
  // key(folderId,path) -> last announced signature. Suppresses re-prompt
  // flicker when repeated events report the same divergence.
  const conflictSignatureByKeyRef = useRef<Map<string, string>>(new Map())
  // key(folderId,path) -> etag of our own most recent save (one-shot).
  const selfWriteEchoRef = useRef<Map<string, { etag: string; at: number }>>(
    new Map()
  )
  const fileTabsRef = useRef<FileWorkspaceTab[]>([])
  // Latest-state mirrors for the stable action callbacks. Actions live in a
  // context value that must NOT change identity when tabs/folder change, so
  // they read these refs instead of capturing render-scoped state. The refs
  // are synced in effects (post-commit), giving the same staleness window a
  // recreated closure would have had — never fresher, never older.
  const activeFileTabIdRef = useRef<string | null>(null)
  // Tab ids that should maximize the files pane on their first successful
  // settle (user-initiated opens). Never set at seed time.
  const pendingMaximizeOnSuccessRef = useRef<Set<string>>(new Set())
  const activeFolderRef = useRef<{ id: number; path: string } | null>(null)
  const fileRevealRequestIdRef = useRef(0)
  // tabId -> generation of its current in-flight fetch. Serves two roles:
  //   (a) Dedup: `has(tabId)` collapses rapid re-clicks within one event
  //       loop turn (where fileTabsRef.current is still pre-render-stale).
  //   (b) Staleness check: each fetch captures the generation it was
  //       started with and only commits state on resolve if it still
  //       matches — preventing an orphaned fetch (after close+reopen, or
  //       a superseding refresh) from clobbering the tab.
  const inFlightLoadsRef = useRef<Map<string, number>>(new Map())
  // tabId -> shared settle Deferred for the current openFilePreview gen.
  // Concurrent openFilePreview calls that hit the in-flight dedup await this
  // so they observe the real settle outcome (not a premature ok:true).
  // `gen` scopes the Deferred so a late settle from a prior open (close +
  // reopen same path) cannot resolve or delete the newer generation's entry.
  const openSettleDeferredRef = useRef<
    Map<
      string,
      {
        gen: number
        promise: Promise<OpenFileSettleResult>
        resolve: (result: OpenFileSettleResult) => void
      }
    >
  >(new Map())
  const nextLoadGenRef = useRef(0)
  // Most-recently-active tab ids, most recent first. Drives the memory
  // guardrail's least-recently-active eviction order.
  const tabRecencyRef = useRef<string[]>([])
  // Per source-tab request generation for document translation. Late API
  // results whose gen no longer matches are dropped (no result tab).
  const translateRequestGenRef = useRef(new Map<string, number>())
  // Drop late translation results after provider unmount.
  const providerAliveRef = useRef(true)

  useEffect(() => {
    providerAliveRef.current = true
    return () => {
      providerAliveRef.current = false
    }
  }, [])

  useEffect(() => {
    fileTabsRef.current = fileTabs
  }, [fileTabs])

  useEffect(() => {
    activeFileTabIdRef.current = activeFileTabId
  }, [activeFileTabId])

  useEffect(() => {
    activeFolderRef.current = activeFolder
      ? { id: activeFolder.id, path: activeFolder.path }
      : null
  }, [activeFolder])

  useEffect(() => {
    externalConflictQueueRef.current = externalConflictQueue
  }, [externalConflictQueue])

  const recordSelfWriteEcho = useCallback(
    (path: string, etag: string | null | undefined) => {
      if (!etag) return
      selfWriteEchoRef.current.set(conflictKey(path), {
        etag,
        at: Date.now(),
      })
    },
    []
  )

  // One-shot: a hit consumes the record, so only the single event burst
  // produced by our own write is suppressed — any later change for the
  // same path marks stale normally. The tab-etag equality check is done
  // by the caller being a CLEAN tab whose etag was set by that same save.
  const consumeSelfWriteEcho = useCallback((path: string): boolean => {
    const key = conflictKey(path)
    const record = selfWriteEchoRef.current.get(key)
    if (!record) return false
    selfWriteEchoRef.current.delete(key)
    if (Date.now() - record.at > SELF_WRITE_ECHO_TTL_MS) return false
    const tabId = buildFileTabId({ kind: "file", path: key })
    const tab = fileTabsRef.current.find((t) => t.id === tabId)
    return Boolean(
      tab && tab.kind === "file" && !tab.isDirty && tab.etag === record.etag
    )
  }, [])

  // Resolve the folder an opener should target: an explicitly requested
  // folder wins; otherwise the active folder. Returns null when neither
  // resolves (no folder open, or the requested folder was removed).
  const resolveTargetFolder = useCallback(
    (explicitFolderId?: number): { id: number; path: string } | null => {
      if (explicitFolderId != null) {
        const folder = useAppWorkspaceStore
          .getState()
          .getFolder(explicitFolderId)
        return folder ? { id: folder.id, path: folder.path } : null
      }
      return activeFolderRef.current
    },
    []
  )

  // Resolve an opener input into the canonical absolute path that is the
  // tab's identity. Absolute and `~/` inputs need no folder at all;
  // relative inputs are joined onto `folderId` (or the active folder) —
  // that is the ONLY role a folder plays in opening a file.
  const resolveOpenAbsolutePath = useCallback(
    async (rawPath: string, baseFolderId?: number): Promise<string | null> => {
      const input = isHomeRelativePath(rawPath)
        ? await expandHomePath(rawPath)
        : rawPath
      if (isAbsoluteFilePath(input)) {
        const abs = normalizeAbsPath(input)
        // Re-root through the owning registered folder when there is one:
        // on case-insensitive filesystems an agent may echo the root with
        // different casing (c:/repo vs C:/Repo), and watch events join the
        // FOLDER's stored casing — canonicalizing here collapses those
        // aliases into the one identity the watcher reproduces.
        const owning = findOwningFolder(
          abs,
          useAppWorkspaceStore.getState().allFolders
        )
        return owning ? joinRootRel(owning.rootPath, owning.relPath) : abs
      }
      const base = resolveTargetFolder(baseFolderId)
      if (!base) return null
      return joinRootRel(base.path, normalizePath(input))
    },
    [resolveTargetFolder]
  )

  // Git gutter base for the file at absPath, derived from its owning
  // registered folder at fetch time. Files outside every registered folder
  // get no git context — the parent directory may sit inside some unrelated
  // repo (a dotfiles repo in $HOME), and spawning git there would paint
  // misleading gutters.
  const fetchGitBase = useCallback(
    async (absPath: string): Promise<string | undefined> => {
      const owning = findOwningFolder(
        absPath,
        useAppWorkspaceStore.getState().allFolders
      )
      if (!owning) return undefined
      const tracked = await gitIsTracked(owning.rootPath, owning.relPath).catch(
        () => false
      )
      if (!tracked) return undefined
      return gitShowFile(owning.rootPath, owning.relPath).catch(() => "")
    },
    []
  )

  const mode: WorkspaceMode = fileTabs.length > 0 ? "fusion" : "conversation"
  const effectiveFilesMaximized = mode === "fusion" && filesMaximized

  // Reset maximize state once the file workspace is empty so reopening a file
  // later starts from the normal split instead of a stale maximized layout.
  useEffect(() => {
    if (fileTabs.length === 0 && filesMaximized) {
      /* eslint-disable react-hooks/set-state-in-effect */
      setFilesMaximized(false)
      /* eslint-enable react-hooks/set-state-in-effect */
    }
  }, [fileTabs.length, filesMaximized])

  const toggleFilesMaximized = useCallback(() => {
    setFilesMaximized((prev) => !prev)
  }, [])

  const setActivePane = useCallback((nextPane: WorkspacePane) => {
    setActivePaneState((prev) => (prev === nextPane ? prev : nextPane))
  }, [])

  const activateConversationPane = useCallback(() => {
    setActivePaneState((prev) =>
      prev === "conversation" ? prev : "conversation"
    )
    // Releasing the files overlay so a session opened from the sidebar (or any
    // other path that activates the conversation pane) becomes visible instead
    // of staying hidden behind a maximized files pane.
    setFilesMaximized(false)
  }, [])

  const activateFilePane = useCallback(() => {
    setActivePaneState((prev) => (prev === "files" ? prev : "files"))
  }, [])

  // NOTE: there is deliberately NO folder-removal cleanup for file tabs.
  // A file tab is identified by its absolute path — removing a workspace
  // folder does not delete the files, so its tabs stay open and simply
  // degrade to unwatched (activation-time freshness + save pre-verify).
  // Git-scoped diff tabs keep their folderId but are snapshots; a gone
  // folder surfaces as a load error on the next refresh, not a wipe.

  // Pure activation — no content mutation.
  const activateTab = useCallback(
    (tabId: string) => {
      setActiveFileTabId(tabId)
      activeFileTabIdRef.current = tabId
      activateFilePane()
    },
    [activateFilePane]
  )

  // Insert a freshly created (loading, empty) tab. Caller has verified no tab
  // with this id exists. If a race introduced one, leave it alone.
  const seedLoadingTab = useCallback(
    (nextTab: FileWorkspaceTab) => {
      setFileTabs((prev) => {
        if (prev.some((tab) => tab.id === nextTab.id)) return prev
        return [...prev, nextTab]
      })
      setActiveFileTabId(nextTab.id)
      activeFileTabIdRef.current = nextTab.id
      // Keep fileTabsRef current for settle paths that run after await within
      // the same turn (tests / microtask-resolved reads).
      fileTabsRef.current = fileTabsRef.current.some((t) => t.id === nextTab.id)
        ? fileTabsRef.current
        : [...fileTabsRef.current, nextTab]
      activateFilePane()
      // Open HTML/Markdown file tabs in the rendered preview by default rather
      // than the source editor. Only runs on first seed: reloads go through
      // markTabRefreshing (never here), so if the user later switches to the
      // source view it survives an external change. Restricted to real file
      // tabs — diffs never enter preview, and .vue/.svelte (language "html"
      // but not isHtmlPreviewable) stay on source.
      if (
        nextTab.kind === "file" &&
        (nextTab.language === "markdown" || isHtmlPreviewable(nextTab.path))
      ) {
        setPreviewFileTabIds((prev) => {
          if (prev.has(nextTab.id)) return prev
          const next = new Set(prev)
          next.add(nextTab.id)
          return next
        })
      }
    },
    [activateFilePane]
  )

  // Mark an existing tab as refreshing. Preserves content / originalContent /
  // modifiedContent / gitBaseContent / savedContent / etag / mtimeMs /
  // isDirty / readonly / lineEnding. Clears any prior error state.
  const markTabRefreshing = useCallback((tabId: string) => {
    setFileTabs((prev) =>
      prev.map((tab) =>
        tab.id === tabId
          ? {
              ...tab,
              loading: true,
              saveState: "idle",
              saveError: null,
            }
          : tab
      )
    )
  }, [])

  // Reset an errored tab for retry. Prefer last-known-good buffer
  // (`savedContent`) so a warm fail cannot leave the tab blank after
  // rejectFileTab wrote an error body into `content`.
  const markErrorRetry = useCallback(
    (tabId: string, kind: FileWorkspaceTabKind) => {
      const patchTab = (tab: FileWorkspaceTab): FileWorkspaceTab => {
        if (tab.id !== tabId) return tab
        // Error body is in content; restore from savedContent when present.
        // Otherwise keep prior content only if it was not an error state.
        const restoreContent =
          typeof tab.savedContent === "string" && tab.savedContent.length > 0
            ? tab.savedContent
            : tab.saveState === "error"
              ? ""
              : tab.content
        return {
          ...tab,
          loading: true,
          content: restoreContent,
          originalContent:
            kind === "rich-diff" ? undefined : tab.originalContent,
          modifiedContent:
            kind === "rich-diff" ? undefined : tab.modifiedContent,
          saveState: "idle" as const,
          saveError: null,
        }
      }
      setFileTabs((prev) => prev.map(patchTab))
      fileTabsRef.current = fileTabsRef.current.map(patchTab)
    },
    []
  )

  // Replace an entire tab atomically. Used for synchronous content sources
  // (session diffs, external-conflict diffs) where the caller already holds
  // the final content.
  const replaceTabContent = useCallback(
    (nextTab: FileWorkspaceTab) => {
      setFileTabs((prev) => {
        const idx = prev.findIndex((tab) => tab.id === nextTab.id)
        if (idx < 0) return [...prev, nextTab]
        const updated = [...prev]
        updated[idx] = nextTab
        return updated
      })
      setActiveFileTabId(nextTab.id)
      activeFileTabIdRef.current = nextTab.id
      activateFilePane()
    },
    [activateFilePane]
  )

  const startOpenSettle = useCallback((tabId: string, gen: number) => {
    let resolve!: (result: OpenFileSettleResult) => void
    const promise = new Promise<OpenFileSettleResult>((res) => {
      resolve = res
    })
    openSettleDeferredRef.current.set(tabId, { gen, promise, resolve })
    return promise
  }, [])

  const finishOpenSettle = useCallback(
    (tabId: string, gen: number, result: OpenFileSettleResult) => {
      const deferred = openSettleDeferredRef.current.get(tabId)
      // Only the matching generation may resolve/delete the Deferred.
      if (!deferred || deferred.gen !== gen) return
      openSettleDeferredRef.current.delete(tabId)
      deferred.resolve(result)
    },
    []
  )

  // Finish an in-flight open settle as "closed" for the gen currently tracked
  // on inFlightLoadsRef (if any). Captures gen before deleting the marker so
  // a subsequent reopen can start a new gen Deferred safely.
  const finishOpenSettleClosed = useCallback(
    (tabId: string) => {
      const gen = inFlightLoadsRef.current.get(tabId)
      inFlightLoadsRef.current.delete(tabId)
      if (gen === undefined) return
      finishOpenSettle(tabId, gen, { ok: false, reason: "closed" })
    },
    [finishOpenSettle]
  )

  // Eager same-turn ref patch so warm/cold classification after await does
  // not depend on React committing the matching setFileTabs.
  const patchFileTabRef = useCallback(
    (tabId: string, patch: Partial<FileWorkspaceTab>) => {
      fileTabsRef.current = fileTabsRef.current.map((tab) =>
        tab.id === tabId ? { ...tab, ...patch } : tab
      )
    },
    []
  )

  // Orchestrates the "I want to start (or restart) a load for this tab" flow.
  // Encapsulates: cache short-circuit, in-flight dedup, error retry, forced
  // refresh, and cold-load creation. Returns whether the caller should
  // proceed with its fetch.
  const beginFetchGeneration = useCallback((tabId: string): number => {
    nextLoadGenRef.current += 1
    const gen = nextLoadGenRef.current
    inFlightLoadsRef.current.set(tabId, gen)
    return gen
  }, [])

  const decideLoad = useCallback(
    (
      seed: FileWorkspaceTab,
      reload: boolean,
      options?: { maximizeOnSuccess?: boolean }
    ): LoadDecision => {
      // Dedup synchronously. inFlightLoadsRef is updated immediately on
      // generation start, so rapid re-clicks within a single event loop
      // turn collapse here — unlike fileTabsRef.current, which only
      // reflects state after React flushes a render.
      if (inFlightLoadsRef.current.has(seed.id)) {
        activateTab(seed.id)
        return { kind: "skip" }
      }

      const existing = fileTabsRef.current.find((t) => t.id === seed.id)
      if (!existing) {
        // "reload" means "refresh an existing tab". If the tab is gone —
        // e.g. the user closed it while a watcher-driven reload was in
        // flight — do not resurrect it as a phantom tab.
        if (reload) return { kind: "skip" }
        seedLoadingTab(seed)
        if (options?.maximizeOnSuccess) {
          pendingMaximizeOnSuccessRef.current.add(seed.id)
        }
        return { kind: "fetch", gen: beginFetchGeneration(seed.id) }
      }

      activateTab(existing.id)
      // Existing-tab activate: never enqueue maximize-on-success.

      if (existing.saveState === "error") {
        markErrorRetry(existing.id, existing.kind)
        return { kind: "fetch", gen: beginFetchGeneration(seed.id) }
      }

      // Stale clean tab — the watcher saw an external change while we were
      // inactive. Promote to reload now so the user never sees stale bytes.
      // Stale dirty tabs are NOT auto-reloaded: conflict resolution belongs
      // to the watcher, which surfaces the prompt instead of clobbering
      // unsaved edits.
      const stalePromotesReload =
        existing.kind === "file" && existing.stale === true && !existing.isDirty

      if (!reload && !stalePromotesReload) {
        // Cache hit — nothing to do.
        return { kind: "skip" }
      }

      markTabRefreshing(existing.id)
      return { kind: "fetch", gen: beginFetchGeneration(seed.id) }
    },
    [
      activateTab,
      beginFetchGeneration,
      markErrorRetry,
      markTabRefreshing,
      seedLoadingTab,
    ]
  )

  // Variant of decideLoad for diff tabs: content is inherently volatile
  // (git state changes), so we always refetch — but non-destructively.
  const beginDiffLoad = useCallback(
    (seed: FileWorkspaceTab): { skip: true } | { skip: false; gen: number } => {
      if (inFlightLoadsRef.current.has(seed.id)) {
        activateTab(seed.id)
        return { skip: true }
      }

      const existing = fileTabsRef.current.find((t) => t.id === seed.id)
      if (!existing) {
        seedLoadingTab(seed)
        return { skip: false, gen: beginFetchGeneration(seed.id) }
      }

      activateTab(seed.id)
      if (existing.saveState === "error") {
        markErrorRetry(seed.id, seed.kind)
      } else {
        markTabRefreshing(seed.id)
      }
      return { skip: false, gen: beginFetchGeneration(seed.id) }
    },
    [
      activateTab,
      beginFetchGeneration,
      markErrorRetry,
      markTabRefreshing,
      seedLoadingTab,
    ]
  )

  // Called from every fetch's resolve/error path. Returns true iff this
  // particular fetch is still the canonical in-flight load for the tab —
  // i.e. the user hasn't closed the tab, switched folders, or started a
  // newer fetch in the meantime. Also performs the cleanup atomically.
  const settleFetch = useCallback((tabId: string, gen: number): boolean => {
    if (inFlightLoadsRef.current.get(tabId) !== gen) return false
    inFlightLoadsRef.current.delete(tabId)
    return true
  }, [])

  const resolveTab = useCallback(
    (tabId: string, content: string, loading = false) => {
      const patch = loading
        ? { content, loading }
        : {
            content,
            loading: false,
            hasLoadedSuccessfully: true as const,
            saveState: "idle" as const,
          }
      setFileTabs((prev) =>
        prev.map((tab) => (tab.id === tabId ? { ...tab, ...patch } : tab))
      )
      if (!loading) {
        patchFileTabRef(tabId, patch)
      }
    },
    [patchFileTabRef]
  )

  // Remove a tab without dirty confirm (cold-open / cold-diff failure path).
  // Mirrors closeFileTab's active-id repair and in-flight cleanup.
  const removeFileTabId = useCallback(
    (tabId: string) => {
      pendingMaximizeOnSuccessRef.current.delete(tabId)
      // Do not finishOpenSettle here: cold-fail callers still need to
      // resolve the Deferred with reason "load". User close paths finish
      // settle themselves with reason "closed".
      setFileTabs((prev) => {
        const idx = prev.findIndex((tab) => tab.id === tabId)
        if (idx < 0) return prev

        const next = prev.filter((candidate) => candidate.id !== tabId)

        setActiveFileTabId((current) => {
          if (current !== tabId) return current
          if (next.length === 0) {
            activateConversationPane()
            activeFileTabIdRef.current = null
            return null
          }
          const nextIdx = Math.min(idx, next.length - 1)
          const nextId = next[nextIdx].id
          activeFileTabIdRef.current = nextId
          return nextId
        })

        setPreviewFileTabIds((ids) => {
          if (!ids.has(tabId)) return ids
          const updated = new Set(ids)
          updated.delete(tabId)
          return updated
        })

        inFlightLoadsRef.current.delete(tabId)
        fileTabsRef.current = next
        return next
      })
    },
    [activateConversationPane]
  )

  const maybeMaximizeAfterSuccess = useCallback((tabId: string) => {
    if (!pendingMaximizeOnSuccessRef.current.has(tabId)) return
    pendingMaximizeOnSuccessRef.current.delete(tabId)
    if (activeFileTabIdRef.current === tabId) {
      setFilesMaximized(true)
    }
  }, [])

  // Cold/warm open failure matrix (files + diffs). rejectFileTab is separate.
  const failOpenTab = useCallback(
    (tabId: string, displayName: string, error: unknown) => {
      const detail = toErrorMessage(error)
      console.error("[file-open]", tabId, detail)
      pendingMaximizeOnSuccessRef.current.delete(tabId)
      const existing = fileTabsRef.current.find((t) => t.id === tabId)
      if (existing?.hasLoadedSuccessfully) {
        // Warm keep: restore last-known-good if markErrorRetry / reject left
        // content empty or only an error body while savedContent still holds
        // the prior buffer.
        const patchTab = (tab: FileWorkspaceTab): FileWorkspaceTab => {
          if (tab.id !== tabId) return tab
          const hasBody = tab.content.length > 0
          const saved =
            typeof tab.savedContent === "string" ? tab.savedContent : ""
          const content = hasBody ? tab.content : saved
          return { ...tab, content, loading: false }
        }
        setFileTabs((prev) => prev.map(patchTab))
        fileTabsRef.current = fileTabsRef.current.map(patchTab)
      } else {
        removeFileTabId(tabId)
      }
      toast.error(t("unableOpenFile", { name: displayName }))
    },
    [removeFileTabId, t]
  )

  const resolveRichDiffTab = useCallback(
    (
      tabId: string,
      originalContent: string,
      modifiedContent: string,
      loading = false
    ) => {
      const patch = loading
        ? {
            originalContent,
            modifiedContent,
            content: "",
            loading: true,
          }
        : {
            originalContent,
            modifiedContent,
            content: "",
            loading: false,
            hasLoadedSuccessfully: true as const,
          }
      setFileTabs((prev) =>
        prev.map((tab) => (tab.id === tabId ? { ...tab, ...patch } : tab))
      )
      if (!loading) {
        patchFileTabRef(tabId, patch)
      }
    },
    [patchFileTabRef]
  )

  const consumePendingFileReveal = useCallback((requestId: number) => {
    setPendingFileReveal((prev) =>
      prev && prev.requestId === requestId ? null : prev
    )
  }, [])

  // Background reload: refresh an open tab's content without changing
  // activeFileTabId or activating the file pane. Used by the workspace
  // watcher when an external change touches a clean tab the user isn't
  // currently looking at — VS Code / IntelliJ silently absorb such changes
  // so the next activation sees the latest bytes. Dirty tabs are off-limits
  // (conflict resolution belongs to the watcher via markTabsStale).
  const reloadOpenFileBackground = useCallback(
    async (rawPath: string) => {
      const absPath = normalizeAbsPath(rawPath)
      const io = splitAbsPath(absPath)
      if (!io) return
      const tabId = buildFileTabId({ kind: "file", path: absPath })
      const existing = fileTabsRef.current.find((t) => t.id === tabId)
      if (!existing || existing.kind !== "file") return
      if (existing.isDirty) return
      if (inFlightLoadsRef.current.has(tabId)) return

      const image = isImageFile(absPath)

      markTabRefreshing(tabId)
      const gen = beginFetchGeneration(tabId)

      try {
        if (image) {
          const ext = absPath.split(".").pop()?.toLowerCase() ?? ""
          const mime = IMAGE_MIME[ext] ?? "image/png"
          const b64 = await withTimeout(
            readFileBase64(absPath),
            15_000,
            t("previewRequestTimedOut")
          )
          if (!settleFetch(tabId, gen)) return
          const imagePatch = {
            content: `data:${mime};base64,${b64}`,
            savedContent: `data:${mime};base64,${b64}`,
            readonly: true as const,
            loading: false,
            saveState: "idle" as const,
            saveError: null,
            stale: false,
            hasLoadedSuccessfully: true as const,
          }
          setFileTabs((prev) =>
            prev.map((tab) =>
              tab.id === tabId ? { ...tab, ...imagePatch } : tab
            )
          )
          patchFileTabRef(tabId, imagePatch)
          return
        }

        const [result, gitBaseContent] = await withTimeout(
          Promise.all([
            readFileForEdit(io.rootPath, io.ioPath),
            fetchGitBase(absPath),
          ]),
          15_000,
          t("previewRequestTimedOut")
        )
        if (!settleFetch(tabId, gen)) return
        const textPatch = {
          content: result.content,
          gitBaseContent: dedupeGitBase(result.content, gitBaseContent),
          savedContent: result.content,
          isDirty: false,
          etag: result.etag,
          mtimeMs: result.mtime_ms,
          readonly: result.readonly,
          lineEnding: result.line_ending,
          saveState: "idle" as const,
          saveError: null,
          loading: false,
          stale: false,
          hasLoadedSuccessfully: true as const,
        }
        setFileTabs((prev) =>
          prev.map((tab) => (tab.id === tabId ? { ...tab, ...textPatch } : tab))
        )
        patchFileTabRef(tabId, textPatch)
      } catch (error) {
        if (!settleFetch(tabId, gen)) return
        // Warm toast if previously loaded; no-op path when tab was closed
        // (settle already returned false above). Never invent error tabs.
        const stillOpen = fileTabsRef.current.some((t) => t.id === tabId)
        if (!stillOpen) return
        failOpenTab(tabId, fileName(absPath), error)
      }
    },
    [
      beginFetchGeneration,
      failOpenTab,
      fetchGitBase,
      markTabRefreshing,
      patchFileTabRef,
      settleFetch,
      t,
    ]
  )

  // Mark the tab matching `path` as stale so the next activation triggers a
  // reload (clean) or a conflict prompt (dirty). The watcher calls this for
  // dirty non-active tabs when an external change is observed, since silently
  // reloading would discard the user's unsaved edits.
  const markTabsStale = useCallback((rawPath: string) => {
    const tabId = buildFileTabId({
      kind: "file",
      path: normalizeAbsPath(rawPath),
    })
    setFileTabs((prev) => {
      const idx = prev.findIndex((tab) => tab.id === tabId)
      if (idx < 0) return prev
      const tab = prev[idx]
      if (tab.stale === true) return prev
      const updated = [...prev]
      updated[idx] = { ...tab, stale: true }
      return updated
    })
  }, [])

  // Batch variant for the watcher's lazy background pass: N affected
  // background tabs cost ONE setState and zero disk reads. Patches ONLY
  // the `stale` flag — never content or any other field — so it composes
  // safely with concurrent keystroke updaters in the same React batch.
  const markTabsStaleBatch = useCallback((rawPaths: string[]) => {
    if (rawPaths.length === 0) return
    const tabIds = new Set(
      rawPaths.map((rawPath) =>
        buildFileTabId({ kind: "file", path: normalizeAbsPath(rawPath) })
      )
    )
    setFileTabs((prev) => {
      let changed = false
      const next = prev.map((tab) => {
        if (!tabIds.has(tab.id) || tab.kind !== "file") return tab
        if (tab.stale === true) return tab
        changed = true
        return { ...tab, stale: true }
      })
      return changed ? next : prev
    })
  }, [])

  // Write a prefetched FileEditContent into the matching tab. The change-
  // detection watcher uses this after its resolver has already read the
  // latest disk content — without this we would re-read every file twice
  // per workspace event (resolver + reload). Dirty tabs are skipped so
  // unsaved edits are never silently clobbered.
  //
  // Concurrency contract: the in-flight marker is bumped to invalidate any
  // concurrent openFilePreview's pending settle (so an older read cannot
  // overwrite our newer payload) and is then settled IMMEDIATELY after the
  // synchronous content write. The slow, cosmetic git-base refresh runs
  // out-of-band — it does NOT extend the in-flight marker's lifetime —
  // so a stuck git invocation cannot block a subsequent user-initiated
  // reload via the openFilePreview dedup path.
  const applyExternalReload = useCallback(
    async (rawPath: string, fetched: FileEditContent) => {
      const absPath = normalizeAbsPath(rawPath)
      const tabId = buildFileTabId({ kind: "file", path: absPath })
      // Outer existence check — purely to avoid bumping the in-flight gen
      // for a non-existent path (which would pollute openFilePreview's
      // dedup). The dirty guard is NOT outer: fileTabsRef can lag a tick
      // behind a user keystroke whose dirty update is already enqueued
      // but not yet committed. The atomic check lives inside the
      // setFileTabs updater below, where prev reflects every earlier
      // queued updater (including the keystroke).
      const existing = fileTabsRef.current.find((t) => t.id === tabId)
      if (!existing || existing.kind !== "file") return

      const gen = beginFetchGeneration(tabId)
      const fetchedEtag = fetched.etag

      // Atomic write: refuses the apply if the tab became dirty between
      // our outer existence check and the actual commit (e.g. user typed
      // in the same React batch as the watcher's apply call). The refused
      // branch flips stale=true so the aux-panel effect (stale && isDirty
      // → announceConflict) surfaces the divergence immediately instead
      // of waiting for the next save to discover the etag mismatch.
      setFileTabs((prev) =>
        prev.map((tab) => {
          if (tab.id !== tabId || tab.kind !== "file") return tab
          if (tab.isDirty) return { ...tab, stale: true }
          return {
            ...tab,
            content: fetched.content,
            savedContent: fetched.content,
            isDirty: false,
            etag: fetched.etag,
            mtimeMs: fetched.mtime_ms,
            readonly: fetched.readonly,
            lineEnding: fetched.line_ending,
            loading: false,
            stale: false,
            saveState: "idle",
            saveError: null,
            hasLoadedSuccessfully: true,
          }
        })
      )

      // Release the in-flight marker NOW. Two-stage invalidation: the
      // beginFetchGeneration above already poisoned any concurrent
      // openFilePreview fetch (its settleFetch will fail), so clearing
      // here cannot resurrect an in-flight overwrite. The cosmetic git
      // base refresh below is decoupled — slow git must not block user
      // reload dedup. (Each call's settle is mutually exclusive: the
      // last applyExternalReload's gen wins, prior gens are stale.)
      settleFetch(tabId, gen)

      // Fire-and-forget git base refresh, etag-gated.
      //
      // The captured fetchedEtag doubles as a staleness token: if our
      // atomic write above succeeded, the tab now carries fetchedEtag;
      // if it was refused (dirty), or a later applyExternalReload /
      // openFilePreview reload / close+reopen changed the tab, the tab
      // carries a different etag. The final write checks tab.etag ===
      // fetchedEtag inside the updater so a stale fetch can never paint
      // gitter decorations onto a tab whose content has moved on. No
      // separate generation token needed — etag is the natural fingerprint.
      void (async () => {
        try {
          const gitBaseContent = await withTimeout(
            fetchGitBase(absPath),
            15_000,
            t("previewRequestTimedOut")
          )
          setFileTabs((prev) =>
            prev.map((tab) => {
              if (tab.id !== tabId || tab.kind !== "file") return tab
              if (tab.etag !== fetchedEtag) return tab
              return {
                ...tab,
                gitBaseContent: dedupeGitBase(tab.content, gitBaseContent),
              }
            })
          )
        } catch {
          // Timeout or unexpected failure: leave existing gitBaseContent.
        }
      })()
    },
    [beginFetchGeneration, fetchGitBase, settleFetch, t]
  )

  // Mark a clean open tab as load-failed. Used by the change-detection
  // watcher when a readFileForEdit on a changed path fails (most commonly
  // external delete). Dirty tabs are deliberately not touched here — the
  // watcher routes them to markTabsStale so unsaved edits are preserved.
  const rejectFileTab = useCallback(
    (rawPath: string, errorMessage: string) => {
      const tabId = buildFileTabId({
        kind: "file",
        path: normalizeAbsPath(rawPath),
      })
      // Outer existence check only; the dirty guard is atomic inside the
      // updater (see applyExternalReload for the same race shape).
      const existing = fileTabsRef.current.find((t) => t.id === tabId)
      if (!existing || existing.kind !== "file") return

      // Bump generation so any concurrent fetch's settle is invalidated
      // and cannot overwrite the error message we are about to write.
      const gen = beginFetchGeneration(tabId)
      setFileTabs((prev) =>
        prev.map((tab) => {
          if (tab.id !== tabId || tab.kind !== "file") return tab
          // Symmetric with applyExternalReload's dirty refusal: surface
          // the divergence via stale rather than silently no-op. Callers
          // typically also call markTabsStale, so this is usually
          // idempotent; the in-updater write protects direct callers.
          if (tab.isDirty) return { ...tab, stale: true }
          // Preserve last-known-good buffer (text savedContent or image data
          // URL in content) so markErrorRetry / warm fail can restore it.
          const preservedSaved =
            typeof tab.savedContent === "string" && tab.savedContent.length > 0
              ? tab.savedContent
              : tab.content
          return {
            ...tab,
            savedContent: preservedSaved,
            content: t("unableLoadContent", { message: errorMessage }),
            loading: false,
            stale: false,
            saveState: "error",
            saveError: errorMessage,
          }
        })
      )
      // Mirror ref so same-turn openFilePreview retry sees savedContent.
      fileTabsRef.current = fileTabsRef.current.map((tab) => {
        if (tab.id !== tabId || tab.kind !== "file" || tab.isDirty) return tab
        const preservedSaved =
          typeof tab.savedContent === "string" && tab.savedContent.length > 0
            ? tab.savedContent
            : tab.content
        return {
          ...tab,
          savedContent: preservedSaved,
          content: t("unableLoadContent", { message: errorMessage }),
          loading: false,
          stale: false,
          saveState: "error" as const,
          saveError: errorMessage,
        }
      })
      settleFetch(tabId, gen)
    },
    [beginFetchGeneration, settleFetch, t]
  )

  const openFilePreview = useCallback(
    async (
      rawPath: string,
      options?: OpenFileOptions
    ): Promise<OpenFileSettleResult> => {
      const maximizeOnSuccess = options?.maximizeOnSuccess !== false
      const absPath = await resolveOpenAbsolutePath(rawPath, options?.folderId)
      if (!absPath) {
        toast.error(t("unableOpenFile", { name: fileName(rawPath) }))
        return { ok: false, reason: "resolve" }
      }
      const io = splitAbsPath(absPath)
      if (!io) {
        toast.error(t("unableOpenFile", { name: fileName(absPath) }))
        return { ok: false, reason: "resolve" }
      }
      const requestedLine =
        typeof options?.line === "number" && Number.isFinite(options.line)
          ? Math.max(1, Math.floor(options.line))
          : null
      if (requestedLine) {
        fileRevealRequestIdRef.current += 1
        setPendingFileReveal({
          requestId: fileRevealRequestIdRef.current,
          path: absPath,
          line: requestedLine,
        })
      } else {
        setPendingFileReveal(null)
      }
      const tabId = buildFileTabId({ kind: "file", path: absPath })
      const displayName = fileName(absPath)
      const image = isImageFile(absPath)
      const office = !image && isOfficePreviewable(absPath)
      const seed = loadingTab(
        tabId,
        null,
        "file",
        displayName,
        absPath,
        absPath,
        image ? "image" : office ? "office" : languageFromPath(absPath)
      )

      const decision = decideLoad(seed, options?.reload ?? false, {
        maximizeOnSuccess,
      })
      if (decision.kind === "skip") {
        const stillOpen = fileTabsRef.current.some((t) => t.id === tabId)
        if (!stillOpen) return { ok: false, reason: "closed" }
        // Concurrent open while first request is still in flight: await the
        // shared Deferred so callers observe the real settle outcome.
        const pending = openSettleDeferredRef.current.get(tabId)
        if (pending) return pending.promise
        return { ok: true, tabId }
      }
      const { gen } = decision
      // Share settle outcome with concurrent openFilePreview callers and with
      // close mid-flight (finishOpenSettleClosed resolves this Deferred).
      // Primary and concurrent callers all await the same gen-scoped promise,
      // returned immediately so a user close can settle as "closed" without
      // waiting for the network.
      const settlePromise = startOpenSettle(tabId, gen)

      const settle = (result: OpenFileSettleResult): void => {
        finishOpenSettle(tabId, gen, result)
      }

      void (async () => {
        try {
          // Office files (.docx/.xlsx/.pptx) are binary OpenXML — never read as
          // text. The OfficePreview component renders them via the OfficeCLI
          // backend on its own, so just settle the tab as a ready preview shell.
          if (office) {
            if (!settleFetch(tabId, gen)) {
              settle({ ok: false, reason: "stale" })
              return
            }
            const officePatch = {
              content: "",
              readonly: true as const,
              loading: false,
              saveState: "idle" as const,
              saveError: null,
              stale: false,
              hasLoadedSuccessfully: true as const,
            }
            setFileTabs((prev) =>
              prev.map((tab) =>
                tab.id === tabId ? { ...tab, ...officePatch } : tab
              )
            )
            patchFileTabRef(tabId, officePatch)
            maybeMaximizeAfterSuccess(tabId)
            settle({ ok: true, tabId })
            return
          }

          if (image) {
            const ext = absPath.split(".").pop()?.toLowerCase() ?? ""
            const mime = IMAGE_MIME[ext] ?? "image/png"
            const b64 = await withTimeout(
              readFileBase64(absPath),
              15_000,
              t("previewRequestTimedOut")
            )
            if (!settleFetch(tabId, gen)) {
              settle({ ok: false, reason: "stale" })
              return
            }
            const imageContent = `data:${mime};base64,${b64}`
            const imagePatch = {
              content: imageContent,
              savedContent: imageContent,
              readonly: true as const,
              loading: false,
              saveState: "idle" as const,
              saveError: null,
              stale: false,
              hasLoadedSuccessfully: true as const,
            }
            setFileTabs((prev) =>
              prev.map((tab) =>
                tab.id === tabId ? { ...tab, ...imagePatch } : tab
              )
            )
            patchFileTabRef(tabId, imagePatch)
            maybeMaximizeAfterSuccess(tabId)
            settle({ ok: true, tabId })
            return
          }

          const [result, gitBaseContent] = await withTimeout(
            Promise.all([
              readFileForEdit(io.rootPath, io.ioPath),
              fetchGitBase(absPath),
            ]),
            15_000,
            t("previewRequestTimedOut")
          )
          if (!settleFetch(tabId, gen)) {
            settle({ ok: false, reason: "stale" })
            return
          }
          const textPatch = {
            content: result.content,
            gitBaseContent: dedupeGitBase(result.content, gitBaseContent),
            savedContent: result.content,
            isDirty: false,
            etag: result.etag,
            mtimeMs: result.mtime_ms,
            readonly: result.readonly,
            lineEnding: result.line_ending,
            saveState: "idle" as const,
            saveError: null,
            loading: false,
            stale: false,
            hasLoadedSuccessfully: true as const,
          }
          setFileTabs((prev) =>
            prev.map((tab) =>
              tab.id === tabId ? { ...tab, ...textPatch } : tab
            )
          )
          // Keep ref current for immediate warm-fail checks / concurrent paths.
          patchFileTabRef(tabId, textPatch)
          maybeMaximizeAfterSuccess(tabId)
          settle({ ok: true, tabId })
        } catch (error) {
          if (!settleFetch(tabId, gen)) {
            settle({ ok: false, reason: "stale" })
            return
          }
          if (requestedLine) {
            setPendingFileReveal((prev) =>
              prev && prev.path === absPath ? null : prev
            )
          }
          failOpenTab(tabId, displayName, error)
          settle({ ok: false, reason: "load" })
        }
      })()

      return settlePromise
    },
    [
      decideLoad,
      failOpenTab,
      fetchGitBase,
      finishOpenSettle,
      maybeMaximizeAfterSuccess,
      patchFileTabRef,
      resolveOpenAbsolutePath,
      settleFetch,
      startOpenSettle,
      t,
    ]
  )

  // Auto-surface office files (.docx/.xlsx/.pptx) the agent produces. This used
  // to live in the file-tree aux panel, but that panel is closed by default and
  // unmounts its subscription with it — so the preview never opened unless the
  // user happened to have the sidebar open. The preview itself lands in the
  // files pane (openFilePreview → seedLoadingTab activates it), which is owned
  // here and always mounted, so the trigger belongs here too.
  //
  // We retain the workspace watch stream from this always-mounted provider so
  // change envelopes keep flowing regardless of the aux panel. The store is a
  // per-path refcounted singleton, so this shares the same backend stream the
  // aux panel tabs use. Gated on the preference: with auto-preview off we hold
  // no extra ref, leaving today's aux-panel-scoped lifecycle untouched.
  const officeAutoPreview = useOfficeAutoPreview()
  // Paths-only subscription: this exists for changed_paths envelopes and
  // must never be the reason a root runs tree/git scans.
  const officeWatchStore = useWorkspaceStateStore(
    officeAutoPreview ? (folderPath ?? null) : null,
    "paths"
  )
  const subscribeOfficeEnvelopes = officeWatchStore.subscribeEnvelopes
  const activeFolderIdForOffice = activeFolder?.id
  useEffect(() => {
    if (!folderPath || activeFolderIdForOffice == null || !officeAutoPreview) {
      return
    }
    // Leading-edge with dedup: an agent building a doc fires a burst of writes,
    // so we open on first sighting and remember it in `autoOpened` (which also
    // keeps a tab the user has since closed from popping back open).
    const autoOpened = new Set<string>()
    const streamRoot = folderPath
    const unsubscribe = subscribeOfficeEnvelopes(({ changed_paths }) => {
      if (!changed_paths || changed_paths.length === 0) return
      // Tab identity is the absolute path, so joining the stream root onto
      // the changed relative path compares exactly — an identically-named
      // doc in another folder has a different absolute path and never
      // suppresses this preview.
      const openPaths = new Set(
        fileTabsRef.current
          .filter((tab) => tab.kind === "file" && tab.path)
          .map((tab) => tab.path as string)
      )
      for (const changed of changed_paths) {
        if (!isOfficePreviewable(changed)) continue
        const abs = joinRootRel(streamRoot, changed)
        if (autoOpened.has(abs) || openPaths.has(abs)) continue
        autoOpened.add(abs)
        void openFilePreview(abs, { maximizeOnSuccess: false })
      }
    })
    return unsubscribe
  }, [
    folderPath,
    activeFolderIdForOffice,
    officeAutoPreview,
    subscribeOfficeEnvelopes,
    openFilePreview,
  ])

  const openWorkingTreeDiff = useCallback(
    async (
      rawPath?: string,
      options?: {
        mode?: "auto" | "unified" | "overview"
        folderId?: number
      }
    ) => {
      const target = resolveTargetFolder(options?.folderId)
      if (!target) return
      const folderPath = target.path

      if (!rawPath) {
        const tabId = buildFileTabId({
          kind: "diff-working-all",
          folderId: target.id,
        })
        const title = t("diffTitleWorkspace")
        const description = t("diffDescriptionWorkingTree")
        const seed = loadingTab(
          tabId,
          target.id,
          "diff",
          title,
          description,
          null,
          "diff"
        )
        const decision = beginDiffLoad(seed)
        if (decision.skip) return
        const { gen } = decision
        try {
          const result = await withTimeout(
            gitDiff(folderPath),
            20_000,
            t("diffRequestTimedOut")
          )
          if (settleFetch(tabId, gen))
            resolveTab(tabId, result || t("noChanges"), false)
        } catch (error) {
          if (settleFetch(tabId, gen)) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, error)
          }
        }
        return
      }

      const path = normalizePath(rawPath)
      const mode = options?.mode ?? "auto"

      if (mode === "overview") {
        const isRoot = path === "."
        const displayPath = isRoot ? folderPath : path
        const tabId = buildFileTabId({
          kind: "diff-working-overview",
          folderId: target.id,
          path,
        })
        const title = t("diffTitleFile", {
          name: fileName(displayPath ?? path),
        })
        const description = displayPath ?? path
        const seed = loadingTab(
          tabId,
          target.id,
          "diff",
          title,
          description,
          path,
          "diff"
        )
        const decision = beginDiffLoad(seed)
        if (decision.skip) return
        const { gen } = decision
        try {
          const result = await withTimeout(
            gitDiff(folderPath, path),
            20_000,
            t("diffRequestTimedOut")
          )
          if (settleFetch(tabId, gen))
            resolveTab(tabId, result || t("noChanges"), false)
        } catch (error) {
          if (settleFetch(tabId, gen)) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, error)
          }
        }
        return
      }

      if (mode === "unified") {
        const tabId = buildFileTabId({
          kind: "diff-working-unified",
          folderId: target.id,
          path,
        })
        const title = t("diffTitleFile", { name: fileName(path) })
        const description = path
        const seed = loadingTab(
          tabId,
          target.id,
          "diff",
          title,
          description,
          path,
          "diff"
        )
        const decision = beginDiffLoad(seed)
        if (decision.skip) return
        const { gen } = decision
        try {
          const result = await withTimeout(
            gitDiff(folderPath, path),
            20_000,
            t("diffRequestTimedOut")
          )
          if (settleFetch(tabId, gen))
            resolveTab(tabId, result || t("noChanges"), false)
        } catch (error) {
          if (settleFetch(tabId, gen)) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, error)
          }
        }
        return
      }

      const tabId = buildFileTabId({
        kind: "diff-working",
        folderId: target.id,
        path,
      })
      const title = t("diffTitleFile", { name: fileName(path) })
      const description = path
      const lang = languageFromPath(path)

      const seed = loadingTab(
        tabId,
        target.id,
        "rich-diff",
        title,
        description,
        path,
        lang
      )
      const decision = beginDiffLoad(seed)
      if (decision.skip) return
      const { gen } = decision
      try {
        const [originalSide, modifiedSide] = await withTimeout(
          Promise.all([
            softSideRead(gitShowFile(folderPath, path), ""),
            softSideRead(readFilePreview(folderPath, path), {
              content: "",
              path: "",
            }),
          ]),
          20_000,
          t("diffRequestTimedOut")
        )
        if (!settleFetch(tabId, gen)) return
        if (!originalSide.ok && !modifiedSide.ok) {
          const name =
            fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
          failOpenTab(tabId, name, originalSide.error)
          return
        }
        resolveRichDiffTab(
          tabId,
          originalSide.value,
          modifiedSide.value.content
        )
      } catch (error) {
        if (settleFetch(tabId, gen)) {
          const name =
            fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
          failOpenTab(tabId, name, error)
        }
      }
    },
    [
      beginDiffLoad,
      failOpenTab,
      resolveTab,
      resolveRichDiffTab,
      resolveTargetFolder,
      settleFetch,
      t,
    ]
  )

  const openBranchDiff = useCallback(
    async (
      branch: string,
      rawPath?: string,
      options?: { mode?: "default" | "overview"; folderId?: number }
    ) => {
      const target = resolveTargetFolder(options?.folderId)
      if (!target) return
      const folderPath = target.path
      const targetBranch = branch.trim()
      if (!targetBranch) return

      const path = rawPath ? normalizePath(rawPath) : null
      const mode = options?.mode ?? "default"
      const tabId =
        mode === "overview"
          ? buildFileTabId({
              kind: "diff-branch-overview",
              folderId: target.id,
              branch: targetBranch,
              path,
            })
          : buildFileTabId({
              kind: "diff-branch",
              folderId: target.id,
              branch: targetBranch,
              path,
            })
      const title = path
        ? t("compareTitleFile", { name: fileName(path) })
        : t("compareTitleBranch", { branch: targetBranch })
      const description = path
        ? t("compareDescriptionPath", { path, branch: targetBranch })
        : t("compareDescriptionBranch", { branch: targetBranch })

      if (mode !== "overview" && path) {
        const lang = languageFromPath(path)
        const seed = loadingTab(
          tabId,
          target.id,
          "rich-diff",
          title,
          description,
          path,
          lang
        )
        const decision = beginDiffLoad(seed)
        if (decision.skip) return
        const { gen } = decision
        try {
          const [originalSide, modifiedSide] = await withTimeout(
            Promise.all([
              softSideRead(gitShowFile(folderPath, path, targetBranch), ""),
              softSideRead(readFilePreview(folderPath, path), {
                content: "",
                path: "",
              }),
            ]),
            20_000,
            t("branchCompareRequestTimedOut")
          )
          if (!settleFetch(tabId, gen)) return
          if (!originalSide.ok && !modifiedSide.ok) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, originalSide.error)
            return
          }
          resolveRichDiffTab(
            tabId,
            originalSide.value,
            modifiedSide.value.content
          )
        } catch (error) {
          if (settleFetch(tabId, gen)) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, error)
          }
        }
        return
      }

      const seed = loadingTab(
        tabId,
        target.id,
        "diff",
        title,
        description,
        path,
        "diff"
      )
      const decision = beginDiffLoad(seed)
      if (decision.skip) return
      const { gen } = decision
      try {
        const result = await withTimeout(
          gitDiffWithBranch(folderPath, targetBranch, path ?? undefined),
          20_000,
          t("branchCompareRequestTimedOut")
        )
        if (settleFetch(tabId, gen))
          resolveTab(tabId, result || t("noChanges"), false)
      } catch (error) {
        if (settleFetch(tabId, gen)) {
          const name =
            fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
          failOpenTab(tabId, name, error)
        }
      }
    },
    [
      beginDiffLoad,
      failOpenTab,
      resolveRichDiffTab,
      resolveTab,
      resolveTargetFolder,
      settleFetch,
      t,
    ]
  )

  const openCommitDiff = useCallback(
    async (
      commit: string,
      rawPath?: string,
      message?: string,
      options?: { folderId?: number }
    ) => {
      const target = resolveTargetFolder(options?.folderId)
      if (!target) return
      const folderPath = target.path
      const path = rawPath ? normalizePath(rawPath) : null
      const tabId = buildFileTabId({
        kind: "diff-commit",
        folderId: target.id,
        commit,
        path,
      })
      const title = path
        ? t("diffTitleCommitFile", {
            name: fileName(path),
            hash: commit.slice(0, 7),
          })
        : t("diffTitleCommit", { hash: commit.slice(0, 7) })
      const description = path
        ? t("diffDescriptionCommitPath", { path, commit })
        : message || t("diffDescriptionCommit", { commit })

      if (path) {
        const lang = languageFromPath(path)
        const seed = loadingTab(
          tabId,
          target.id,
          "rich-diff",
          title,
          description,
          path,
          lang
        )
        const decision = beginDiffLoad(seed)
        if (decision.skip) return
        const { gen } = decision
        try {
          const [originalSide, modifiedSide] = await withTimeout(
            Promise.all([
              softSideRead(gitShowFile(folderPath, path, `${commit}~1`), ""),
              softSideRead(gitShowFile(folderPath, path, commit), ""),
            ]),
            20_000,
            t("commitDiffRequestTimedOut")
          )
          if (!settleFetch(tabId, gen)) return
          if (!originalSide.ok && !modifiedSide.ok) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, originalSide.error)
            return
          }
          resolveRichDiffTab(tabId, originalSide.value, modifiedSide.value)
        } catch (error) {
          if (settleFetch(tabId, gen)) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, error)
          }
        }
      } else {
        const seed = loadingTab(
          tabId,
          target.id,
          "diff",
          title,
          description,
          path,
          "diff"
        )
        const decision = beginDiffLoad(seed)
        if (decision.skip) return
        const { gen } = decision
        try {
          const result = await withTimeout(
            gitShowDiff(folderPath, commit, undefined),
            20_000,
            t("commitDiffRequestTimedOut")
          )
          if (settleFetch(tabId, gen))
            resolveTab(tabId, result || t("noDiffOutput"), false)
        } catch (error) {
          if (settleFetch(tabId, gen)) {
            const name =
              fileTabsRef.current.find((t) => t.id === tabId)?.title ?? "diff"
            failOpenTab(tabId, name, error)
          }
        }
      }
    },
    [
      beginDiffLoad,
      failOpenTab,
      resolveTab,
      resolveRichDiffTab,
      resolveTargetFolder,
      settleFetch,
      t,
    ]
  )

  const openSessionFileDiff = useCallback(
    (
      filePath: string,
      diffContent: string,
      groupLabel: string,
      options?: { folderId?: number }
    ) => {
      const target = resolveTargetFolder(options?.folderId)
      if (!target) return
      const path = normalizePath(filePath)
      const tabId = buildFileTabId({
        kind: "diff-session",
        folderId: target.id,
        groupLabel,
        path,
      })
      const title = t("diffTitleFile", { name: fileName(path) })
      const description = `${path} · ${groupLabel}`

      const tab: FileWorkspaceTab = {
        id: tabId,
        kind: "diff",
        folderId: target.id,
        title,
        description,
        path: null,
        language: "diff",
        content: diffContent,
        loading: false,
        hasLoadedSuccessfully: true,
      }

      replaceTabContent(tab)
    },
    [replaceTabContent, resolveTargetFolder, t]
  )

  const openExternalConflictDiff = useCallback(
    (filePath: string, diskContent: string, unsavedContent: string) => {
      const path = normalizeAbsPath(filePath)
      const tabId = buildFileTabId({ kind: "diff-external-conflict", path })
      const title = t("diffTitleConflictFile", { name: fileName(path) })
      const description = t("diffDescriptionConflict", { path })
      const language = languageFromPath(path)

      const tab: FileWorkspaceTab = {
        id: tabId,
        kind: "rich-diff",
        folderId: null,
        title,
        description,
        path,
        language,
        content: "",
        loading: false,
        originalContent: diskContent,
        modifiedContent: unsavedContent,
        hasLoadedSuccessfully: true,
      }

      replaceTabContent(tab)
    },
    [replaceTabContent, t]
  )

  // Queue a divergence for the conflict dialog. Deduped two ways: a
  // signature already announced for this path is dropped entirely (no
  // flicker on repeated watcher events); a NEW signature for an
  // already-queued path replaces that entry in place (disk moved again
  // while the prompt waited) instead of queueing a second prompt.
  //
  // `force` bypasses the shown-signature dedup: an explicit USER action
  // (a refused save) must re-surface the dialog even when the same
  // divergence was announced before and dismissed/compared — otherwise
  // the save silently no-ops with no recovery UI. Watcher-driven
  // announcements never force.
  const enqueueExternalConflict = useCallback(
    (conflict: WorkspaceExternalConflict, options?: { force?: boolean }) => {
      const key = conflictKey(conflict.path)
      const shown = conflictSignatureByKeyRef.current.get(key)
      if (!options?.force && shown === conflict.signature) return
      conflictSignatureByKeyRef.current.set(key, conflict.signature)
      setExternalConflictQueue((prev) => {
        const idx = prev.findIndex((queued) => conflictKey(queued.path) === key)
        if (idx >= 0) {
          const next = [...prev]
          next[idx] = conflict
          return next
        }
        return [...prev, conflict]
      })
    },
    []
  )

  // Dequeue the current head. `clearSignature` re-arms the dedup so the
  // same divergence prompts again (used by "reload", which resolves it);
  // compare/save-copy/dismiss keep the signature so the still-diverged
  // file does not immediately re-prompt.
  const dequeueExternalConflict = useCallback(
    (options?: { clearSignature?: boolean }) => {
      const head = externalConflictQueueRef.current[0]
      if (!head) return null
      if (options?.clearSignature) {
        conflictSignatureByKeyRef.current.delete(conflictKey(head.path))
      }
      setExternalConflictQueue((prev) =>
        prev[0] === head ? prev.slice(1) : prev.filter((c) => c !== head)
      )
      return head
    },
    []
  )

  const compareExternalConflict = useCallback(() => {
    const head = dequeueExternalConflict()
    if (!head) return
    // Prefer the LIVE buffer content over the snapshot captured when the
    // conflict was detected — the user may have typed since.
    const tabId = buildFileTabId({ kind: "file", path: head.path })
    const latestTab = fileTabsRef.current.find((t) => t.id === tabId)
    const unsavedContent =
      latestTab && latestTab.kind === "file" && !latestTab.loading
        ? latestTab.content
        : head.unsavedContent
    openExternalConflictDiff(head.path, head.diskContent, unsavedContent)
  }, [dequeueExternalConflict, openExternalConflictDiff])

  const reloadExternalConflict = useCallback(() => {
    const head = dequeueExternalConflict({ clearSignature: true })
    if (!head) return
    void openFilePreview(head.path, { reload: true })
  }, [dequeueExternalConflict, openFilePreview])

  const saveExternalConflictCopy = useCallback(async (): Promise<string> => {
    const head = externalConflictQueueRef.current[0]
    if (!head) throw new Error("no external conflict to resolve")
    const io = splitAbsPath(head.path)
    if (!io) throw new Error("invalid file path")
    const tabId = buildFileTabId({ kind: "file", path: head.path })
    const latestTab = fileTabsRef.current.find(
      (candidate) => candidate.id === tabId
    )
    const unsavedContent =
      latestTab && latestTab.kind === "file" && !latestTab.loading
        ? latestTab.content
        : head.unsavedContent
    // Throws on failure BEFORE dequeueing — the conflict stays queued so
    // the user can retry or pick another resolution.
    const result = await saveFileCopy(io.rootPath, io.ioPath, unsavedContent)
    dequeueExternalConflict()
    return result.path
  }, [dequeueExternalConflict])

  const dismissExternalConflict = useCallback(() => {
    dequeueExternalConflict()
  }, [dequeueExternalConflict])

  const updateActiveFileContent = useCallback((content: string) => {
    const activeId = activeFileTabIdRef.current
    if (!activeId) return

    setFileTabs((prev) =>
      prev.map((tab) => {
        if (tab.id !== activeId || tab.kind !== "file") return tab
        if (tab.loading || tab.readonly) return tab
        if (tab.content === content) return tab

        const savedContent = tab.savedContent ?? ""
        return {
          ...tab,
          content,
          isDirty: content !== savedContent,
          saveState: tab.saveState === "saving" ? "saving" : "idle",
          saveError: null,
        }
      })
    )
  }, [])

  const saveFileTab = useCallback(
    async (tabId: string, options?: { force?: boolean }): Promise<boolean> => {
      const tab = fileTabsRef.current.find(
        (candidate) => candidate.id === tabId
      )
      if (!tab || tab.kind !== "file") return false
      if (tab.loading || tab.readonly) return false
      if (!tab.path) return false
      if (!tab.isDirty) return true

      const io = splitAbsPath(tab.path)
      if (!io) return false

      // Divergence guard (covers EVERY write path — manual save, 5s
      // autosave, blur/switch/close saves — because they all funnel here):
      // `stale` means the watcher observed an external change this buffer
      // has not reconciled against; a file OUTSIDE every registered folder
      // has no watcher at all, so its saves ALWAYS pre-verify. Never write
      // blindly: an equal etag proves the flag was spurious (our own save
      // echo) and the save proceeds; a different etag is a real divergence
      // — surface the conflict prompt and refuse the save. `force: true`
      // (the conflict dialog's own overwrite path) bypasses.
      const unwatched = !findOwningFolder(
        tab.path,
        useAppWorkspaceStore.getState().allFolders
      )
      if ((tab.stale || unwatched) && !options?.force) {
        try {
          const latest = await readFileForEdit(io.rootPath, io.ioPath)
          if ((latest.etag ?? null) !== (tab.etag ?? null)) {
            // Forced: the user just asked to save and the save is being
            // refused — the dialog must re-appear even if this divergence
            // was announced before and dismissed/compared.
            enqueueExternalConflict(
              {
                path: tab.path,
                diskContent: latest.content,
                unsavedContent: tab.content,
                signature: latest.etag ?? "",
              },
              { force: true }
            )
            return false
          }
        } catch (error) {
          // Disk unreadable (deleted/locked). Keep the dirty buffer and
          // fail the save visibly; the user decides via the error state.
          const message = toErrorMessage(error)
          setFileTabs((prev) =>
            prev.map((candidate) =>
              candidate.id === tabId
                ? { ...candidate, saveState: "error", saveError: message }
                : candidate
            )
          )
          return false
        }
      }

      const contentAtSaveStart = tab.content
      const expectedEtag = options?.force ? null : (tab.etag ?? null)

      setFileTabs((prev) =>
        prev.map((candidate) =>
          candidate.id === tabId
            ? {
                ...candidate,
                saveState: "saving",
                saveError: null,
              }
            : candidate
        )
      )

      try {
        const result = await withTimeout(
          saveFileContent(
            io.rootPath,
            io.ioPath,
            contentAtSaveStart,
            expectedEtag
          ),
          20_000,
          t("saveRequestTimedOut")
        )

        // One-shot echo record: the watcher will see this write as a
        // change event; suppress that single event for this path.
        recordSelfWriteEcho(tab.path, result.etag)

        setFileTabs((prev) =>
          prev.map((candidate) => {
            if (candidate.id !== tabId || candidate.kind !== "file") {
              return candidate
            }

            const savedContent = contentAtSaveStart
            return {
              ...candidate,
              etag: result.etag,
              mtimeMs: result.mtime_ms,
              readonly: result.readonly,
              lineEnding: result.line_ending,
              savedContent,
              isDirty: candidate.content !== savedContent,
              // An optimistic-locked save succeeding means the buffer IS
              // the disk state now — any prior stale flag is resolved.
              stale: false,
              saveState: "idle",
              saveError: null,
            }
          })
        )

        return true
      } catch (error) {
        const message = toErrorMessage(error)
        setFileTabs((prev) =>
          prev.map((candidate) =>
            candidate.id === tabId
              ? {
                  ...candidate,
                  saveState: "error",
                  saveError: message,
                }
              : candidate
          )
        )
        return false
      }
    },
    [enqueueExternalConflict, recordSelfWriteEcho, t]
  )

  const saveActiveFile = useCallback(
    async (options?: { force?: boolean }) => {
      const activeId = activeFileTabIdRef.current
      if (!activeId) return false
      return saveFileTab(activeId, options)
    },
    [saveFileTab]
  )

  const reloadFileTab = useCallback(
    async (tabId: string) => {
      const tab = fileTabsRef.current.find(
        (candidate) => candidate.id === tabId
      )
      if (!tab || tab.kind !== "file" || !tab.path) return
      const tabPath = tab.path
      const io = splitAbsPath(tabPath)
      if (!io) return

      setFileTabs((prev) =>
        prev.map((candidate) =>
          candidate.id === tabId
            ? {
                ...candidate,
                loading: true,
                saveError: null,
                saveState: "idle",
              }
            : candidate
        )
      )

      try {
        const [result, gitBaseContent] = await withTimeout(
          Promise.all([
            readFileForEdit(io.rootPath, io.ioPath),
            fetchGitBase(tabPath),
          ]),
          15_000,
          t("reloadRequestTimedOut")
        )
        setFileTabs((prev) =>
          prev.map((candidate) =>
            candidate.id === tabId
              ? {
                  ...candidate,
                  content: result.content,
                  gitBaseContent: dedupeGitBase(result.content, gitBaseContent),
                  savedContent: result.content,
                  isDirty: false,
                  etag: result.etag,
                  mtimeMs: result.mtime_ms,
                  readonly: result.readonly,
                  lineEnding: result.line_ending,
                  saveState: "idle",
                  saveError: null,
                  loading: false,
                  // A successful reload IS the reconciliation a stale flag
                  // asks for — clearing it here keeps the activation pass
                  // from immediately re-reloading the same tab.
                  stale: false,
                }
              : candidate
          )
        )
      } catch (error) {
        const message = toErrorMessage(error)
        setFileTabs((prev) =>
          prev.map((candidate) =>
            candidate.id === tabId
              ? {
                  ...candidate,
                  loading: false,
                  saveState: "error",
                  saveError: message,
                }
              : candidate
          )
        )
      }
    },
    [fetchGitBase, t]
  )

  const reloadActiveFile = useCallback(async () => {
    const activeId = activeFileTabIdRef.current
    if (!activeId) return
    await reloadFileTab(activeId)
  }, [reloadFileTab])

  const switchFileTab = useCallback(
    (tabId: string) => {
      const activeId = activeFileTabIdRef.current
      if (activeId && activeId !== tabId) {
        void saveFileTab(activeId)
      }
      setActiveFileTabId(tabId)
      activeFileTabIdRef.current = tabId
      activateFilePane()
    },
    [activateFilePane, saveFileTab]
  )

  const closeFileTab = useCallback(
    (tabId: string) => {
      setFileTabs((prev) => {
        const idx = prev.findIndex((tab) => tab.id === tabId)
        if (idx < 0) return prev

        const tab = prev[idx]
        if (isDirtyFileTab(tab)) {
          const confirmed = window.confirm(
            t("confirmCloseDirtyTab", { title: tab.title })
          )
          if (!confirmed) return prev
        }

        const next = prev.filter((candidate) => candidate.id !== tabId)

        setActiveFileTabId((current) => {
          if (current !== tabId) return current
          if (next.length === 0) {
            activateConversationPane()
            activeFileTabIdRef.current = null
            return null
          }
          const nextIdx = Math.min(idx, next.length - 1)
          const nextId = next[nextIdx].id
          activeFileTabIdRef.current = nextId
          return nextId
        })

        setPreviewFileTabIds((prev) => {
          if (!prev.has(tabId)) return prev
          const updated = new Set(prev)
          updated.delete(tabId)
          return updated
        })

        // Drop any in-flight marker and resolve the matching gen Deferred so
        // reopening this path starts an independent open settle.
        finishOpenSettleClosed(tabId)
        pendingMaximizeOnSuccessRef.current.delete(tabId)

        return next
      })
    },
    [activateConversationPane, finishOpenSettleClosed, t]
  )

  const closeOtherFileTabs = useCallback(
    (tabId: string) => {
      setFileTabs((prev) => {
        const remaining = prev.filter((tab) => tab.id === tabId)
        if (remaining.length === 0) return prev

        const closingTabs = prev.filter((tab) => tab.id !== tabId)
        if (closingTabs.some(isDirtyFileTab)) {
          const confirmed = window.confirm(t("confirmCloseOtherDirtyTabs"))
          if (!confirmed) return prev
        }

        for (const closing of closingTabs) {
          finishOpenSettleClosed(closing.id)
          pendingMaximizeOnSuccessRef.current.delete(closing.id)
        }

        setActiveFileTabId(tabId)
        activeFileTabIdRef.current = tabId
        activateFilePane()
        return remaining
      })
    },
    [activateFilePane, finishOpenSettleClosed, t]
  )

  const closeAllFileTabs = useCallback(() => {
    setFileTabs((prev) => {
      if (prev.some(isDirtyFileTab)) {
        const confirmed = window.confirm(t("confirmCloseAllDirtyTabs"))
        if (!confirmed) return prev
      }

      for (const tab of prev) {
        finishOpenSettleClosed(tab.id)
      }
      inFlightLoadsRef.current.clear()
      openSettleDeferredRef.current.clear()
      pendingMaximizeOnSuccessRef.current.clear()
      setActiveFileTabId(null)
      activeFileTabIdRef.current = null
      setPreviewFileTabIds(new Set())
      activateConversationPane()
      return []
    })
  }, [activateConversationPane, finishOpenSettleClosed, t])

  const reorderFileTabs = useCallback((tabs: FileWorkspaceTab[]) => {
    setFileTabs(tabs)
  }, [])

  const activeFileTab = useMemo(
    () => fileTabs.find((tab) => tab.id === activeFileTabId) ?? null,
    [fileTabs, activeFileTabId]
  )

  const activeFilePath = activeFileTab?.path ?? null

  useEffect(() => {
    if (!activeFileTabId) return
    const recency = tabRecencyRef.current
    const existingIdx = recency.indexOf(activeFileTabId)
    if (existingIdx >= 0) recency.splice(existingIdx, 1)
    recency.unshift(activeFileTabId)
    // Bounded bookkeeping; anything beyond this is "long unused" anyway.
    if (recency.length > 512) recency.length = 512
  }, [activeFileTabId])

  // Memory guardrail: once hidden clean tabs retain more text than the
  // budget, drop the least-recently-active buffers (content + git base;
  // metadata/etag survive) and flag them stale — activation refetches
  // through the existing stale machinery. Dirty/loading/saving tabs are
  // never touched. Transient translation tabs are pathless (no disk
  // refetch) and must keep their content until the user closes them.
  // Converges in one pass: unloaded tabs hold no content, so they stop
  // being candidates.
  useEffect(() => {
    const candidates = fileTabs
      .filter(
        (tab) =>
          tab.kind === "file" &&
          tab.id !== activeFileTabId &&
          !tab.isDirty &&
          !tab.loading &&
          tab.saveState !== "saving" &&
          tab.content.length > 0 &&
          tab.transient?.type !== "translation"
      )
      .map((tab) => ({
        id: tab.id,
        charCount:
          tab.content.length +
          (tab.gitBaseContent && tab.gitBaseContent !== tab.content
            ? tab.gitBaseContent.length
            : 0),
      }))
    if (candidates.length === 0) return
    const recencyRank = new Map(
      tabRecencyRef.current.map((id, index) => [id, index])
    )
    const toUnload = selectTabsToUnload(
      candidates,
      recencyRank,
      HIDDEN_TAB_CONTENT_BUDGET_CHARS
    )
    if (toUnload.size === 0) return

    setFileTabs((prev) =>
      prev.map((tab) => {
        if (!toUnload.has(tab.id) || tab.kind !== "file") return tab
        // Atomic re-check: a keystroke/save enqueued in the same batch
        // must win over the eviction.
        if (tab.isDirty || tab.loading || tab.saveState === "saving") {
          return tab
        }
        return {
          ...tab,
          content: "",
          savedContent: "",
          gitBaseContent: undefined,
          stale: true,
        }
      })
    )
  }, [fileTabs, activeFileTabId])

  // Once the active tab is clean and settled (e.g. the user reloaded, or a
  // successful save resolved the divergence), any conflict recorded for
  // its path is moot — drop it and re-arm the signature dedup.
  useEffect(() => {
    const tab = activeFileTab
    if (!tab || tab.kind !== "file" || !tab.path) return
    if (tab.loading || tab.isDirty) return
    const key = conflictKey(tab.path)
    conflictSignatureByKeyRef.current.delete(key)
    /* eslint-disable react-hooks/set-state-in-effect */
    setExternalConflictQueue((prev) =>
      prev.some((conflict) => conflictKey(conflict.path) === key)
        ? prev.filter((conflict) => conflictKey(conflict.path) !== key)
        : prev
    )
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [activeFileTab])

  // The watcher: per-root FS stream subscriptions derived from the open
  // file tabs' absolute paths (owning registered folders only), lazy
  // background staleness, eager active-tab reconciliation, and
  // stale-on-activation. Owned here (always mounted) so detection works
  // with the aux panel closed and across all folders. Tabs outside every
  // registered folder are not live-watched; they get activation-time
  // freshness checks instead.
  useOpenFileTabsWatch({
    fileTabs,
    fileTabsRef,
    activeFileTabIdRef,
    activeFileTab,
    allFolders,
    openFilePreview,
    reloadOpenFileBackground,
    applyExternalReload,
    markTabsStale,
    markTabsStaleBatch,
    rejectFileTab,
    enqueueExternalConflict,
    consumeSelfWriteEcho,
  })

  const toggleFileTabPreview = useCallback((tabId: string) => {
    setPreviewFileTabIds((prev) => {
      const next = new Set(prev)
      if (next.has(tabId)) {
        next.delete(tabId)
      } else {
        next.add(tabId)
      }
      return next
    })
  }, [])

  const beginTranslateRequest = useCallback((sourceTabId: string) => {
    const next = (translateRequestGenRef.current.get(sourceTabId) ?? 0) + 1
    translateRequestGenRef.current.set(sourceTabId, next)
    return next
  }, [])

  const openTranslationResultTab = useCallback(
    (input: {
      sourceTabId: string
      requestGen: number
      content: string
      locale: string
      format: DocumentTranslateFormat
      sourcePath: string | null
      sourceContentHash: string
      sourceTitle: string
    }): string | null => {
      if (!providerAliveRef.current) return null
      const current = translateRequestGenRef.current.get(input.sourceTabId)
      if (current !== input.requestGen) return null

      const sourceName =
        input.sourceTitle ||
        (input.sourcePath
          ? input.sourcePath.replace(/\\/g, "/").split("/").pop() || "document"
          : "document")
      const suggestedName = buildSuggestedTranslationName(
        sourceName,
        input.locale
      )
      const tabId = buildTranslationTabId(
        input.sourceTabId,
        input.locale,
        input.requestGen
      )
      const language = input.format === "plainText" ? "plaintext" : "markdown"
      const title = tFiles("translationTabTitle", {
        name: sourceName,
        locale: input.locale,
      })

      const transient: TranslationTransientMeta = {
        type: "translation",
        sourceTabId: input.sourceTabId,
        sourcePath: input.sourcePath,
        sourceContentHash: input.sourceContentHash,
        locale: input.locale,
        format: input.format,
        suggestedName,
      }

      const tab: FileWorkspaceTab = {
        id: tabId,
        kind: "file",
        folderId: null,
        title,
        description: suggestedName,
        path: null,
        language,
        content: input.content,
        loading: false,
        savedContent: input.content,
        isDirty: false,
        etag: null,
        mtimeMs: null,
        readonly: true,
        lineEnding: "lf",
        saveState: "idle",
        saveError: null,
        hasLoadedSuccessfully: true,
        transient,
      }

      // Replace existing same-id tab (re-run with same gen is rare) or append.
      setFileTabs((prev) => {
        const idx = prev.findIndex((t) => t.id === tabId)
        const next =
          idx >= 0 ? prev.map((t, i) => (i === idx ? tab : t)) : [...prev, tab]
        fileTabsRef.current = next
        return next
      })
      setActiveFileTabId(tabId)
      activeFileTabIdRef.current = tabId
      activateFilePane()
      // User-initiated translate success → maximize (same policy as open).
      setFilesMaximized(true)
      if (language === "markdown") {
        setPreviewFileTabIds((prev) => {
          if (prev.has(tabId)) return prev
          const next = new Set(prev)
          next.add(tabId)
          return next
        })
      }
      return tabId
    },
    [activateFilePane, tFiles]
  )

  // Stable for the provider's lifetime: every callback reads mutable state
  // through refs or functional updaters, never through render-scoped
  // closures, so this memo's inputs only change if a callback identity
  // changes (which none do after mount).
  const actions = useMemo<WorkspaceActionsValue>(
    () => ({
      setActivePane,
      activateConversationPane,
      activateFilePane,
      switchFileTab,
      closeFileTab,
      closeOtherFileTabs,
      closeAllFileTabs,
      reorderFileTabs,
      openFilePreview,
      reloadOpenFileBackground,
      applyExternalReload,
      markTabsStale,
      rejectFileTab,
      consumePendingFileReveal,
      openWorkingTreeDiff,
      openBranchDiff,
      openCommitDiff,
      openSessionFileDiff,
      openExternalConflictDiff,
      updateActiveFileContent,
      saveActiveFile,
      reloadActiveFile,
      toggleFileTabPreview,
      toggleFilesMaximized,
      beginTranslateRequest,
      openTranslationResultTab,
    }),
    [
      setActivePane,
      activateConversationPane,
      activateFilePane,
      switchFileTab,
      closeFileTab,
      closeOtherFileTabs,
      closeAllFileTabs,
      reorderFileTabs,
      openFilePreview,
      reloadOpenFileBackground,
      applyExternalReload,
      markTabsStale,
      rejectFileTab,
      consumePendingFileReveal,
      openWorkingTreeDiff,
      openBranchDiff,
      openCommitDiff,
      openSessionFileDiff,
      openExternalConflictDiff,
      updateActiveFileContent,
      saveActiveFile,
      reloadActiveFile,
      toggleFileTabPreview,
      toggleFilesMaximized,
      beginTranslateRequest,
      openTranslationResultTab,
    ]
  )

  const view = useMemo<WorkspaceViewValue>(
    () => ({
      mode,
      activePane,
      filesMaximized: effectiveFilesMaximized,
    }),
    [mode, activePane, effectiveFilesMaximized]
  )

  const fileTabsValue = useMemo<WorkspaceFileTabsValue>(
    () => ({
      fileTabs,
      activeFileTabId,
      activeFileTab,
      activeFilePath,
      previewFileTabIds,
      pendingFileReveal,
    }),
    [
      fileTabs,
      activeFileTabId,
      activeFileTab,
      activeFilePath,
      previewFileTabIds,
      pendingFileReveal,
    ]
  )

  const externalConflictValue = useMemo<WorkspaceExternalConflictValue>(
    () => ({
      externalConflict: externalConflictQueue[0] ?? null,
      compareExternalConflict,
      reloadExternalConflict,
      saveExternalConflictCopy,
      dismissExternalConflict,
    }),
    [
      externalConflictQueue,
      compareExternalConflict,
      reloadExternalConflict,
      saveExternalConflictCopy,
      dismissExternalConflict,
    ]
  )

  return (
    <WorkspaceActionsContext.Provider value={actions}>
      <WorkspaceViewContext.Provider value={view}>
        <WorkspaceExternalConflictContext.Provider
          value={externalConflictValue}
        >
          <WorkspaceFileTabsContext.Provider value={fileTabsValue}>
            {children}
          </WorkspaceFileTabsContext.Provider>
        </WorkspaceExternalConflictContext.Provider>
      </WorkspaceViewContext.Provider>
    </WorkspaceActionsContext.Provider>
  )
}

// Workspace action callbacks. Value identity is stable for the provider's
// lifetime — subscribing here never re-renders on tab/content churn.
export function useWorkspaceActions(): WorkspaceActionsValue {
  const ctx = useContext(WorkspaceActionsContext)
  if (!ctx) {
    throw new Error("useWorkspaceActions must be used within WorkspaceProvider")
  }
  return ctx
}

// Low-frequency layout state (mode / activePane / filesMaximized). Changes
// only on fusion transitions, pane switches, and maximize toggles.
export function useWorkspaceView(): WorkspaceViewValue {
  const ctx = useContext(WorkspaceViewContext)
  if (!ctx) {
    throw new Error("useWorkspaceView must be used within WorkspaceProvider")
  }
  return ctx
}

// Disk-vs-buffer conflict queue head + resolutions. Isolated slice: only
// the always-mounted conflict dialog subscribes, and its value changes
// only when conflicts come and go — never on tab/content churn.
export function useWorkspaceExternalConflict(): WorkspaceExternalConflictValue {
  const ctx = useContext(WorkspaceExternalConflictContext)
  if (!ctx) {
    throw new Error(
      "useWorkspaceExternalConflict must be used within WorkspaceProvider"
    )
  }
  return ctx
}

// High-frequency tab data — changes on every keystroke, load, and
// watcher-driven reload. Only file-pane components should subscribe.
export function useWorkspaceFileTabs(): WorkspaceFileTabsValue {
  const ctx = useContext(WorkspaceFileTabsContext)
  if (!ctx) {
    throw new Error(
      "useWorkspaceFileTabs must be used within WorkspaceProvider"
    )
  }
  return ctx
}

/**
 * Aggregate of all three workspace slices.
 *
 * @deprecated Subscribes to the high-frequency fileTabs slice, so callers
 * re-render on every keystroke and watcher reload. Components on the
 * conversation render path must use `useWorkspaceActions` /
 * `useWorkspaceView` / `useWorkspaceFileTabs` instead.
 */
export function useWorkspaceContext(): WorkspaceContextValue {
  const actions = useWorkspaceActions()
  const view = useWorkspaceView()
  const fileTabs = useWorkspaceFileTabs()
  return useMemo(
    () => ({ ...actions, ...view, ...fileTabs }),
    [actions, view, fileTabs]
  )
}
