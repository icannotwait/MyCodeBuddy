"use client"

/**
 * Expanded workflow graph: collapsible phase lanes with adaptive node rows.
 * Observed nodes open via child-tab path; estimated nodes are non-actionable.
 * Node rows mirror SubAgentOverlayRow card chrome (no per-node detail expand).
 */

import {
  memo,
  useCallback,
  useMemo,
  useState,
  useSyncExternalStore,
} from "react"
import {
  AlertTriangleIcon,
  ArrowRightIcon,
  ChevronDownIcon,
  Eye,
  FileTextIcon,
} from "lucide-react"
import { useTranslations } from "next-intl"

import { AgentIcon } from "@/components/agent-icon"
import { useOpenLinkOrFile } from "@/components/ai-elements/link-safety"
import { WorkflowStatusIcon } from "@/components/chat/workflow-status-icon"
import { CompletionDecisionCard } from "@/components/chat/completion-decision-card"
import { phaseProgressFragments } from "@/components/chat/workflow-phase-rail"
import { Badge } from "@/components/ui/badge"
import { getAgentLabel } from "@/lib/custom-agents"
import {
  computeDelegationElapsedMs,
  type EditRollupViewModel,
} from "@/lib/delegation-card"
import { formatElapsedLabel } from "@/lib/format-elapsed"
import { formatConversationTitle } from "@/lib/conversation-title"
import { joinRootRel } from "@/lib/file-open-target"
import { openDelegatedChildSession } from "@/lib/open-delegated-child-session"
import {
  buildPhaseRail,
  canOpenWorkflowNode,
  isEstimatedNode,
  type PhaseRailItem,
  type PhaseRailKind,
} from "@/lib/workflow-graph-store"
import type {
  AgentType,
  CompletionProjectionV2,
  WorkflowGraphSnapshot,
  WorkflowNodeSnapshot,
} from "@/lib/types"
import { cn } from "@/lib/utils"

/** Line-2 title: session/task text first; never leave blank when summary exists. */
function nodeDisplayTitle(node: WorkflowNodeSnapshot): string {
  const fromTitle = formatConversationTitle(node.title).trim()
  if (fromTitle) return fromTitle
  const fromSummary = formatConversationTitle(node.summary).trim()
  if (fromSummary) return fromSummary
  return node.node_id
}

const PHASE_ORDER: PhaseRailKind[] = ["design", "plan", "tasks", "final"]

type LaneBooleanMap = Record<PhaseRailKind, boolean>

interface WorkflowGraphPanelProps {
  snapshot: WorkflowGraphSnapshot
  conversationId?: number | null
  workspaceRootPath?: string | null
  className?: string
  onResumeRoot?: () => void | Promise<void>
  onOpenRootConversation?: (conversationId: number) => void | Promise<void>
}

function laneDefaults(phases: readonly PhaseRailItem[]): LaneBooleanMap {
  return Object.fromEntries(
    phases.map((phase) => [phase.kind, phase.nodeRows.length > 0])
  ) as LaneBooleanMap
}

function isAgentType(value: string | null | undefined): value is AgentType {
  return typeof value === "string" && value.length > 0
}

function isLiveNodeStatus(status: WorkflowNodeSnapshot["status"]): boolean {
  return status === "running" || status === "reserving"
}

const SIMPLE_ADVISORY_WARNING_CODES = new Set([
  "simple_completed_task_missing_commit",
])

function hasBlockingSimpleWarning(codes: readonly string[]): boolean {
  return codes.some((code) => !SIMPLE_ADVISORY_WARNING_CODES.has(code))
}

function isSimpleNodeOutOfSync(node: WorkflowNodeSnapshot): boolean {
  if (node.sync_state !== "out_of_sync") return false
  return (
    node.projection_warning_codes.length === 0 ||
    hasBlockingSimpleWarning(node.projection_warning_codes)
  )
}

/**
 * 1s tick while any live node needs a live elapsed clock.
 * Snapshot value must only change on interval fire — never return Date.now()
 * from getSnapshot (that would re-render every read).
 */
