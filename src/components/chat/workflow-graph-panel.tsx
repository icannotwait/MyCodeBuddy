"use client"

/**
 * Expanded deterministic workflow graph (phase lanes + Task rows).
 * Not force-directed; observed nodes open via child-tab path; estimated
 * nodes are non-actionable.
 */

import { memo, useCallback, useMemo, useState } from "react"
import { useTranslations } from "next-intl"

import { WorkflowNodeDetail } from "@/components/chat/workflow-node-detail"
import { Badge } from "@/components/ui/badge"
import { openDelegatedChildSession } from "@/lib/open-delegated-child-session"
import {
  buildPhaseRail,
  canOpenWorkflowNode,
  isEstimatedNode,
  type PhaseRailKind,
} from "@/lib/workflow-graph-store"
import type {
  AgentType,
  WorkflowGraphSnapshot,
  WorkflowNodeSnapshot,
} from "@/lib/types"
import { cn } from "@/lib/utils"

const PHASE_ORDER: PhaseRailKind[] = ["design", "plan", "tasks", "final"]

interface WorkflowGraphPanelProps {
  snapshot: WorkflowGraphSnapshot
  className?: string
  /** When true, render denser (overlay expanded body). */
  compact?: boolean
}

export const WorkflowGraphPanel = memo(function WorkflowGraphPanel({
  snapshot,
  className,
  compact = false,
}: WorkflowGraphPanelProps) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const [selectedId, setSelectedId] = useState<string | null>(
    snapshot.current_node_ids[0] ?? snapshot.nodes[0]?.node_id ?? null
  )

  const selected = useMemo(
    () => snapshot.nodes.find((n) => n.node_id === selectedId) ?? null,
    [snapshot.nodes, selectedId]
  )

  const lanes = useMemo(() => {
    const byKind = new Map(
      buildPhaseRail(snapshot).map((lane) => [lane.kind, lane])
    )
    return PHASE_ORDER.map((kind) => byKind.get(kind)!)
  }, [snapshot])

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

  const renderNodeButton = (
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
        disabled={estimated && !openable}
        title={
          estimated
            ? t("estimatedNonActionable")
            : openable
              ? t("openSession")
              : undefined
        }
        onClick={() => onActivateNode(node)}
        className={cn(
          "flex h-12 w-full min-w-0 flex-col justify-center gap-0.5 overflow-hidden rounded border px-1.5 py-1 text-left text-[11px] transition-colors",
          selectedNode
            ? "border-primary bg-primary/10"
            : "border-border/70 bg-background/60 hover:bg-muted/50",
          estimated && "border-dashed text-muted-foreground opacity-80",
          !openable && estimated && "cursor-default"
        )}
      >
        <span className="w-full truncate font-medium">
          {node.title?.trim() || node.node_id}
        </span>
        <span className="flex w-full min-w-0 items-center gap-1 overflow-hidden">
          <Badge variant="secondary" className="h-4 shrink-0 px-1 text-[9px]">
            {t(`nodeStatus.${node.status}`)}
          </Badge>
          {node.run_count > 0 && (
            <span className="truncate tabular-nums text-muted-foreground">
              {t("runCount", { count: node.run_count })}
            </span>
          )}
          {node.replacement_count > 0 && (
            <span className="truncate tabular-nums text-muted-foreground">
              {t("replacementCount", {
                count: node.replacement_count,
              })}
            </span>
          )}
          {!node.required && (
            <span className="truncate text-muted-foreground">
              {t("optionalReviewer")}
            </span>
          )}
        </span>
      </button>
    )
  }

  return (
    <div
      className={cn("space-y-2", className)}
      data-testid="workflow-graph-panel"
      role="region"
      aria-label={t("graphTitle")}
    >
      <div
        className={cn(
          "grid gap-2",
          compact ? "grid-cols-1" : "grid-cols-1 sm:grid-cols-2 lg:grid-cols-4"
        )}
      >
        {lanes.map((lane) => (
          <section
            key={lane.kind}
            className="min-w-0 space-y-1.5 rounded-md border bg-card/40 p-1.5"
            data-testid={`workflow-graph-lane-${lane.kind}`}
            aria-label={t(`phase.${lane.kind}`)}
          >
            <header className="px-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
              {t(`phase.${lane.kind}`)}
            </header>
            {lane.nodeRows.length === 0 ? (
              <p className="px-1 text-[10px] text-muted-foreground">
                {t("emptyLane")}
              </p>
            ) : (
              <ul className="space-y-1">
                {lane.nodeRows.map((row) => {
                  const primary = row.nodes.filter(
                    (node) => node.role !== "reviewer"
                  )
                  const reviewers = row.nodes.filter(
                    (node) => node.role === "reviewer"
                  )
                  const taskRow = lane.kind === "tasks" && row.taskIndex != null
                  const rowTestId = taskRow
                    ? `workflow-graph-row-tasks-${row.taskIndex}`
                    : lane.kind === "plan" && row.id === "plan"
                      ? "workflow-graph-row-plan"
                      : undefined

                  return (
                    <li
                      key={row.id}
                      className="min-w-0 overflow-hidden"
                      data-testid={rowTestId}
                      data-reviewer-count={reviewers.length}
                    >
                      <div className="flex h-12 min-w-0 items-stretch gap-1">
                        {primary.length > 0 && (
                          <div
                            className="grid h-12 min-w-0 flex-1 gap-1"
                            style={{
                              gridTemplateColumns: `repeat(${primary.length}, minmax(0, 1fr))`,
                            }}
                          >
                            {primary.map((node) => (
                              <div key={node.node_id} className="min-w-0">
                                {renderNodeButton(node, lane.kind)}
                              </div>
                            ))}
                          </div>
                        )}
                        {reviewers.length > 0 && (
                          <div
                            className="grid h-12 min-w-0 flex-1 gap-1 border-l border-border/60 pl-1"
                            style={{
                              gridTemplateColumns: `repeat(${reviewers.length}, minmax(0, 1fr))`,
                            }}
                            data-testid={
                              taskRow
                                ? `workflow-task-reviewers-${row.taskIndex}`
                                : undefined
                            }
                            aria-label={t("reviewerCohort")}
                          >
                            {reviewers.map((node) => (
                              <div
                                key={node.node_id}
                                className="min-w-0"
                                data-testid={
                                  taskRow
                                    ? `workflow-task-reviewer-node-${node.node_id}`
                                    : undefined
                                }
                              >
                                {renderNodeButton(node, lane.kind)}
                              </div>
                            ))}
                          </div>
                        )}
                        {taskRow && row.reviewerProgress && (
                          <span
                            className="flex h-12 w-10 shrink-0 items-center justify-center rounded border border-border/70 bg-muted/30 text-[10px] tabular-nums text-muted-foreground"
                            data-testid={`workflow-task-reviewer-count-${row.taskIndex}`}
                          >
                            {t("gateProgress", row.reviewerProgress)}
                          </span>
                        )}
                      </div>
                    </li>
                  )
                })}
              </ul>
            )}
          </section>
        ))}
      </div>

      {snapshot.edges.length > 0 && (
        <div
          className="rounded-md border border-dashed px-2 py-1 text-[10px] text-muted-foreground"
          data-testid="workflow-graph-edges"
        >
          <div className="mb-0.5 font-medium">{t("dependencies")}</div>
          <ul className="space-y-0.5">
            {snapshot.edges.map((edge, i) => (
              <li
                key={edge.id ?? `${edge.from}->${edge.to}-${i}`}
                className="truncate font-mono"
              >
                {edge.from} → {edge.to}
              </li>
            ))}
          </ul>
        </div>
      )}

      {selected && (
        <WorkflowNodeDetail node={selected} onOpenSession={onOpenSession} />
      )}
    </div>
  )
})
