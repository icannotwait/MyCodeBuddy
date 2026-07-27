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
    return PHASE_ORDER.map((kind) => {
      const phaseMeta =
        snapshot.phases.find((p) => p.kind === kind || p.id === kind) ?? null
      const nodes = snapshot.nodes.filter((n) => {
        const pid = n.phase_id
        if (!pid) return false
        if (pid === kind) return true
        if (phaseMeta && pid === phaseMeta.id) return true
        const meta = snapshot.phases.find((p) => p.id === pid)
        return meta?.kind === kind
      })
      // Stable: task_index then role then node_id
      nodes.sort((a, b) => {
        const ta = a.task_index ?? 9999
        const tb = b.task_index ?? 9999
        if (ta !== tb) return ta - tb
        const ra = a.role ?? ""
        const rb = b.role ?? ""
        if (ra !== rb) return ra.localeCompare(rb)
        return a.node_id.localeCompare(b.node_id)
      })
      return { kind, title: phaseMeta?.title ?? null, nodes }
    })
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
            {lane.nodes.length === 0 ? (
              <p className="px-1 text-[10px] text-muted-foreground">
                {t("emptyLane")}
              </p>
            ) : (
              <ul className="space-y-1">
                {lane.nodes.map((node) => {
                  const estimated = isEstimatedNode(node)
                  const openable = canOpenWorkflowNode(node)
                  const selectedNode = node.node_id === selectedId
                  const accessibleName = [
                    t(`phase.${lane.kind}`),
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
                    <li key={node.node_id}>
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
                          "flex w-full min-w-0 flex-col gap-0.5 rounded border px-1.5 py-1 text-left text-[11px] transition-colors",
                          selectedNode
                            ? "border-primary bg-primary/10"
                            : "border-border/70 bg-background/60 hover:bg-muted/50",
                          estimated &&
                            "border-dashed text-muted-foreground opacity-80",
                          !openable && estimated && "cursor-default"
                        )}
                      >
                        <span className="truncate font-medium">
                          {node.title?.trim() || node.node_id}
                        </span>
                        <span className="flex flex-wrap items-center gap-1">
                          <Badge
                            variant="secondary"
                            className="h-4 px-1 text-[9px]"
                          >
                            {t(`nodeStatus.${node.status}`)}
                          </Badge>
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
                          {!node.required && (
                            <span className="text-muted-foreground">
                              {t("optionalReviewer")}
                            </span>
                          )}
                        </span>
                      </button>
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
