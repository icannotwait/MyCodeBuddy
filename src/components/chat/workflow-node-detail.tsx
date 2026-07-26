"use client"

/**
 * Selected-node detail for the expanded workflow graph.
 * Exposes B12 vocabulary separately: run_count, active_child_generation,
 * replacement_count, gate_cycle, round_count.
 */

import { memo } from "react"
import { useTranslations } from "next-intl"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  canOpenWorkflowNode,
  isEstimatedNode,
} from "@/lib/workflow-graph-store"
import type { WorkflowNodeSnapshot } from "@/lib/types"
import { cn } from "@/lib/utils"

interface WorkflowNodeDetailProps {
  node: WorkflowNodeSnapshot
  onOpenSession?: (node: WorkflowNodeSnapshot) => void
  className?: string
}

export const WorkflowNodeDetail = memo(function WorkflowNodeDetail({
  node,
  onOpenSession,
  className,
}: WorkflowNodeDetailProps) {
  const t = useTranslations("Folder.chat.workflowGraph")
  const openable = canOpenWorkflowNode(node)
  const estimated = isEstimatedNode(node)

  return (
    <div
      className={cn(
        "space-y-2 rounded-md border bg-muted/20 p-2 text-xs",
        className
      )}
      data-testid="workflow-node-detail"
      data-node-id={node.node_id}
    >
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="font-medium">
          {node.title?.trim() || node.node_id}
        </span>
        <Badge variant="secondary" className="h-5">
          {t(`nodeStatus.${node.status}`)}
        </Badge>
        {node.role && (
          <Badge variant="outline" className="h-5">
            {t("roleLabel", { role: node.role })}
          </Badge>
        )}
        {node.agent_type && (
          <Badge variant="outline" className="h-5">
            {t("agentLabel", { agent: node.agent_type })}
          </Badge>
        )}
        {!node.required && (
          <Badge variant="outline" className="h-5">
            {t("optionalReviewer")}
          </Badge>
        )}
      </div>

      {node.summary && (
        <p className="text-muted-foreground line-clamp-3">{node.summary}</p>
      )}

      {/* B12 field vocabulary — separate labels, never collapsed into one counter */}
      <dl
        className="grid grid-cols-2 gap-x-2 gap-y-1 tabular-nums"
        data-testid="workflow-node-b12"
      >
        <div>
          <dt className="text-muted-foreground">{t("runCountLabel")}</dt>
          <dd data-testid="workflow-node-run-count">{node.run_count}</dd>
        </div>
        <div>
          <dt className="text-muted-foreground">
            {t("activeGenerationLabel")}
          </dt>
          <dd data-testid="workflow-node-active-generation">
            {node.active_child_generation ?? "—"}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">
            {t("replacementCountLabel")}
          </dt>
          <dd data-testid="workflow-node-replacement-count">
            {node.replacement_count}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">{t("gateCycleLabel")}</dt>
          <dd data-testid="workflow-node-gate-cycle">
            {node.gate_cycle ?? "—"}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">{t("roundCountLabel")}</dt>
          <dd data-testid="workflow-node-round-count">
            {node.round_count ?? "—"}
          </dd>
        </div>
      </dl>

      {estimated ? (
        <p
          className="text-muted-foreground"
          data-testid="workflow-node-estimated-hint"
        >
          {t("estimatedNonActionable")}
        </p>
      ) : openable ? (
        <Button
          type="button"
          size="sm"
          variant="secondary"
          className="h-7"
          data-testid="workflow-node-open-session"
          onClick={() => onOpenSession?.(node)}
        >
          {t("openSession")}
        </Button>
      ) : null}
    </div>
  )
})