let liveClockMs = 0
const liveClockListeners = new Set<() => void>()
let liveClockInterval: number | null = null

function ensureLiveClock() {
  if (liveClockInterval != null) return
  liveClockMs = Date.now()
  liveClockInterval = window.setInterval(() => {
    liveClockMs = Date.now()
    for (const listener of liveClockListeners) listener()
  }, 1000)
}

function releaseLiveClock() {
  if (liveClockListeners.size > 0) return
  if (liveClockInterval != null) {
    window.clearInterval(liveClockInterval)
    liveClockInterval = null
  }
}

function subscribeLiveClock(onStoreChange: () => void): () => void {
  liveClockListeners.add(onStoreChange)
  ensureLiveClock()
  return () => {
    liveClockListeners.delete(onStoreChange)
    releaseLiveClock()
  }
}

function useNowMs(active: boolean): number {
  return useSyncExternalStore(
    active ? subscribeLiveClock : () => () => {},
    // Terminal-only views never need a wall clock (finished − started).
    // Live views read the interval-backed snapshot (updated once per second).
    () => (active ? liveClockMs : 0),
    () => 0
  )
}

type EditSegmentTranslator = {
  (
    key: "editFilesCount" | "editCallsDetected",
    values: { count: number }
  ): string
  (key: "editFilesTruncated", values: { count: number }): string
  (key: "lineTotals", values: { additions: number; deletions: number }): string
}

function editRollupFromNode(node: WorkflowNodeSnapshot): EditRollupViewModel {
  const fileCount = node.touched_file_count
  if (fileCount != null && fileCount > 0) {
    const additions = node.additions ?? null
    const deletions = node.deletions ?? null
    const showLineTotals =
      node.line_counts_complete === true &&
      additions != null &&
      deletions != null
    return {
      mode: "files",
      fileCount,
      fileCountTruncated: node.touched_files_truncated === true,
      additions,
      deletions,
      showLineTotals,
    }
  }
  const editCalls = node.edit_tool_call_count
  if (editCalls != null && editCalls > 0) {
    return { mode: "editCalls", editCallCount: editCalls }
  }
  return { mode: "omit" }
}

function buildEditSegment(
  editRollup: EditRollupViewModel,
  t: EditSegmentTranslator
): string | null {
  if (editRollup.mode === "files") {
    const countLabel = editRollup.fileCountTruncated
      ? t("editFilesTruncated", { count: editRollup.fileCount })
      : t("editFilesCount", { count: editRollup.fileCount })
    if (
      editRollup.showLineTotals &&
      editRollup.additions != null &&
      editRollup.deletions != null
    ) {
      return `${countLabel} ${t("lineTotals", {
        additions: editRollup.additions,
        deletions: editRollup.deletions,
      })}`
    }
    return countLabel
  }
  if (editRollup.mode === "editCalls") {
    return t("editCallsDetected", { count: editRollup.editCallCount })
  }
  return null
}

type LiveStatsTranslator = {
  (key: "toolUseCount", values: { count: number }): string
} & Parameters<typeof formatElapsedLabel>[1]

function totalElapsedMs(
  node: WorkflowNodeSnapshot,
  nowMs: number
): number | null {
  const completed =
    typeof node.elapsed_completed_ms === "number" &&
    Number.isFinite(node.elapsed_completed_ms) &&
    node.elapsed_completed_ms >= 0
      ? node.elapsed_completed_ms
      : 0

  // Live latest run: add wall-clock since its start. Prior finished generations
  // are already in `elapsed_completed_ms`.
  if (isLiveNodeStatus(node.status)) {
    const live = computeDelegationElapsedMs({
      lifecycleStatus: "running",
      startedAt: node.started_at ?? null,
      finishedAt: null,
      completedDurationMs: null,
      nowMs,
    })
    if (live == null && completed === 0) return null
    return completed + (live ?? 0)
  }

  // Terminal: prefer the lineage sum (includes latest when finished).
  if (completed > 0) return completed

  // Single-run / legacy snapshots without elapsed_completed_ms.
  return computeDelegationElapsedMs({
    lifecycleStatus: "ok",
    startedAt: node.started_at ?? null,
    finishedAt: node.finished_at ?? null,
    completedDurationMs: null,
    nowMs,
  })
}

