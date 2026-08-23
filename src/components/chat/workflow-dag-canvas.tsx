"use client"

import {
  type ReactElement,
  type RefObject,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react"
import { AlertTriangleIcon } from "lucide-react"
import { useLocale, useTranslations } from "next-intl"

import { WorkflowStatusIcon } from "@/components/chat/workflow-status-icon"
import { getAgentLabel } from "@/lib/custom-agents"
import { layoutWorkflowDag, type LaidOutNode } from "@/lib/workflow-dag-layout"
import { isEstimatedNode } from "@/lib/workflow-graph-store"
import type { WorkflowEdgeSnapshot, WorkflowNodeSnapshot } from "@/lib/types"
import { cn } from "@/lib/utils"

export interface WorkflowDagCanvasProps {
  nodes: readonly WorkflowNodeSnapshot[]
  edges: readonly WorkflowEdgeSnapshot[]
  currentNodeIds: readonly string[]
  selectedNodeId: string | null
  detailId: string
  nodeDisplayTitle: (node: WorkflowNodeSnapshot) => string
  onSelect: (nodeId: string) => void
}

function sanitizeId(value: string): string {
  return value.replace(/[^A-Za-z0-9_-]/g, "") || "workflow-dag"
}

function useMeasuredWidth(ref: RefObject<HTMLDivElement | null>): number {
  const [width, setWidth] = useState(0)

  useLayoutEffect(() => {
    const element = ref.current
    if (!element) return
    const publish = (nextWidth: number) => {
      setWidth(Number.isFinite(nextWidth) && nextWidth > 0 ? nextWidth : 0)
    }
    publish(element.getBoundingClientRect().width)

    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver((entries) => {
        const entry = entries.find((candidate) => candidate.target === element)
        if (entry) publish(entry.contentRect.width)
      })
      observer.observe(element)
      return () => observer.disconnect()
    }

    const onWindowResize = () => publish(element.getBoundingClientRect().width)
    window.addEventListener("resize", onWindowResize)
    return () => window.removeEventListener("resize", onWindowResize)
  }, [ref])

  return width
}

