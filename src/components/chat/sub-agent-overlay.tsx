"use client"

/**
 * Inline-start overlay listing sub-agents delegated across the conversation.
 *
 * Mirrors `AgentPlanOverlay` (the "计划任务" panel): collapses to a bullet chip,
 * expands to a card, remembers collapse state per `overlayKey`, and renders
 * nothing when there's nothing to show. Positioning (absolute inline-start/top) is
 * owned by the shared overlay-stack container in `MessageListView`, which
 * places this panel BELOW the plan panel when both are present.
 *
 * Codeg rows resolve agent type / task / status / child ids from the same
 * `useDelegationCardModel` the inline `DelegatedSubThread` card uses, so the
 * overlay and the message-stream card never disagree. Clicking a Codeg row opens
 * the child's full conversation in a main tab via `openDelegatedChildSession`
 * ("查看会话").
 *
 * Native rows are informational only (`authoritative=false`): origin label +
 * timestamps, no Broker cancel action, no open-session control unless
 * Codeg-backed.
 *
 * Defaults to expanded so historical sub-agents are visible without an extra
 * click; the parent supplies the full conversation's delegation list.
 */

import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type PointerEvent as ReactPointerEvent,
} from "react"
import { useTranslations } from "next-intl"
import {
  BotIcon,
  ChevronDownIcon,
  Eye,
  GitBranch,
  Maximize2,
  Minimize2,
} from "lucide-react"

import { AgentIcon } from "@/components/agent-icon"
import { CollapsedOverlayChip } from "@/components/chat/collapsed-overlay-chip"
import { WorkflowGraphPanel } from "@/components/chat/workflow-graph-panel"
import { WorkflowPhaseRail } from "@/components/chat/workflow-phase-rail"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { DelegationCardChrome } from "@/components/message/delegation-card-chrome"
import { StatusBadge } from "@/components/message/delegation-status-badge"
import {
  useDelegationCardModel,
  type DelegationCardSource,
} from "@/hooks/use-delegation-card-model"
import {
  SUB_AGENT_OVERLAY_SIZE_KEY,
  clampOverlayWidth,
  defaultOverlaySize,
  loadOverlaySize,
  nextOverlayMaxHeight,
  saveOverlaySize,
  type OverlaySize,
} from "@/lib/overlay-size-storage"
import { openDelegatedChildSession } from "@/lib/open-delegated-child-session"
import { getAgentLabel } from "@/lib/custom-agents"
import {
  type DelegationActivityView,
  type WorkflowGraphSnapshot,
} from "@/lib/types"
import {
  isUncorrelatedDelegationFailure,
  parseDelegateRunIdentity,
  parseDelegateTaskId,
  parseDelegationMeta,
  parseToolOutput,
} from "@/lib/delegation-card"
import { delegationRunSnapshotCache } from "@/lib/delegation-run-snapshot"
import { groupDelegationRuns } from "@/lib/delegation-work-unit"
import {
  buildPhaseRail,
  selectCurrentNodes,
  useWorkflowGraphStore,
  type WorkflowSegment,
} from "@/lib/workflow-graph-store"
import { cn } from "@/lib/utils"

interface SubAgentOverlayProps {
  /** All `delegate_to_agent` tool calls in this conversation (timeline order). */
  delegations: DelegationCardSource[]
  /**
   * Read-only activity projection (Codeg + native). Native rows are
   * informational only — no Broker cancel. When empty/omitted, only
   * `delegations` drive the overlay (legacy Codeg path).
   */
  activities?: DelegationActivityView[]
  /** Stable key for collapse/expand state (typically per-conversation). The
   *  parent also remounts via `key` on conversation change so state resets
   *  across sessions but is retained while browsing the same thread. */
  overlayKey?: string | null
  /** Expanded by default so the full sub-agent history is visible. */
  defaultExpanded?: boolean
  /**
   * Parent conversation id — seeds the workflow graph store and, only when the
   * overlay is expanded on the workflow segment with the full graph open,
   * acquires active expanded-graph refresh interest.
   */
  conversationId?: number | null
  /**
   * Optional cold-detail seed. Store remains source of truth once seeded for
   * `conversationId`. A13: presence mounts overlay even with zero sessions.
   * Detail reseeds do not reinstall active refresh interest.
   */
  workflowGraph?: WorkflowGraphSnapshot | null
}