function buildOperationalLine(
  node: WorkflowNodeSnapshot,
  nowMs: number,
  tLive: LiveStatsTranslator,
  tEdit: EditSegmentTranslator
): string | null {
  const segments: string[] = []
  const elapsedMs = totalElapsedMs(node, nowMs)
  if (elapsedMs != null) {
    segments.push(formatElapsedLabel(elapsedMs, tLive))
  }
  if (node.tool_call_count != null) {
    segments.push(tLive("toolUseCount", { count: node.tool_call_count }))
  }
  const editSegment = buildEditSegment(editRollupFromNode(node), tEdit)
  if (editSegment) segments.push(editSegment)
  return segments.length > 0 ? segments.join(" | ") : null
}

function HistoricalCompletionCard({
  request,
}: {
  request: CompletionProjectionV2
}) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const card = request.card
  return (
    <div
      className="space-y-1.5 border-t border-border/60 pt-2"
      data-testid="workflow-historical-completion-card"
    >
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="min-w-0 text-xs font-medium">
          {t(
            card.state === "resolved"
              ? "completionResolved"
              : card.state === "needs_decision"
                ? "completionNeedsDecision"
                : "completionBlocked"
          )}
        </span>
        {card.evidence_validated && (
          <span className="shrink-0 text-[10px] text-muted-foreground">
            {t("completionEvidenceValidated")}
          </span>
        )}
      </div>
      {card.summary && (
        <p className="break-words text-xs leading-5 text-muted-foreground">
          {card.summary}
        </p>
      )}
      {(card.source || card.report_file) && (
        <div className="flex min-w-0 flex-wrap gap-x-3 text-[10px] text-muted-foreground">
          {card.source && <span>{t(`completionSource.${card.source}`)}</span>}
          {card.report_file && (
            <span className="min-w-0 break-all">
              {t("completionReportFile", { file: card.report_file })}
            </span>
          )}
        </div>
      )}
    </div>
  )
}