export function WorkflowDagCanvas({
  nodes,
  edges,
  currentNodeIds,
  selectedNodeId,
  detailId,
  nodeDisplayTitle,
  onSelect,
}: WorkflowDagCanvasProps): ReactElement {
  const t = useTranslations("Folder.chat.workflowGraph")
  const locale = useLocale()
  const direction = locale === "ar" ? "rtl" : "ltr"
  const rootRef = useRef<HTMLDivElement>(null)
  const viewportWidth = useMeasuredWidth(rootRef)
  const idPrefix = sanitizeId(useId())
  const markerId = `${idPrefix}-arrow`
  const currentIds = useMemo(() => new Set(currentNodeIds), [currentNodeIds])
  const titles = useMemo(
    () => new Map(nodes.map((node) => [node.node_id, nodeDisplayTitle(node)])),
    [nodeDisplayTitle, nodes]
  )
  const relationships = useMemo(() => {
    const incoming = new Map<string, string[]>()
    const outgoing = new Map<string, string[]>()
    for (const node of nodes) {
      incoming.set(node.node_id, [])
      outgoing.set(node.node_id, [])
    }
    for (const edge of edges) {
      const fromTitle = titles.get(edge.from)
      const toTitle = titles.get(edge.to)
      if (fromTitle && toTitle) {
        outgoing.get(edge.from)?.push(toTitle)
        incoming.get(edge.to)?.push(fromTitle)
      }
    }
    return { incoming, outgoing }
  }, [edges, nodes, titles])
  const layout = useMemo(
    () =>
      viewportWidth > 0
        ? layoutWorkflowDag({ nodes, edges, viewportWidth, direction })
        : null,
    [direction, edges, nodes, viewportWidth]
  )

  const renderNode = (
    node: WorkflowNodeSnapshot,
    nodeIndex: number,
    position: LaidOutNode | null,
    interactive: boolean,
    describeRelationships: boolean
  ) => {
    const title = nodeDisplayTitle(node)
    const selected = interactive && node.node_id === selectedNodeId
    const current = interactive && currentIds.has(node.node_id)
    const estimated = isEstimatedNode(node)
    const agent = node.agent_type
      ? (getAgentLabel(node.agent_type) ?? node.agent_type)
      : null
    const identity = node.role
      ? t("roleLabel", { role: node.role })
      : agent
        ? t("agentLabel", { agent })
        : node.task_index != null
          ? t("taskIndex", { index: node.task_index })
          : title
    const incoming = relationships.incoming.get(node.node_id) ?? []
    const outgoing = relationships.outgoing.get(node.node_id) ?? []
    const relationshipText = describeRelationships
      ? ([
          incoming.length > 0
            ? t("dagDependsOn", { nodes: incoming.join(" · ") })
            : null,
          outgoing.length > 0
            ? t("dagRequiredBy", { nodes: outgoing.join(" · ") })
            : null,
        ].filter(Boolean) as string[])
      : []
    const descriptionId = `${idPrefix}-relationships-${nodeIndex}`
    const accessibleName = [
      identity,
      t(`nodeStatus.${node.status}`),
      title,
      current ? t("dagCurrentNode") : null,
      node.sync_state === "out_of_sync" ? t("simpleOutOfSync") : null,
    ]
      .filter(Boolean)
      .join(", ")
    const className = cn(
      "h-12 min-w-0 overflow-hidden rounded-lg border bg-card px-2 py-1 text-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
      position && "absolute",
      !position && "w-full",
      estimated && "border-dashed text-muted-foreground opacity-80",
      current && "border-s-2 border-s-blue-500",
      selected && "inset-ring-2 inset-ring-blue-500",
      node.status === "blocked" && "border-destructive/70"
    )
    const style = position
      ? {
          left: position.x,
          top: position.y,
          width: position.width,
          height: position.height,
        }
      : undefined
    const content = (
      <>
        <span className="flex min-w-0 items-center gap-1 text-[10px]">
          <WorkflowStatusIcon visualStatus={node.status} className="size-3.5" />
          <span className="min-w-0 flex-1 truncate font-medium">
            {identity}
          </span>
          <span className="max-w-[45%] min-w-0 truncate text-muted-foreground">
            {t(`nodeStatus.${node.status}`)}
          </span>
          {node.sync_state === "out_of_sync" && (
            <AlertTriangleIcon
              className="size-3 shrink-0 text-amber-600 dark:text-amber-400"
              aria-hidden
            />
          )}
        </span>
        <span
          className="block truncate text-[11px] text-muted-foreground"
          dir="auto"
        >
          {title}
        </span>
        {current && <span className="sr-only">{t("dagCurrentNode")}</span>}
        {node.sync_state === "out_of_sync" && (
          <span className="sr-only">{t("simpleOutOfSync")}</span>
        )}
        {relationshipText.length > 0 && (
          <span id={descriptionId} className="sr-only">
            {relationshipText.join(". ")}
          </span>
        )}
      </>
    )

    if (!interactive) {
      return (
        <div
          key={nodeIndex}
          className={className}
          data-status={node.status}
          data-estimated={estimated ? "true" : "false"}
          data-current={current ? "true" : "false"}
          data-selected="false"
          data-sync-state={node.sync_state}
          title={title}
        >
          {content}
        </div>
      )
    }
    return (
      <button
        key={node.node_id}
        type="button"
        className={className}
        style={style}
        data-testid={`workflow-dag-node-${node.node_id}`}
        data-status={node.status}
        data-estimated={estimated ? "true" : "false"}
        data-current={current ? "true" : "false"}
        data-selected={selected ? "true" : "false"}
        data-sync-state={node.sync_state}
        aria-label={accessibleName}
        aria-describedby={
          relationshipText.length > 0 ? descriptionId : undefined
        }
        aria-pressed={selected}
        aria-controls={selected ? detailId : undefined}
        title={title}
        onClick={() => onSelect(node.node_id)}
      >
        {content}
      </button>
    )
  }

  const rootProps = {
    ref: rootRef,
    "data-testid": "workflow-dag-canvas",
    role: "group" as const,
    "aria-label": t("dagAria"),
    dir: direction,
  }

  if (viewportWidth <= 0 || layout == null) {
    return <div {...rootProps} aria-busy="true" className="min-w-0" />
  }

  if (!layout.ok) {
    const interactive =
      layout.error !== "duplicate_node" && layout.error !== "invalid_node_id"
    return (
      <div {...rootProps} className="min-w-0 space-y-2">
        <p
          role="status"
          data-testid="workflow-dag-error"
          data-layout-error={layout.error}
          className="border-s-2 border-amber-500 px-2 py-1 text-xs"
        >
          {t("dagInvalidGraph")}
        </p>
        <ul
          data-testid="workflow-dag-fallback"
          aria-label={t("dagFallbackAria")}
          className="space-y-1"
        >
          {nodes.map((node, nodeIndex) => (
            <li key={nodeIndex}>
              {renderNode(node, nodeIndex, null, interactive, false)}
            </li>
          ))}
        </ul>
      </div>
    )
  }

  return (
    <div
      {...rootProps}
      aria-busy="false"
      className="min-w-0 overflow-x-auto overflow-y-hidden"
    >
      <div
        className="relative"
        style={{ width: layout.canvasWidth, height: layout.height }}
      >
        <svg
          aria-hidden="true"
          className="pointer-events-none absolute inset-0"
          width={layout.canvasWidth}
          height={layout.height}
          viewBox={`0 0 ${layout.canvasWidth} ${layout.height}`}
          data-testid="workflow-dag-svg"
        >
          <defs>
            <marker
              id={markerId}
              aria-hidden="true"
              viewBox="0 0 8 8"
              refX="7"
              refY="4"
              markerWidth="6"
              markerHeight="6"
              orient="auto"
            >
              <path
                d="M 0 0 L 8 4 L 0 8 z"
                fill="currentColor"
                className="text-border"
              />
            </marker>
          </defs>
          {layout.edges.map((edge) => (
            <path
              key={edge.edgeIndex}
              d={edge.path}
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              markerEnd={`url(#${markerId})`}
              className="text-border"
              data-testid={`workflow-dag-edge-${edge.edgeIndex}`}
              data-from={edge.from}
              data-to={edge.to}
              data-edge-id={edge.edgeId ?? undefined}
            />
          ))}
        </svg>
        {layout.nodes.map((position) =>
          renderNode(
            nodes[position.nodeIndex],
            position.nodeIndex,
            position,
            true,
            true
          )
        )}
      </div>
    </div>
  )
}