function formatActivityTime(iso?: string): string | null {
  if (!iso) return null
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return null
  return d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  })
}

function observedToBadgeStatus(
  status: DelegationActivityView["observed_status"]
): "starting" | "running" | "ok" | "err" | "checked" | "waiting" {
  switch (status) {
    case "running":
      return "running"
    case "completed":
      return "ok"
    case "failed":
      return "err"
    case "canceled":
      // Informational only — not a Broker failure; avoid destructive "err" chrome.
      return "waiting"
    default:
      return "checked"
  }
}

type ResizeAxis = "x" | "y" | "both"

type DelegationOverlayGroup = {
  key: string
  childConversationId: number | null
  latestSource: DelegationCardSource
  sources: DelegationCardSource[]
  latestIndex: number
  latestGeneration: number | null
  runCount: number
  isReplacement: boolean
}

type OverlayRunValue = {
  source: DelegationCardSource
  index: number
  generation: number | null
  replacement: boolean
}

function rawDelegationMeta(
  source: DelegationCardSource
): Record<string, unknown> | null {
  const value = source.meta?.["codeg.delegation"]
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function sourceReplacementMarker(source: DelegationCardSource): boolean {
  const meta = rawDelegationMeta(source)
  if (typeof meta?.replaced_task_id === "string" && meta.replaced_task_id) {
    return true
  }
  try {
    const input = JSON.parse(source.input ?? "") as Record<string, unknown>
    return (
      typeof input.replaces_task_id === "string" && !!input.replaces_task_id
    )
  } catch {
    return false
  }
}

function sourceBrokerTaskId(
  source: DelegationCardSource,
  meta: Record<string, unknown> | null
): string | null {
  return (
    (typeof meta?.task_id === "string" && meta.task_id) ||
    parseDelegateTaskId(source.output, source.errorText)
  )
}

function sourceSnapshot(source: DelegationCardSource, taskId: string | null) {
  if (source.parentConversationId == null || !taskId) return null
  return delegationRunSnapshotCache.get(source.parentConversationId, taskId)
}

function sourceExplicitUserCancel(source: DelegationCardSource): boolean {
  const parsedMeta = parseDelegationMeta(source.meta)
  if (parsedMeta?.errorCode === "user_cancelled") return true
  const output =
    (source.errorText ? parseToolOutput(source.errorText, true) : null) ??
    parseToolOutput(source.output)
  return output?.kind === "outcome" && output.errorCode === "user_cancelled"
}

function structuredIdentityInput(
  input: string | null | undefined
): string | null | undefined {
  if (!input) return input
  try {
    JSON.parse(input)
    return input
  } catch {
    // parseInput would return an empty identity after logging; the row model
    // owns that diagnostic, so grouping skips the duplicate parse.
    return null
  }
}

/** Group overlay rows with the same work-unit identity rules as history. */
export function groupDelegationSourcesForOverlay(
  delegations: DelegationCardSource[]
): DelegationOverlayGroup[] {
  const records = delegations.map((source, index) => {
    const meta = rawDelegationMeta(source)
    const taskId = sourceBrokerTaskId(source, meta)
    const snapshot = sourceSnapshot(source, taskId)
    const toolOutput =
      (source.errorText ? parseToolOutput(source.errorText, true) : null) ??
      parseToolOutput(source.output ?? null)
    const uncorrelatedFailure = isUncorrelatedDelegationFailure(
      toolOutput,
      taskId
    )
    const parsedIdentity = parseDelegateRunIdentity({
      parentConversationId: source.parentConversationId ?? 0,
      parentToolUseId: source.parentToolUseId,
      input: structuredIdentityInput(source.input),
      output: source.output,
      errorText: source.errorText,
      meta: source.meta,
    })
    const linkedTaskIds = new Set(parsedIdentity.linkedTaskIds)
    for (const linked of [
      snapshot?.previous_task_id,
      snapshot?.root_task_id,
      snapshot?.replaced_task_id,
    ]) {
      if (linked && linked !== (snapshot?.task_id ?? parsedIdentity.taskId)) {
        linkedTaskIds.add(linked)
      }
    }
    const generation =
      typeof meta?.generation === "number" && Number.isInteger(meta.generation)
        ? meta.generation
        : (snapshot?.generation ?? null)
    const replacement =
      sourceReplacementMarker(source) || Boolean(snapshot?.replaced_task_id)
    return {
      value: { source, index, generation, replacement },
      identity: uncorrelatedFailure
        ? {
            ...parsedIdentity,
            workUnitKey: null,
            taskId: null,
            childConversationId: null,
            linkedTaskIds: [],
          }
        : {
            ...parsedIdentity,
            taskId: snapshot?.task_id ?? parsedIdentity.taskId,
            childConversationId:
              parsedIdentity.childConversationId ??
              snapshot?.child_conversation_id ??
              null,
            linkedTaskIds: [...linkedTaskIds],
          },
    }
  })

  return groupDelegationRuns<OverlayRunValue>(records).units.map((unit) => {
    let latest = unit.runs[0].value
    for (const run of unit.runs.slice(1)) {
      const candidate = run.value
      const isNewerGeneration =
        candidate.generation != null &&
        (latest.generation == null || candidate.generation >= latest.generation)
      if (
        isNewerGeneration ||
        (candidate.generation == null && candidate.index > latest.index)
      ) {
        latest = candidate
      }
    }
    const onlyIdentity = unit.runs.length === 1 ? unit.runs[0].identity : null
    const key =
      onlyIdentity &&
      !onlyIdentity.workUnitKey &&
      onlyIdentity.childConversationId == null &&
      !onlyIdentity.taskId &&
      onlyIdentity.linkedTaskIds.length === 0
        ? `source:${onlyIdentity.parentToolUseId}`
        : unit.key

    return {
      key,
      childConversationId:
        unit.runs.find((run) => run.value === latest)?.identity
          .childConversationId ?? null,
      latestSource: latest.source,
      sources: unit.runs.map((run) => run.value.source),
      latestIndex: latest.index,
      latestGeneration: latest.generation,
      runCount: unit.runs.length,
      isReplacement: unit.runs.some((run) => run.value.replacement),
    }
  })
}

export const SubAgentOverlay = memo(function SubAgentOverlay({
  delegations,
  activities = [],
  overlayKey,
  defaultExpanded = true,
  conversationId = null,
  workflowGraph = null,
}: SubAgentOverlayProps) {
  const t = useTranslations("Folder.chat.subAgentOverlay")
  const tw = useTranslations("Folder.chat.workflowGraph")
  const stateKey = overlayKey ?? "__subagents__default__"
  const [collapsedByKey, setCollapsedByKey] = useState<Record<string, boolean>>(
    {}
  )
  const [size, setSize] = useState<OverlaySize>(defaultOverlaySize)
  const [segment, setSegment] = useState<WorkflowSegment | null>(null)
  const [graphExpanded, setGraphExpanded] = useState(false)
  const listRef = useRef<HTMLDivElement | null>(null)
  const sizeRef = useRef(size)

  const storeSnapshot = useWorkflowGraphStore((s) =>
    conversationId != null && conversationId > 0
      ? (s.byConversationId.get(conversationId)?.snapshot ?? null)
      : null
  )
  const graph: WorkflowGraphSnapshot | null =
    storeSnapshot ?? workflowGraph ?? null
  const hasGraph = graph != null
  const activeSegment: WorkflowSegment =
    segment ?? (hasGraph ? "workflow" : "sessions")
  const userCollapsed = collapsedByKey[stateKey]
  const isExpanded =
    userCollapsed !== undefined ? !userCollapsed : defaultExpanded
  const overlayInterestActive =
    conversationId != null && conversationId > 0 && isExpanded
  const expandedGraphInterestActive =
    overlayInterestActive && activeSegment === "workflow" && graphExpanded

  // Detail seed only — never installs listener/activation cleanup.
  useEffect(() => {
    if (conversationId == null || conversationId <= 0) return
    if (workflowGraph !== undefined) {
      useWorkflowGraphStore
        .getState()
        .applyFromDetail(conversationId, workflowGraph)
    }
  }, [conversationId, workflowGraph])

  // Open-overlay interest discovers the first graph and receives live nudges.
  useEffect(() => {
    if (!overlayInterestActive || conversationId == null) return
    return useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(conversationId)
  }, [conversationId, overlayInterestActive])

  // Full-graph interest adds immediate refresh and fallback timer ownership.
  useEffect(() => {
    if (!expandedGraphInterestActive || conversationId == null) return
    return useWorkflowGraphStore.getState().activateConversation(conversationId)
  }, [conversationId, expandedGraphInterestActive])

  useEffect(() => {
    sizeRef.current = size
  }, [size])

  // Hydrate after the first paint so SSR/client markup stays default-stable.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setSize(loadOverlaySize(SUB_AGENT_OVERLAY_SIZE_KEY))
    }, 0)
    return () => window.clearTimeout(timer)
  }, [])

  const beginResize = useCallback(
    (axis: ResizeAxis, event: ReactPointerEvent<HTMLElement>) => {
      event.preventDefault()
      event.stopPropagation()

      const startX = event.clientX
      const startY = event.clientY
      const startSize = sizeRef.current
      const contentHeight = listRef.current?.scrollHeight ?? 0

      const onMove = (ev: PointerEvent) => {
        const next: OverlaySize = { ...sizeRef.current }

        if (axis === "x" || axis === "both") {
          next.width = clampOverlayWidth(
            startSize.width + (ev.clientX - startX)
          )
        }
        if (axis === "y" || axis === "both") {
          // Re-read content height while dragging so growth stays honest as
          // more rows become visible under a rising maxHeight.
          const liveContent = listRef.current?.scrollHeight ?? contentHeight
          next.maxHeight = nextOverlayMaxHeight({
            startMaxHeight: startSize.maxHeight,
            deltaY: ev.clientY - startY,
            contentHeight: liveContent,
          })
        }

        sizeRef.current = next
        setSize(next)
      }

      const onUp = () => {
        window.removeEventListener("pointermove", onMove)
        window.removeEventListener("pointerup", onUp)
        window.removeEventListener("pointercancel", onUp)
        saveOverlaySize(SUB_AGENT_OVERLAY_SIZE_KEY, sizeRef.current)
      }

      window.addEventListener("pointermove", onMove)
      window.addEventListener("pointerup", onUp)
      window.addEventListener("pointercancel", onUp)
    },
    []
  )

  const codegActivities = useMemo(
    () => activities.filter((a) => a.origin === "codeg"),
    [activities]
  )
  const nativeActivities = useMemo(
    () => activities.filter((a) => a.origin === "native"),
    [activities]
  )
  const snapshotRequests = useMemo(
    () =>
      delegations
        .map((source) => {
          const taskId = sourceBrokerTaskId(source, rawDelegationMeta(source))
          return source.parentConversationId != null && taskId
            ? { parentConversationId: source.parentConversationId, taskId }
            : null
        })
        .filter(
          (
            request
          ): request is { parentConversationId: number; taskId: string } =>
            request != null
        ),
    [delegations]
  )
  const snapshotVersion = useSyncExternalStore(
    (listener) => delegationRunSnapshotCache.subscribe(listener),
    () => delegationRunSnapshotCache.getVersion(),
    () => 0
  )
  useEffect(() => {
    for (const request of snapshotRequests) {
      delegationRunSnapshotCache.ensure(
        request.parentConversationId,
        request.taskId
      )
    }
  }, [snapshotRequests])
  const delegationGroups = useMemo(() => {
    // snapshotVersion is a cache-generation signal; groupDelegationSourcesForOverlay
    // reads cold snapshots from the external cache, so we must recompute when it changes.
    void snapshotVersion
    return groupDelegationSourcesForOverlay(delegations)
  }, [delegations, snapshotVersion])

  // Prefer full Codeg delegation-card rows (open-session control / existing
  // actions) when `delegations` is present. Fall back to activity views only
  // when the parent supplies Codeg activities without tool-call sources.
  // Native rows are always additive and informational.
  const showDelegationRows = delegations.length > 0
  const showCodegActivityRows =
    !showDelegationRows && codegActivities.length > 0
  const count =
    (showDelegationRows ? delegationGroups.length : 0) +
    (showCodegActivityRows ? codegActivities.length : 0) +
    nativeActivities.length

  const phaseRail = useMemo(() => (graph ? buildPhaseRail(graph) : []), [graph])
  const currentNodes = useMemo(
    () => (graph ? selectCurrentNodes(graph) : []),
    [graph]
  )

  // A13: mount when graph present even if session/activity count is zero.
  // Without a graph, keep today's null-when-empty behavior (no Workflow segment).
  if (!hasGraph && count === 0) {
    return null
  }

  if (!isExpanded) {
    const summary = hasGraph
      ? tw("collapsedSummary", {
          state: tw(`overallState.${graph!.overall_state}`),
        })
      : t("collapsedSummary", { count })
    return (
      <CollapsedOverlayChip
        icon={
          hasGraph ? (
            <GitBranch className="size-3" />
          ) : (
            <BotIcon className="size-3" />
          )
        }
        summary={summary}
        onClick={() =>
          setCollapsedByKey((prev) => ({ ...prev, [stateKey]: false }))
        }
      />
    )
  }

  const headerTitle = hasGraph ? tw("title") : t("title")
  const panelWidth =
    hasGraph && graphExpanded && activeSegment === "workflow"
      ? Math.max(size.width, 28 * 16)
      : size.width

  return (
    <div
      className={cn(
        "pointer-events-none flex",
        hasGraph && graphExpanded && activeSegment === "workflow"
          ? "max-w-[min(48rem,calc(100%-2rem))]"
          : "max-w-[min(28rem,calc(100%-2rem))]"
      )}
      data-testid="sub-agent-overlay"
      data-has-workflow={hasGraph ? "true" : "false"}
      data-segment={activeSegment}
      style={{ width: panelWidth }}
    >
      <div
        className="pointer-events-auto relative w-full max-w-full rounded-xl border bg-card/60 hover:bg-card/95 shadow-lg backdrop-blur transition-colors supports-[backdrop-filter]:bg-card/50 supports-[backdrop-filter]:hover:bg-card/85"
        data-testid="sub-agent-overlay-card"
      >
        <div className="flex items-center justify-between gap-2 border-b px-3 py-2">
          <div className="flex min-w-0 items-center gap-2">
            {hasGraph ? (
              <GitBranch className="h-4 w-4 shrink-0 text-muted-foreground" />
            ) : (
              <BotIcon className="h-4 w-4 shrink-0 text-muted-foreground" />
            )}
            <span className="truncate text-sm font-medium">{headerTitle}</span>
            {hasGraph && graph ? (
              <Badge
                variant="secondary"
                className="h-5 shrink-0"
                data-testid="workflow-overall-state"
              >
                {tw(`overallState.${graph.overall_state}`)}
              </Badge>
            ) : (
              <Badge variant="secondary" className="h-5 shrink-0">
                {count}
              </Badge>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-0.5">
            {hasGraph && activeSegment === "workflow" && (
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={
                  graphExpanded
                    ? tw("collapseGraphAria")
                    : tw("expandGraphAria")
                }
                data-testid="workflow-expand-toggle"
                onClick={() => setGraphExpanded((v) => !v)}
              >
                {graphExpanded ? (
                  <Minimize2 className="h-4 w-4" />
                ) : (
                  <Maximize2 className="h-4 w-4" />
                )}
              </Button>
            )}
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={t("collapseAria")}
              onClick={() =>
                setCollapsedByKey((prev) => ({ ...prev, [stateKey]: true }))
              }
            >
              <ChevronDownIcon className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {hasGraph && (
          <div
            className="flex items-center gap-1 border-b px-2 py-1.5"
            data-testid="workflow-sessions-segment"
            role="tablist"
            aria-label={tw("segmentAria")}
          >
            <Button
              type="button"
              size="sm"
              variant={activeSegment === "workflow" ? "secondary" : "ghost"}
              className="h-7 flex-1 text-xs"
              role="tab"
              aria-selected={activeSegment === "workflow"}
              data-testid="workflow-segment-workflow"
              onClick={() => setSegment("workflow")}
            >
              {tw("segmentWorkflow")}
            </Button>
            <Button
              type="button"
              size="sm"
              variant={activeSegment === "sessions" ? "secondary" : "ghost"}
              className="h-7 flex-1 text-xs"
              role="tab"
              aria-selected={activeSegment === "sessions"}
              data-testid="workflow-segment-sessions"
              onClick={() => setSegment("sessions")}
            >
              {tw("segmentSessions")}
              {count > 0 && (
                <Badge variant="outline" className="ml-1 h-4 px-1 text-[10px]">
                  {count}
                </Badge>
              )}
            </Button>
          </div>
        )}

        <div
          ref={listRef}
          className="space-y-2 overflow-y-auto p-2"
          style={{ maxHeight: size.maxHeight }}
          data-testid="sub-agent-overlay-list"
        >
          {hasGraph && activeSegment === "workflow" && graph && (
            <div className="space-y-2" data-testid="workflow-compact-body">
              <WorkflowPhaseRail phases={phaseRail} />
              {currentNodes.length > 0 && (
                <div
                  className="space-y-1 rounded-md border bg-muted/20 p-2 text-xs"
                  data-testid="workflow-current-work"
                >
                  {currentNodes.map((node) => (
                    <div
                      key={node.node_id}
                      className="flex flex-wrap items-center gap-1.5"
                    >
                      <span className="font-medium">
                        {tw("currentWork", {
                          title: node.title?.trim() || node.node_id,
                        })}
                      </span>
                      {node.role && (
                        <Badge variant="outline" className="h-4 text-[10px]">
                          {node.role}
                        </Badge>
                      )}
                      {node.agent_type && (
                        <Badge variant="outline" className="h-4 text-[10px]">
                          {node.agent_type}
                        </Badge>
                      )}
                      <Badge variant="secondary" className="h-4 text-[10px]">
                        {tw(`nodeStatus.${node.status}`)}
                      </Badge>
                      {node.round_count != null && node.round_count > 0 && (
                        <span className="tabular-nums text-muted-foreground">
                          {tw("roundCount", { count: node.round_count })}
                        </span>
                      )}
                    </div>
                  ))}
                </div>
              )}
              {graphExpanded && <WorkflowGraphPanel snapshot={graph} compact />}
            </div>
          )}

          {(!hasGraph || activeSegment === "sessions") && (
            <>
              {count === 0 && hasGraph && (
                <p
                  className="px-1 py-2 text-xs text-muted-foreground"
                  data-testid="workflow-sessions-empty"
                >
                  {tw("noSessions")}
                </p>
              )}
              {showDelegationRows && (
                <section
                  className="space-y-1.5"
                  data-testid="sub-agent-origin-codeg"
                >
                  <div className="px-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                    {t("originCodeg")}
                  </div>
                  {delegationGroups.map((group) => (
                    <div
                      key={group.key}
                      data-testid={`sub-agent-overlay-group-${group.childConversationId ?? group.key}`}
                    >
                      <SubAgentOverlayRow
                        source={group.latestSource}
                        sources={group.sources}
                        stickyKey={group.key}
                        runCount={group.runCount}
                        replacement={group.isReplacement}
                        groupChildConversationId={group.childConversationId}
                      />
                    </div>
                  ))}
                </section>
              )}

              {showCodegActivityRows && (
                <section
                  className="space-y-1.5"
                  data-testid="sub-agent-origin-codeg"
                >
                  <div className="px-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                    {t("originCodeg")}
                  </div>
                  {codegActivities.map((activity, i) => (
                    <NativeActivityRow
                      key={`codeg-${activity.task_id ?? i}-${activity.started_at ?? i}`}
                      activity={activity}
                    />
                  ))}
                </section>
              )}

              {nativeActivities.length > 0 && (
                <section
                  className="space-y-1.5"
                  data-testid="sub-agent-origin-native"
                >
                  <div className="px-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                    {t("originNative")}
                  </div>
                  {nativeActivities.map((activity, i) => (
                    <NativeActivityRow
                      key={`native-${activity.task_id ?? i}-${activity.operation}-${activity.started_at ?? i}`}
                      activity={activity}
                    />
                  ))}
                </section>
              )}
            </>
          )}
        </div>

        {/* Right edge — width only */}
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label={t("resizeWidthAria")}
          aria-valuenow={Math.round(size.width)}
          data-testid="sub-agent-overlay-resize-x"
          className={cn(
            "absolute inset-y-2 -right-1 z-10 w-2 cursor-col-resize touch-none",
            "rounded-full hover:bg-foreground/10 active:bg-foreground/15"
          )}
          onPointerDown={(e) => beginResize("x", e)}
        />
        {/* Bottom edge — list max-height only */}
        <div
          role="separator"
          aria-orientation="horizontal"
          aria-label={t("resizeHeightAria")}
          aria-valuenow={Math.round(size.maxHeight)}
          data-testid="sub-agent-overlay-resize-y"
          className={cn(
            "absolute inset-x-2 -bottom-1 z-10 h-2 cursor-row-resize touch-none",
            "rounded-full hover:bg-foreground/10 active:bg-foreground/15"
          )}
          onPointerDown={(e) => beginResize("y", e)}
        />
        {/* Corner — both axes */}
        <div
          role="separator"
          aria-label={t("resizeCornerAria")}
          data-testid="sub-agent-overlay-resize-xy"
          className={cn(
            "absolute -right-1 -bottom-1 z-20 size-3 cursor-nwse-resize touch-none",
            "rounded-sm hover:bg-foreground/15 active:bg-foreground/20"
          )}
          onPointerDown={(e) => beginResize("both", e)}
        />
      </div>
    </div>
  )
})

