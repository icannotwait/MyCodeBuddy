"use client"

import type { ReactElement } from "react"
import { useCallback, useEffect, useMemo, useState } from "react"
import {
  ArrowRight,
  ChevronsDownUp,
  ChevronsUpDown,
  CloudOff,
  GitBranch,
  Loader2,
  Upload,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import {
  FileTree,
  FileTreeFile,
  FileTreeFolder,
} from "@/components/ai-elements/file-tree"
import {
  Commit,
  CommitContent,
  CommitFileAdditions,
  CommitFileChanges,
  CommitFileDeletions,
  CommitFileIcon,
  CommitFileInfo,
  CommitFilePath,
  CommitFiles,
  CommitFileStatus,
  CommitHash,
  CommitHeader,
  CommitInfo,
  CommitMessage,
  CommitMetadata,
  CommitTimestamp,
} from "@/components/ai-elements/commit"
import { DiffViewer } from "@/components/diff/diff-viewer"
import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { gitLog, gitPush, gitPushInfo, gitShowFile } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import { languageFromPath } from "@/lib/language-detect"
import type { GitLogEntry, GitLogFileChange, GitPushInfo } from "@/lib/types"
import {
  useGitCredential,
  type GitRemoteHint,
} from "@/contexts/git-credential-context"
import {
  buildCommitFileTree,
  collectExpandedDirectoryPaths,
  type CommitFileTreeNode,
} from "@/lib/commit-file-tree"
import {
  formatRelativeTime,
  mapFileStatus,
  parseDate,
} from "@/lib/git-log-format"

// --- Main component ---

interface PushWorkspaceProps {
  folderPath: string
  folderName: string
  folderId?: number | null
  onPushed?: () => void
}

export function PushWorkspace({
  folderPath,
  folderName,
  folderId,
  onPushed,
}: PushWorkspaceProps) {
  const t = useTranslations("Folder.pushWindow")
  const tLog = useTranslations("Folder.gitLogTab")
  const { withCredentialRetry } = useGitCredential()

  const [pushInfoData, setPushInfoData] = useState<GitPushInfo | null>(null)
  const [selectedRemote, setSelectedRemote] = useState<string | null>(null)
  const [commits, setCommits] = useState<GitLogEntry[]>([])
  const [hasUpstream, setHasUpstream] = useState(true)
  const [listLoading, setListLoading] = useState(false)
  const [openByCommit, setOpenByCommit] = useState<Record<string, boolean>>({})
  const [pushing, setPushing] = useState(false)

  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null)
  const [originalContent, setOriginalContent] = useState("")
  const [modifiedContent, setModifiedContent] = useState("")
  const [diffLoading, setDiffLoading] = useState(false)

  const unpushedCommits = useMemo(
    () => commits.filter((c) => c.pushed === false),
    [commits]
  )

  // Load push info (branch, remotes, tracking remote)
  useEffect(() => {
    gitPushInfo(folderPath)
      .then((info) => {
        setPushInfoData(info)
        // Default to tracking remote or first remote
        const defaultRemote =
          info.tracking_remote ??
          (info.remotes.length > 0 ? info.remotes[0].name : null)
        setSelectedRemote(defaultRemote)
      })
      .catch((err) => {
        toast.error(toErrorMessage(err))
      })
  }, [folderPath])

  // Deduplicate remotes (git remote -v returns fetch + push entries)
  const uniqueRemotes = useMemo(() => {
    if (!pushInfoData) return []
    const seen = new Set<string>()
    return pushInfoData.remotes.filter((r) => {
      if (seen.has(r.name)) return false
      seen.add(r.name)
      return true
    })
  }, [pushInfoData])

  const loadCommits = useCallback(
    async (remote?: string) => {
      setListLoading(true)
      try {
        const result = await gitLog(
          folderPath,
          100,
          undefined,
          remote ?? undefined
        )
        setCommits(result.entries)
        setHasUpstream(result.has_upstream)
      } catch (err) {
        toast.error(toErrorMessage(err))
      } finally {
        setListLoading(false)
      }
    },
    [folderPath]
  )

  // Reload commits when selected remote changes
  useEffect(() => {
    if (selectedRemote !== null) {
      loadCommits(selectedRemote)
    }
  }, [selectedRemote, loadCommits])

  async function handleSelectFile(commitHash: string, file: string) {
    setSelectedFile(file)
    setSelectedCommit(commitHash)
    setDiffLoading(true)
    try {
      const [orig, mod] = await Promise.all([
        gitShowFile(folderPath, file, `${commitHash}~1`).catch(() => ""),
        gitShowFile(folderPath, file, commitHash).catch(() => ""),
      ])
      setOriginalContent(orig)
      setModifiedContent(mod)
    } catch {
      setOriginalContent("")
      setModifiedContent("")
    } finally {
      setDiffLoading(false)
    }
  }

  async function handlePush() {
    setPushing(true)
    try {
      // Resolve the selected remote's URL for credential matching
      const remoteUrl = pushInfoData?.remotes.find(
        (r) => r.name === selectedRemote
      )?.url
      const hint: GitRemoteHint = remoteUrl ? { remoteUrl } : { folderPath }
      await withCredentialRetry(
        (creds) => gitPush(folderPath, selectedRemote, creds, folderId),
        hint
      )
      onPushed?.()
    } catch (err) {
      toast.error(t("toasts.pushFailed"), {
        description: toErrorMessage(err),
      })
    } finally {
      setPushing(false)
    }
  }

  return (
    <div className="flex h-full flex-col">
      {/* Push target header: branch → remote/branch */}
      {pushInfoData && (
        <div className="flex items-center gap-2 border-b px-3 py-2">
          <GitBranch className="h-4 w-4 shrink-0 text-muted-foreground" />
          <span className="truncate text-sm font-medium">
            {pushInfoData.branch}
          </span>
          {uniqueRemotes.length > 0 && (
            <ArrowRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          )}
          {uniqueRemotes.length === 0 ? null : uniqueRemotes.length <= 1 ? (
            <span className="truncate text-sm text-muted-foreground">
              {selectedRemote ?? "origin"}/{pushInfoData.branch}
            </span>
          ) : (
            <div className="flex items-center gap-1">
              <Select
                value={selectedRemote ?? ""}
                onValueChange={setSelectedRemote}
              >
                <SelectTrigger className="h-7 w-auto gap-1 border-none bg-transparent px-1.5 text-sm shadow-none">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {uniqueRemotes.map((r) => (
                    <SelectItem key={r.name} value={r.name}>
                      {r.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <span className="text-sm text-muted-foreground">
                /{pushInfoData.branch}
              </span>
            </div>
          )}
        </div>
      )}

      <ResizablePanelGroup direction="horizontal" className="min-h-0 flex-1">
        {/* Left panel: commit list */}
        <ResizablePanel defaultSize={35} minSize={25}>
          <div className="flex h-full flex-col">
            <ScrollArea className="min-h-0 flex-1">
              {listLoading ? (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
                </div>
              ) : uniqueRemotes.length === 0 ? (
                <div className="flex items-center justify-center px-4 py-12 text-center text-sm text-muted-foreground whitespace-pre-line">
                  {t("noRemoteConfigured")}
                </div>
              ) : unpushedCommits.length === 0 ? (
                <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
                  {!hasUpstream
                    ? t("newBranchNoPushedCommits")
                    : t("noUnpushedCommits")}
                </div>
              ) : (
                <div className="flex flex-col gap-2 p-2">
                  {unpushedCommits.map((entry) => {
                    const commitKey = entry.full_hash
                    const commitDate = parseDate(entry.date)
                    const isOpen = !!openByCommit[commitKey]

                    return (
                      <Commit
                        key={commitKey}
                        open={isOpen}
                        onOpenChange={(open) => {
                          setOpenByCommit((prev) => ({
                            ...prev,
                            [commitKey]: open,
                          }))
                        }}
                      >
                        <CommitHeader>
                          <CommitInfo className="min-w-0">
                            <CommitMessage className="line-clamp-1 leading-snug">
                              {entry.message}
                            </CommitMessage>
                            <CommitMetadata className="mt-1 min-w-0 flex items-center gap-1.5">
                              <span
                                className="inline-flex shrink-0"
                                title={t("unpushed")}
                              >
                                <CloudOff
                                  className="text-amber-500"
                                  size={12}
                                />
                              </span>
                              <span className="truncate">{entry.author}</span>
                              <CommitTimestamp
                                className="shrink-0"
                                date={commitDate ?? new Date()}
                              >
                                {formatRelativeTime(entry.date, tLog)}
                              </CommitTimestamp>
                              <CommitHash className="text-primary/70">
                                {entry.hash}
                              </CommitHash>
                            </CommitMetadata>
                          </CommitInfo>
                        </CommitHeader>
                        <CommitContent>
                          {entry.files.length === 0 ? (
                            <p className="text-xs text-muted-foreground">
                              {tLog("noFileChangeDetails")}
                            </p>
                          ) : (
                            <PushCommitFilesTree
                              commitHash={entry.full_hash}
                              files={entry.files}
                              folderName={folderName}
                              onSelectFile={(file) =>
                                handleSelectFile(entry.full_hash, file)
                              }
                            />
                          )}
                        </CommitContent>
                      </Commit>
                    )
                  })}
                </div>
              )}
            </ScrollArea>

            {/* Push button */}
            <div className="border-t p-2">
              <Button
                className="w-full"
                disabled={
                  pushing ||
                  uniqueRemotes.length === 0 ||
                  (hasUpstream && unpushedCommits.length === 0)
                }
                onClick={handlePush}
              >
                {pushing ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Upload className="mr-2 h-4 w-4" />
                )}
                {t("push")}
                {unpushedCommits.length > 0 && ` (${unpushedCommits.length})`}
              </Button>
            </div>
          </div>
        </ResizablePanel>

        <ResizableHandle />

        {/* Right panel: diff viewer */}
        <ResizablePanel defaultSize={65} minSize={40}>
          {diffLoading ? (
            <div className="flex h-full items-center justify-center">
              <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
            </div>
          ) : selectedFile && selectedCommit ? (
            <DiffViewer
              original={originalContent}
              modified={modifiedContent}
              originalLabel={`${selectedCommit.slice(0, 7)}~ (${t("before")})`}
              modifiedLabel={`${selectedCommit.slice(0, 7)} (${t("after")})`}
              language={languageFromPath(selectedFile)}
              className="h-full"
            />
          ) : (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
              {t("selectFileToViewDiff")}
            </div>
          )}
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  )
}

// --- Commit Files Tree for Push Window ---

function PushCommitFilesTree({
  commitHash,
  files,
  folderName,
  onSelectFile,
}: {
  commitHash: string
  files: GitLogFileChange[]
  folderName: string
  onSelectFile: (file: string) => void
}) {
  const tLog = useTranslations("Folder.gitLogTab")
  const rootPath = "__push_file_tree_root__"
  const treeNodes = useMemo(() => buildCommitFileTree(files), [files])
  const allDirectoryPaths = useMemo(() => {
    const paths = collectExpandedDirectoryPaths(treeNodes)
    paths.add(rootPath)
    return paths
  }, [treeNodes])
  const [expandedPaths, setExpandedPaths] =
    useState<Set<string>>(allDirectoryPaths)

  useEffect(() => {
    setExpandedPaths(allDirectoryPaths)
  }, [allDirectoryPaths])

  const canExpandAll = useMemo(() => {
    if (allDirectoryPaths.size === 0) return false
    for (const path of allDirectoryPaths) {
      if (!expandedPaths.has(path)) return true
    }
    return false
  }, [allDirectoryPaths, expandedPaths])

  const canCollapseAll = expandedPaths.size > 0

  const toggleExpanded = useCallback(() => {
    if (canExpandAll) {
      setExpandedPaths(new Set(allDirectoryPaths))
      return
    }
    setExpandedPaths(new Set())
  }, [allDirectoryPaths, canExpandAll])

  const renderNode = (node: CommitFileTreeNode): ReactElement => {
    if (node.kind === "dir") {
      return (
        <FileTreeFolder
          key={node.path}
          path={node.path}
          name={node.name}
          suffix={`(${node.fileCount})`}
          suffixClassName="text-muted-foreground/45"
          title={node.path}
        >
          {node.children.map(renderNode)}
        </FileTreeFolder>
      )
    }

    const file = node.change
    return (
      <FileTreeFile
        key={`${commitHash}:${file.path}`}
        className="w-full min-w-0 cursor-pointer"
        name={node.name}
        onClick={() => onSelectFile(file.path)}
        path={node.path}
        title={file.path}
      >
        <>
          <span className="size-4 shrink-0" />
          <CommitFileInfo className="flex-1 min-w-0 gap-1.5">
            <CommitFileStatus status={mapFileStatus(file.status)}>
              {file.status}
            </CommitFileStatus>
            <CommitFileIcon />
            <CommitFilePath title={file.path}>{node.name}</CommitFilePath>
          </CommitFileInfo>
          <CommitFileChanges>
            <CommitFileAdditions count={file.additions} />
            <CommitFileDeletions count={file.deletions} />
          </CommitFileChanges>
        </>
      </FileTreeFile>
    )
  }

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between gap-2">
        <p className="text-[11px] text-muted-foreground">
          {tLog("filesTitle")}
        </p>
        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="icon"
            className="size-5"
            onClick={toggleExpanded}
            disabled={!canExpandAll && !canCollapseAll}
            title={
              canExpandAll ? tLog("expandAllFiles") : tLog("collapseAllFiles")
            }
          >
            {canExpandAll ? (
              <ChevronsUpDown className="size-3.5" />
            ) : (
              <ChevronsDownUp className="size-3.5" />
            )}
          </Button>
        </div>
      </div>
      <CommitFiles>
        <FileTree
          className="max-h-[32rem] overflow-auto rounded-md border-border/60 bg-transparent text-xs [&>div]:p-1"
          expanded={expandedPaths}
          onExpandedChange={setExpandedPaths}
        >
          <FileTreeFolder
            path={rootPath}
            name={folderName}
            suffix={`(${files.length})`}
            suffixClassName="text-muted-foreground/45"
            title={folderName}
          >
            {treeNodes.map(renderNode)}
          </FileTreeFolder>
        </FileTree>
      </CommitFiles>
    </div>
  )
}