function SimpleWorkflowProjection({
  snapshot,
  workspaceRootPath,
  className,
}: {
  snapshot: WorkflowGraphSnapshot
  workspaceRootPath?: string | null
  className?: string
}) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const tLive = useTranslations("Folder.chat.liveTurnStats")
  const tDel = useTranslations("Folder.chat.delegation")
  const openLinkOrFile = useOpenLinkOrFile()
  const needsLiveClock = snapshot.nodes.some((node) =>
    isLiveNodeStatus(node.status)
  )
  const nowMs = useNowMs(needsLiveClock)
  const locator = snapshot.simple ?? null
  const partial =
    locator == null ||
    hasBlockingSimpleWarning(snapshot.projection_warning_codes) ||
    snapshot.nodes.some((node) =>
      hasBlockingSimpleWarning(node.projection_warning_codes)
    )

  const openFile = useCallback(
    (relPath: string) => {
      const target = workspaceRootPath
        ? joinRootRel(workspaceRootPath, relPath)
        : relPath
      void openLinkOrFile(target)
    },
    [openLinkOrFile, workspaceRootPath]
  )

  const openSession = useCallback((node: WorkflowNodeSnapshot) => {
    if (!canOpenWorkflowNode(node)) return
    void openDelegatedChildSession({
      childConversationId: node.latest_child_conversation_id,
      agentType: (node.agent_type as AgentType | null) ?? null,
      title: node.title,
    })
  }, [])

  return (
    <div
      className={cn("min-w-0 space-y-2", className)}
      data-testid="workflow-graph-panel"
      data-compatibility="simple"
      role="region"
      aria-label={t("simpleTasks")}
    >
      {locator && (
        <div className="flex min-w-0 gap-1.5" data-testid="simple-file-links">
          <button
            type="button"
            className="inline-flex h-8 min-w-0 flex-1 items-center justify-center gap-1.5 rounded-md border px-2 text-xs font-medium hover:bg-muted/60"
            onClick={() => openFile(locator.plan_rel_path)}
            title={locator.plan_rel_path}
            aria-label={t("simpleOpenPlan")}
          >
            <FileTextIcon className="size-3.5 shrink-0" aria-hidden />
            <span className="min-w-0 truncate">{t("simpleOpenPlan")}</span>
          </button>
          <button
            type="button"
            className="inline-flex h-8 min-w-0 flex-1 items-center justify-center gap-1.5 rounded-md border px-2 text-xs font-medium hover:bg-muted/60"
            data-testid="simple-progress-link"
            onClick={() => openFile(locator.progress_rel_path)}
            title={locator.progress_rel_path}
            aria-label={t("simpleOpenProgress")}
          >
            <FileTextIcon className="size-3.5 shrink-0" aria-hidden />
            <span className="min-w-0 truncate">{t("simpleOpenProgress")}</span>
          </button>
        </div>
      )}

      {partial && (
        <div
          className="min-w-0 border-s-2 border-amber-500 px-2 py-1 text-xs"
          data-testid="simple-projection-warning"
        >
          <div className="flex min-w-0 items-center gap-1.5 font-medium">
            <AlertTriangleIcon
              className="size-3.5 shrink-0 text-amber-600 dark:text-amber-400"
              aria-hidden
            />
            <span className="min-w-0 break-words">
              {t("simplePartialProjection")}
            </span>
          </div>
          <p className="mt-0.5 break-words text-[11px] text-muted-foreground">
            {t("simpleProjectionWarning")}
          </p>
        </div>
      )}

      <div className="flex min-w-0 items-center justify-between gap-2 px-1">
        <span className="min-w-0 text-xs font-semibold">
          {t("simpleTasks")}
        </span>
        <span className="shrink-0 text-[10px] text-muted-foreground">
          {t("simpleTaskCount", { count: snapshot.nodes.length })}
        </span>
      </div>

      {snapshot.nodes.length === 0 ? (
        <p className="px-1 py-2 text-xs text-muted-foreground">
          {t("simpleNoTasks")}
        </p>
      ) : (
        <ul className="divide-y divide-border/60 border-y border-border/60">
          {snapshot.nodes.map((node) => {
            const title = nodeDisplayTitle(node)
            const openable = canOpenWorkflowNode(node)
            const liveRun =
              node.latest_run_status === "running" ||
              node.latest_run_status === "reserving"
                ? node.latest_run_status
                : isLiveNodeStatus(node.status)
                  ? node.status
                  : null
            const operationalLine = buildOperationalLine(
              node,
              nowMs,
              tLive as unknown as LiveStatsTranslator,
              tDel as unknown as EditSegmentTranslator
            )
            return (
              <li
                key={node.node_id}
                className="flex min-h-12 min-w-0 items-center gap-2 px-1 py-1.5"
                data-testid={`simple-task-row-${node.node_id}`}
                data-sync-state={node.sync_state}
              >
                <WorkflowStatusIcon visualStatus={node.status} />
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 items-center gap-1.5">
                    {node.task_index != null && (
                      <span className="shrink-0 text-[10px] font-medium text-muted-foreground">
                        {t("taskIndex", { index: node.task_index })}
                      </span>
                    )}
                    <span
                      className="min-w-0 flex-1 truncate text-xs font-medium"
                      title={title}
                    >
                      {title}
                    </span>
                    <Badge
                      variant="secondary"
                      className="h-4 shrink-0 px-1 text-[10px]"
                    >
                      {t(`nodeStatus.${node.status}`)}
                    </Badge>
                  </div>
                  {liveRun && (
                    <p className="truncate text-[11px] text-blue-600 dark:text-blue-400">
                      {t("simpleLiveRun", {
                        status: t(`nodeStatus.${liveRun}`),
                      })}
                    </p>
                  )}
                  {operationalLine && (
                    <p
                      className="truncate text-[11px] text-muted-foreground"
                      title={operationalLine}
                    >
                      {operationalLine}
                    </p>
                  )}
                  {isSimpleNodeOutOfSync(node) && (
                    <p className="break-words text-[11px] text-amber-700 dark:text-amber-300">
                      {t("simpleOutOfSync")}
                    </p>
                  )}
                </div>
                {openable && (
                  <button
                    type="button"
                    className="inline-flex size-7 shrink-0 items-center justify-center rounded-md hover:bg-muted/60"
                    data-testid={`simple-task-open-${node.node_id}`}
                    onClick={() => openSession(node)}
                    title={t("openSession")}
                    aria-label={t("openSession")}
                  >
                    <Eye className="size-3.5" aria-hidden />
                  </button>
                )}
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}

export const WorkflowGraphPanel = memo(function WorkflowGraphPanel({
  snapshot,
  workspaceRootPath,
  className,
  onOpenRootConversation,
}: WorkflowGraphPanelProps) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const tLive = useTranslations("Folder.chat.liveTurnStats")
  const tDel = useTranslations("Folder.chat.delegation")
  const [dependenciesExpanded, setDependenciesExpanded] = useState(false)
  const readOnly =
    snapshot.archived != null || snapshot.completion_protocol?.version === 1

  const needsLiveClock = useMemo(
    () => snapshot.nodes.some((node) => isLiveNodeStatus(node.status)),
    [snapshot.nodes]
  )
  const nowMs = useNowMs(needsLiveClock)

  const lanes = useMemo(() => {
    const byKind = new Map(
      buildPhaseRail(snapshot).map((lane) => [lane.kind, lane])
    )
    return PHASE_ORDER.map((kind) => byKind.get(kind)!)
  }, [snapshot])

  const defaults = useMemo(() => laneDefaults(lanes), [lanes])
  // Dirty lanes store the user's manual expansion choice; non-dirty lanes
  // always follow empty→collapsed / non-empty→expanded defaults.
  const [dirtyByLane, setDirtyByLane] = useState<Partial<LaneBooleanMap>>({})

  const expandedByLane = useMemo(() => {
    return Object.fromEntries(
      PHASE_ORDER.map((kind) => [
        kind,
        dirtyByLane[kind] !== undefined ? dirtyByLane[kind]! : defaults[kind],
      ])
    ) as LaneBooleanMap
  }, [defaults, dirtyByLane])

  const toggleLane = useCallback(
    (kind: PhaseRailKind) => {
      setDirtyByLane((previous) => {
        const current =
          previous[kind] !== undefined ? previous[kind]! : defaults[kind]
        return { ...previous, [kind]: !current }
      })
    },
    [defaults]
  )

  const nodeTitles = useMemo(
    () =>
      new Map(
        snapshot.nodes.map((node) => [node.node_id, nodeDisplayTitle(node)])
      ),
    [snapshot.nodes]
  )
  const workflowCompletionIsOnNode = useMemo(() => {
    const attentionId = snapshot.completion?.card.attention?.attention_id
    if (!attentionId) return false
    return snapshot.nodes.some(
      (node) => node.completion?.card.attention?.attention_id === attentionId
    )
  }, [snapshot.completion, snapshot.nodes])

  const onOpenSession = useCallback(async (node: WorkflowNodeSnapshot) => {
    if (!canOpenWorkflowNode(node)) return
    await openDelegatedChildSession({
      childConversationId: node.latest_child_conversation_id,
      agentType: (node.agent_type as AgentType | null) ?? null,
      title: node.title,
    })
  }, [])

  const renderNodeRow = (
    node: WorkflowNodeSnapshot,
    laneKind: PhaseRailKind,
    reviewerWrapperTestId?: string
  ) => {
    const estimated = isEstimatedNode(node)
    const openable = canOpenWorkflowNode(node)
    // Prefer real task/session title; never leave line 2 blank when summary exists.
    const title = nodeDisplayTitle(node)
    const agentType = isAgentType(node.agent_type) ? node.agent_type : null
    const model = node.model?.trim() || null
    const effort = node.effort?.trim() || null
    const operationalLine = buildOperationalLine(
      node,
      nowMs,
      tLive as unknown as LiveStatsTranslator,
      tDel as unknown as EditSegmentTranslator
    )
    const accessibleName = [
      t(`phase.${laneKind}`),
      node.task_index != null
        ? t("taskIndex", { index: node.task_index })
        : null,
      node.role,
      node.agent_type,
      model,
      effort,
      operationalLine,
      t(`nodeStatus.${node.status}`),
      node.title,
    ]
      .filter(Boolean)
      .join(", ")

    // Role is the card identity (workflow "角色卡片"); fall back to agent/title.
    const primaryLabel = node.role
      ? t("roleLabel", { role: node.role })
      : agentType
        ? (getAgentLabel(agentType) ?? agentType)
        : title

    // Line 3: agent type / model / effort (not the title — title is line 2).
    const agentBits = [
      agentType ? t("agentLabel", { agent: agentType }) : null,
      model ? t("modelLabel", { model }) : null,
      effort ? t("effortLabel", { effort }) : null,
      !node.required ? t("optionalReviewer") : null,
    ].filter(Boolean) as string[]

    return (
      <div
        key={node.node_id}
        data-testid={reviewerWrapperTestId}
        className="min-w-0"
      >
        <div
          data-testid={`workflow-graph-node-${node.node_id}`}
          data-status={node.status}
          data-estimated={estimated ? "true" : "false"}
          data-openable={openable ? "true" : "false"}
          aria-label={accessibleName}
          aria-disabled={estimated ? "true" : undefined}
          title={estimated ? t("estimatedNonActionable") : undefined}
          className={cn(
            "flex h-auto w-full min-w-0 items-start gap-2 rounded-lg border bg-transparent px-2 py-1.5",
            estimated &&
              "cursor-default border-dashed text-muted-foreground opacity-80"
          )}
        >
          <div className="min-w-0 flex-1 space-y-1">
            {/* Line 1: identity + status */}
            <div className="flex min-w-0 flex-wrap items-center gap-1.5">
              <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-border bg-background text-foreground">
                {agentType ? (
                  <AgentIcon agentType={agentType} className="h-3.5 w-3.5" />
                ) : (
                  <WorkflowStatusIcon
                    visualStatus={node.status}
                    className="size-3.5"
                  />
                )}
              </span>
              <span className="min-w-0 break-words text-xs font-semibold text-foreground">
                {primaryLabel}
              </span>
              <Badge
                variant="secondary"
                className="h-4 shrink-0 px-1 text-[10px]"
              >
                {t(`nodeStatus.${node.status}`)}
              </Badge>
              {node.run_count > 0 && (
                <span className="shrink-0 tabular-nums text-[11px] text-muted-foreground">
                  {t("runCount", { count: node.run_count })}
                </span>
              )}
              {node.replacement_count > 0 && (
                <span className="shrink-0 tabular-nums text-[11px] text-muted-foreground">
                  {t("replacementCount", {
                    count: node.replacement_count,
                  })}
                </span>
              )}
            </div>
            {/* Line 2: title */}
            <div
              data-node-title
              className="min-w-0 text-xs text-muted-foreground line-clamp-2"
              title={title}
            >
              {title}
            </div>
            {/* Line 3: agent / model / effort */}
            {agentBits.length > 0 && (
              <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
                {agentBits.map((bit) => (
                  <span key={bit} className="min-w-0 break-words">
                    {bit}
                  </span>
                ))}
              </div>
            )}
            {/* Line 4: elapsed | tools | file edits (same segments as sub-agent cards) */}
            {operationalLine && (
              <div
                data-testid={`workflow-graph-node-ops-${node.node_id}`}
                className="min-w-0 truncate text-[11px] leading-snug text-muted-foreground"
                title={operationalLine}
              >
                {operationalLine}
              </div>
            )}
          </div>
          {openable && (
            <button
              type="button"
              data-testid={`workflow-graph-node-open-${node.node_id}`}
              onClick={() => {
                void onOpenSession(node)
              }}
              className="inline-flex shrink-0 items-center gap-1 self-center rounded-md px-1.5 py-1 text-[11px] font-medium text-foreground/80 transition-colors hover:bg-muted/60 hover:text-foreground"
              title={t("openSession")}
              aria-label={t("openSession")}
            >
              <Eye className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
        {readOnly && node.completion ? (
          <HistoricalCompletionCard request={node.completion} />
        ) : (
          node.completion && (
            <CompletionDecisionCard request={node.completion} />
          )
        )}
      </div>
    )
  }

  if (snapshot.compatibility === "simple") {
    return (
      <SimpleWorkflowProjection
        snapshot={snapshot}
        workspaceRootPath={workspaceRootPath}
        className={className}
      />
    )
  }

  return (
    <div
      className={cn("space-y-2", className)}
      data-testid="workflow-graph-panel"
      role="region"
      aria-label={t("graphTitle")}
    >
      {snapshot.completion &&
        !workflowCompletionIsOnNode &&
        (readOnly ? (
          <HistoricalCompletionCard request={snapshot.completion} />
        ) : (
          <CompletionDecisionCard request={snapshot.completion} />
        ))}
      {snapshot.completion_protocol && (
        <div className="space-y-2 rounded-md border bg-muted/20 p-2 text-xs">
          {snapshot.completion_protocol.read_only_reason && (
            <p className="font-medium">{t("completionLegacyReadOnly")}</p>
          )}
          <div className="flex flex-wrap gap-x-3 gap-y-1 text-muted-foreground">
            {snapshot.completion_protocol.legacy_source && (
              <button
                type="button"
                disabled={!onOpenRootConversation}
                className="underline-offset-2 enabled:hover:text-foreground enabled:hover:underline disabled:cursor-default"
                onClick={() =>
                  void onOpenRootConversation?.(
                    snapshot.completion_protocol!.legacy_source!.conversation_id
                  )
                }
              >
                {t("completionLegacySource", {
                  conversation:
                    snapshot.completion_protocol.legacy_source.conversation_id,
                })}
              </button>
            )}
            {snapshot.completion_protocol.v2_successor && (
              <button
                type="button"
                disabled={!onOpenRootConversation}
                className="underline-offset-2 enabled:hover:text-foreground enabled:hover:underline disabled:cursor-default"
                onClick={() =>
                  void onOpenRootConversation?.(
                    snapshot.completion_protocol!.v2_successor!.conversation_id
                  )
                }
              >
                {t("completionLegacySuccessor", {
                  conversation:
                    snapshot.completion_protocol.v2_successor.conversation_id,
                })}
              </button>
            )}
          </div>
          {snapshot.completion_protocol.automatic_root_wake && (
            <p className="text-muted-foreground">
              {t("completionAutomaticWake")}
            </p>
          )}
        </div>
      )}
      <div className="flex flex-col gap-2">
        {lanes.map((lane) => {
          const expanded = expandedByLane[lane.kind]
          const progressParts = phaseProgressFragments(lane, t)
          return (
            <section
              key={lane.kind}
              className="min-w-0 space-y-1.5 rounded-md border bg-card/40 p-1.5"
              data-testid={`workflow-graph-lane-${lane.kind}`}
              aria-label={t(`phase.${lane.kind}`)}
            >
              <button
                type="button"
                className="flex w-full min-w-0 items-center gap-1.5 rounded px-1 py-0.5 text-start hover:bg-muted/40"
                data-testid={`workflow-lane-toggle-${lane.kind}`}
                aria-expanded={expanded}
                aria-label={t("laneToggleAria", {
                  phase: t(`phase.${lane.kind}`),
                })}
                onClick={() => toggleLane(lane.kind)}
              >
                <WorkflowStatusIcon visualStatus={lane.status} />
                <span className="min-w-0 flex-1 truncate text-xs font-semibold">
                  {t(`phase.${lane.kind}`)}
                </span>
                <span className="shrink-0 text-[10px] text-muted-foreground">
                  {t(`phaseStatus.${lane.status}`)}
                </span>
                {progressParts.length > 0 && (
                  <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">
                    {progressParts.join(" · ")}
                  </span>
                )}
                {!expanded && lane.nodeRows.length === 0 && (
                  <span className="min-w-0 truncate text-[10px] text-muted-foreground">
                    {t("emptyLane")}
                  </span>
                )}
                <ChevronDownIcon
                  className={cn(
                    "size-3.5 shrink-0 text-muted-foreground transition-transform",
                    !expanded && "-rotate-90"
                  )}
                  aria-hidden
                />
              </button>

              {expanded && lane.nodeRows.length === 0 && (
                <p className="px-1 text-[10px] text-muted-foreground">
                  {t("emptyLane")}
                </p>
              )}

              {expanded && lane.nodeRows.length > 0 && (
                <ul className="space-y-2">
                  {lane.nodeRows.map((row) => {
                    const primary = row.nodes.filter(
                      (node) => node.role !== "reviewer"
                    )
                    const reviewers = row.nodes.filter(
                      (node) => node.role === "reviewer"
                    )
                    const taskIndex =
                      lane.kind === "tasks" ? row.taskIndex : null
                    const taskRow = taskIndex != null

                    return (
                      <li key={row.id} className="min-w-0 space-y-1">
                        {taskIndex != null && (
                          <div
                            className="flex items-center gap-2 px-1 text-[10px] font-medium text-muted-foreground"
                            data-testid={`workflow-task-reviewer-count-${taskIndex}`}
                          >
                            <span>{t("taskIndex", { index: taskIndex })}</span>
                            {row.reviewerProgress && (
                              <span className="tabular-nums">
                                {t("gateProgress", row.reviewerProgress)}
                              </span>
                            )}
                          </div>
                        )}

                        <div className="space-y-1">
                          {primary.map((node) =>
                            renderNodeRow(node, lane.kind)
                          )}
                        </div>

                        {reviewers.length > 0 && (
                          <div
                            className="ms-6 space-y-1 border-s border-border/60 ps-3"
                            data-testid={
                              taskRow
                                ? `workflow-task-reviewers-${row.taskIndex}`
                                : undefined
                            }
                            aria-label={t("reviewerCohort")}
                          >
                            {reviewers.map((node) =>
                              renderNodeRow(
                                node,
                                lane.kind,
                                taskRow
                                  ? `workflow-task-reviewer-node-${node.node_id}`
                                  : undefined
                              )
                            )}
                          </div>
                        )}
                      </li>
                    )
                  })}
                </ul>
              )}
            </section>
          )
        })}
      </div>

      {snapshot.edges.length > 0 && (
        <div
          className="rounded-md border border-dashed px-2 py-1 text-[10px] text-muted-foreground"
          data-testid="workflow-graph-edges"
        >
          <button
            type="button"
            className="flex w-full items-center justify-between gap-2 py-0.5 text-start font-medium hover:text-foreground"
            data-testid="workflow-dependencies-toggle"
            aria-expanded={dependenciesExpanded}
            onClick={() => setDependenciesExpanded((value) => !value)}
          >
            <span>
              {t("dependenciesToggle", { count: snapshot.edges.length })}
            </span>
            <ChevronDownIcon
              className={cn(
                "size-3.5 shrink-0 transition-transform",
                !dependenciesExpanded && "-rotate-90"
              )}
              aria-hidden
            />
          </button>
          {dependenciesExpanded && (
            <ul className="mt-1 space-y-1">
              {snapshot.edges.map((edge, i) => {
                const fromTitle = nodeTitles.get(edge.from) ?? edge.from
                const toTitle = nodeTitles.get(edge.to) ?? edge.to
                return (
                  <li
                    key={edge.id ?? `${edge.from}->${edge.to}-${i}`}
                    className="flex min-w-0 items-center gap-1.5"
                  >
                    <span className="min-w-0 truncate rounded border bg-background/60 px-1.5 py-0.5 text-foreground">
                      {fromTitle}
                    </span>
                    <ArrowRightIcon
                      data-testid="workflow-dependency-arrow"
                      className="size-3 shrink-0 rtl:rotate-180"
                      aria-hidden
                    />
                    <span className="min-w-0 truncate rounded border bg-background/60 px-1.5 py-0.5 text-foreground">
                      {toTitle}
                    </span>
                  </li>
                )
              })}
            </ul>
          )}
        </div>
      )}
    </div>
  )
})
