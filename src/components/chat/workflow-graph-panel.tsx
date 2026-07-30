"use client"

/**
 * Expanded workflow graph: collapsible phase lanes with adaptive node rows.
 * Observed nodes open via child-tab path; estimated nodes are non-actionable.
 */

import { memo, useCallback, useMemo, useState } from "react"
import { ArrowRightIcon, ChevronDownIcon } from "lucide-react"
import { useTranslations } from "next-intl"

import { WorkflowNodeDetail } from "@/components/chat/workflow-node-detail"
import { WorkflowStatusIcon } from "@/components/chat/workflow-status-icon"
import { phaseProgressFragments } from "@/components/chat/workflow-phase-rail"
import { Badge } from "@/components/ui/badge"
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
  WorkflowGraphSnapshot,
  WorkflowNodeSnapshot,
} from "@/lib/types"
import { cn } from "@/lib/utils"

const PHASE_ORDER: PhaseRailKind[] = ["design", "plan", "tasks", "final"]

type LaneBooleanMap = Record<PhaseRailKind, boolean>

interface WorkflowGraphPanelProps {
  snapshot: WorkflowGraphSnapshot
  className?: string
}

function laneDefaults(phases: readonly PhaseRailItem[]): LaneBooleanMap {
  return Object.fromEntries(
    phases.map((phase) => [phase.kind, phase.nodeRows.length > 0])
  ) as LaneBooleanMap
}

export const WorkflowGraphPanel = memo(function WorkflowGraphPanel({
  snapshot,
  className,
}: WorkflowGraphPanelProps) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const [selectedId, setSelectedId] = useState<string | null>(
    snapshot.current_node_ids[0] ?? snapshot.nodes[0]?.node_id ?? null
  )
  const [dependenciesExpanded, setDependenciesExpanded] = useState(false)

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
        snapshot.nodes.map((node) => [
          node.node_id,
          node.title?.trim() || node.node_id,
        ])
      ),
    [snapshot.nodes]
  )

  const onOpenSession = useCallback(async (node: WorkflowNodeSnapshot) => {
    if (!canOpenWorkflowNode(node)) return
    await openDelegatedChildSession({
      childConversationId: node.latest_child_conversation_id,
      agentType: (node.agent_type as AgentType | null) ?? null,
      title: node.title,
    })
  }, [])

  const onActivateNode = useCallback(
    (node: WorkflowNodeSnapshot) => {
      setSelectedId(node.node_id)
      if (canOpenWorkflowNode(node)) {
        void onOpenSession(node)
      }
    },
    [onOpenSession]
  )

  const renderNodeControl = (
    node: WorkflowNodeSnapshot,
    laneKind: PhaseRailKind
  ) => {
    const estimated = isEstimatedNode(node)
    const openable = canOpenWorkflowNode(node)
    const selectedNode = node.node_id === selectedId
    const accessibleName = [
      t(`phase.${laneKind}`),
      node.task_index != null
        ? t("taskIndex", { index: node.task_index })
        : null,
      node.role,
      node.agent_type,
      t(`nodeStatus.${node.status}`),
      node.title,
    ]
      .filter(Boolean)
      .join(", ")

    return (
      <button
        type="button"
        data-testid={`workflow-graph-node-${node.node_id}`}
        data-status={node.status}
        data-estimated={estimated ? "true" : "false"}
        data-openable={openable ? "true" : "false"}
        aria-label={accessibleName}
        aria-current={selectedNode ? "true" : undefined}
        aria-disabled={estimated ? "true" : undefined}
        disabled={estimated}
        title={
          estimated
            ? t("estimatedNonActionable")
            : openable
              ? t("openSession")
              : undefined
        }
        onClick={() => {
          if (estimated) return
          onActivateNode(node)
        }}
        className={cn(
          "flex h-auto w-full min-w-0 flex-col justify-center gap-0.5 rounded border px-1.5 py-1.5 text-start text-[11px] transition-colors",
          selectedNode
            ? "border-primary bg-primary/10"
            : "border-border/70 bg-background/60 hover:bg-muted/50",
          estimated &&
            "cursor-default border-dashed text-muted-foreground opacity-80"
        )}
      >
        <span className="flex w-full min-w-0 items-start gap-1.5">
          <WorkflowStatusIcon
            visualStatus={node.status}
            className="mt-0.5 size-3.5"
          />
          <span
            data-node-title
            className="min-w-0 flex-1 line-clamp-2 font-medium"
          >
            {node.title?.trim() || node.node_id}
          </span>
          <Badge variant="secondary" className="h-4 shrink-0 px-1 text-[9px]">
            {t(`nodeStatus.${node.status}`)}
          </Badge>
        </span>
        <span className="flex w-full min-w-0 flex-wrap items-center gap-1 ps-5">
          {node.role && (
            <Badge variant="outline" className="h-4 px-1 text-[9px]">
              {t("roleLabel", { role: node.role })}
            </Badge>
          )}
          {node.agent_type && (
            <Badge variant="outline" className="h-4 px-1 text-[9px]">
              {t("agentLabel", { agent: node.agent_type })}
            </Badge>
          )}
          {!node.required && (
            <span className="text-muted-foreground">
              {t("optionalReviewer")}
            </span>
          )}
          {node.run_count > 0 && (
            <span className="tabular-nums text-muted-foreground">
              {t("runCount", { count: node.run_count })}
            </span>
          )}
          {node.replacement_count > 0 && (
            <span className="tabular-nums text-muted-foreground">
              {t("replacementCount", {
                count: node.replacement_count,
              })}
            </span>
          )}
        </span>
      </button>
    )
  }

  const renderNodeWithDetail = (
    node: WorkflowNodeSnapshot,
    laneKind: PhaseRailKind,
    reviewerWrapperTestId?: string
  ) => (
    <div
      key={node.node_id}
      data-testid={reviewerWrapperTestId}
      className="min-w-0"
    >
      {renderNodeControl(node, laneKind)}
      {selectedId === node.node_id && expandedByLane[laneKind] && (
        <WorkflowNodeDetail
          node={node}
          onOpenSession={onOpenSession}
          className="mt-1"
        />
      )}
    </div>
  )

  return (
    <div
      className={cn("space-y-2", className)}
      data-testid="workflow-graph-panel"
      role="region"
      aria-label={t("graphTitle")}
    >
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
                <ChevronDownIcon
                  className={cn(
                    "size-3.5 shrink-0 text-muted-foreground transition-transform",
                    !expanded && "-rotate-90"
                  )}
                  aria-hidden
                />
              </button>

              {!expanded && lane.nodeRows.length === 0 && (
                <p className="px-1 text-[10px] text-muted-foreground">
                  {t("emptyLane")}
                </p>
              )}

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
                    const taskRow =
                      lane.kind === "tasks" && row.taskIndex != null

                    return (
                      <li key={row.id} className="min-w-0 space-y-1">
                        {taskRow && (
                          <div
                            className="flex items-center gap-2 px-1 text-[10px] font-medium text-muted-foreground"
                            data-testid={`workflow-task-reviewer-count-${row.taskIndex}`}
                          >
                            <span>
                              {t("taskIndex", { index: row.taskIndex })}
                            </span>
                            {row.reviewerProgress && (
                              <span className="tabular-nums">
                                {t("gateProgress", row.reviewerProgress)}
                              </span>
                            )}
                          </div>
                        )}

                        <div className="space-y-1">
                          {primary.map((node) =>
                            renderNodeWithDetail(node, lane.kind)
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
                              renderNodeWithDetail(
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