const SubAgentOverlayRow = memo(function SubAgentOverlayRow({
  source,
  sources,
  stickyKey,
  runCount,
  replacement,
  groupChildConversationId,
}: {
  source: DelegationCardSource
  sources: readonly DelegationCardSource[]
  stickyKey: string
  runCount: number
  replacement: boolean
  groupChildConversationId: number | null
}) {
  const t = useTranslations("Folder.chat.delegation")
  const [filesExpanded, setFilesExpanded] = useState(false)
  const {
    agentType,
    agentDisplayLabel,
    task,
    taskId,
    status,
    errorCode,
    childConversationId,
    displaySecondary,
    conversationTitle,
    elapsedMs,
    toolCallCount,
    editRollup,
    attentionRequest,
    runtimeStats,
    isReplacement,
    childTurnAnchor,
    showGeneratingSegment,
  } = useDelegationCardModel(source, {
    workUnitSources: sources,
    stickyKey,
    explicitUserCancel: sourceExplicitUserCancel(source),
  })

  // Unlike the inline DelegatedSubThread (which falls through to the generic
  // tool renderer when nothing resolves), the overlay always renders one row
  // per real delegation so the collapsed count never disagrees with the list,
  // and meta/output-only states (e.g. after a refresh) still surface. Rows
  // degrade gracefully: unknown agent → neutral dot + "Sub-agent" label,
  // missing child id → no open-conversation control.
  //
  // Structure is a container (never a nested button wrapping expand): chrome
  // owns file expand; a separate sibling button opens the child in a main tab.
  // Child id is enough for the affordance; open helper may resolve agentType
  // from the workspace summary when the row lacks it.
  const clickable = childConversationId != null

  const onOpenChild = useCallback(() => {
    void openDelegatedChildSession({
      childConversationId,
      agentType,
      title: conversationTitle ?? task,
      kickoffTask: task,
      childTurnAnchor,
      liveOwnsActiveTurn: true,
    })
  }, [childConversationId, agentType, conversationTitle, task, childTurnAnchor])

  return (
    <div
      data-testid="sub-agent-row"
      data-origin="codeg"
      data-work-unit-key={stickyKey}
      data-child-conversation-id={groupChildConversationId ?? undefined}
      className="flex w-full items-start gap-2 rounded-lg border bg-transparent px-2 py-1.5"
    >
      <div className="min-w-0 flex-1 space-y-1">
        {/* Name line: small icon inline with the name, then task id + status. */}
        <div className="flex min-w-0 flex-wrap items-center gap-1.5">
          <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-border bg-background text-foreground">
            {agentType ? (
              <AgentIcon agentType={agentType} className="h-3.5 w-3.5" />
            ) : (
              <span className="h-1.5 w-1.5 rounded-sm bg-muted-foreground/60" />
            )}
          </span>
          <span className="min-w-0 break-words text-xs font-semibold text-foreground">
            {agentDisplayLabel ??
              (agentType ? getAgentLabel(agentType) : t("unknownAgent"))}
          </span>
          {taskId && (
            <span
              className="shrink-0 font-mono text-[11px] text-muted-foreground"
              title={taskId}
            >
              #{taskId.slice(0, 8)}
            </span>
          )}
          <StatusBadge status={status} errorCode={errorCode} />
          {runCount > 1 && (
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {t("runCount", { count: runCount })}
            </span>
          )}
          {(replacement || isReplacement) && (
            <span className="shrink-0 text-[11px] font-medium text-amber-700 dark:text-amber-400">
              {t("replacement")}
            </span>
          )}
        </div>
        <DelegationCardChrome
          displaySecondary={displaySecondary}
          conversationTitle={conversationTitle}
          task={task}
          elapsedMs={elapsedMs}
          toolCallCount={toolCallCount}
          editRollup={editRollup}
          attentionRequest={attentionRequest}
          runtimeStats={runtimeStats}
          filesExpanded={filesExpanded}
          onToggleFilesExpanded={() => setFilesExpanded((v) => !v)}
          showGeneratingSegment={showGeneratingSegment}
          compact
        />
      </div>
      {clickable && (
        <button
          type="button"
          data-testid="sub-agent-open"
          onClick={onOpenChild}
          className="shrink-0 inline-flex items-center gap-1 self-center rounded-md px-1.5 py-1 text-[11px] font-medium text-foreground/80 hover:bg-muted/60 hover:text-foreground transition-colors"
          title={t("openDetail")}
          aria-label={t("openDetail")}
        >
          <Eye className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  )
})

/**
 * Informational activity row (native always; Codeg when only activity views
 * are supplied). Never renders a cancel button — native has no Broker action;
 * Codeg cancel stays on the existing companion-tool cards.
 */
const NativeActivityRow = memo(function NativeActivityRow({
  activity,
}: {
  activity: DelegationActivityView
}) {
  const t = useTranslations("Folder.chat.subAgentOverlay")
  const tDel = useTranslations("Folder.chat.delegation")
  const time =
    formatActivityTime(activity.updated_at) ??
    formatActivityTime(activity.started_at)

  return (
    <div
      data-testid="sub-agent-row"
      data-origin={activity.origin}
      data-authoritative={activity.authoritative ? "true" : "false"}
      className="flex w-full min-w-0 items-start gap-2 rounded-lg border bg-transparent px-2 py-1.5"
    >
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex min-w-0 flex-wrap items-center gap-1.5">
          <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-border bg-background text-foreground">
            <AgentIcon agentType={activity.platform} className="h-3.5 w-3.5" />
          </span>
          <span className="min-w-0 break-words text-xs font-semibold text-foreground">
            {getAgentLabel(activity.platform) ?? tDel("unknownAgent")}
          </span>
          {activity.task_id && (
            <span
              className="shrink-0 font-mono text-[11px] text-muted-foreground"
              title={activity.task_id}
            >
              #{activity.task_id.slice(0, 8)}
            </span>
          )}
          <StatusBadge
            status={observedToBadgeStatus(activity.observed_status)}
          />
        </div>
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
          <span className="break-words">
            {t("operation", { op: activity.operation })}
          </span>
          {time && (
            <span
              className="shrink-0 tabular-nums"
              title={activity.updated_at ?? activity.started_at}
            >
              {time}
            </span>
          )}
        </div>
      </div>
    </div>
  )
})
