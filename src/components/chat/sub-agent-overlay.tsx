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
import { BotIcon, ChevronDownIcon, Eye } from "lucide-react"

import { AgentIcon } from "@/components/agent-icon"
import { CollapsedOverlayChip } from "@/components/chat/collapsed-overlay-chip"
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
import { AGENT_LABELS, type DelegationActivityView } from "@/lib/types"
import { parseDelegateTaskId, parseToolOutput } from "@/lib/delegation-card"
import { delegationRunSnapshotCache } from "@/lib/delegation-run-snapshot"
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
  latestIndex: number
  latestGeneration: number | null
  runCount: number
  isReplacement: boolean
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

/** Group only durable child identities; unknown/live cards remain independent. */
export function groupDelegationSourcesForOverlay(
  delegations: DelegationCardSource[]
): DelegationOverlayGroup[] {
  const groups = new Map<string, DelegationOverlayGroup>()
  delegations.forEach((source, index) => {
    const meta = rawDelegationMeta(source)
    const taskId = sourceBrokerTaskId(source, meta)
    const snapshot = sourceSnapshot(source, taskId)
    const fromMeta = meta?.child_conversation_id
    const fromOutput = parseToolOutput(
      source.output ?? null
    )?.childConversationId
    const childConversationId =
      typeof fromMeta === "number" && Number.isInteger(fromMeta)
        ? fromMeta
        : (fromOutput ?? snapshot?.child_conversation_id ?? null)
    const generation =
      typeof meta?.generation === "number" && Number.isInteger(meta.generation)
        ? meta.generation
        : (snapshot?.generation ?? null)
    const key =
      childConversationId == null
        ? `source:${taskId ?? source.parentToolUseId}`
        : `child:${childConversationId}`
    const replacement =
      sourceReplacementMarker(source) || Boolean(snapshot?.replaced_task_id)
    const existing = groups.get(key)
    if (!existing) {
      groups.set(key, {
        key,
        childConversationId,
        latestSource: source,
        latestIndex: index,
        latestGeneration: generation,
        runCount: 1,
        isReplacement: replacement,
      })
      return
    }
    existing.runCount += 1
    existing.isReplacement ||= replacement
    const isNewer =
      generation != null &&
      (existing.latestGeneration == null ||
        generation >= existing.latestGeneration)
    if (
      isNewer ||
      (generation == null &&
        existing.latestGeneration == null &&
        index > existing.latestIndex)
    ) {
      existing.latestSource = source
      existing.latestIndex = index
      existing.latestGeneration = generation
    }
  })
  return Array.from(groups.values())
}

export const SubAgentOverlay = memo(function SubAgentOverlay({
  delegations,
  activities = [],
  overlayKey,
  defaultExpanded = true,
}: SubAgentOverlayProps) {
  const t = useTranslations("Folder.chat.subAgentOverlay")
  const stateKey = overlayKey ?? "__subagents__default__"
  const [collapsedByKey, setCollapsedByKey] = useState<Record<string, boolean>>(
    {}
  )
  const [size, setSize] = useState<OverlaySize>(defaultOverlaySize)
  const listRef = useRef<HTMLDivElement | null>(null)
  const sizeRef = useRef(size)

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

  if (count === 0) {
    return null
  }

  const userCollapsed = collapsedByKey[stateKey]
  const isExpanded =
    userCollapsed !== undefined ? !userCollapsed : defaultExpanded

  if (!isExpanded) {
    return (
      <CollapsedOverlayChip
        icon={<BotIcon className="size-3" />}
        summary={t("collapsedSummary", { count })}
        onClick={() =>
          setCollapsedByKey((prev) => ({ ...prev, [stateKey]: false }))
        }
      />
    )
  }

  return (
    <div
      className="pointer-events-none flex max-w-[min(28rem,calc(100%-2rem))]"
      data-testid="sub-agent-overlay"
      style={{ width: size.width }}
    >
      <div
        className="pointer-events-auto relative w-full max-w-full rounded-xl border bg-card/60 hover:bg-card/95 shadow-lg backdrop-blur transition-colors supports-[backdrop-filter]:bg-card/50 supports-[backdrop-filter]:hover:bg-card/85"
        data-testid="sub-agent-overlay-card"
      >
        <div className="flex items-center justify-between border-b px-3 py-2">
          <div className="flex min-w-0 items-center gap-2">
            <BotIcon className="h-4 w-4 shrink-0 text-muted-foreground" />
            <span className="truncate text-sm font-medium">{t("title")}</span>
            <Badge variant="secondary" className="h-5 shrink-0">
              {count}
            </Badge>
          </div>
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

        <div
          ref={listRef}
          className="space-y-2 overflow-y-auto p-2"
          style={{ maxHeight: size.maxHeight }}
          data-testid="sub-agent-overlay-list"
        >
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
  runCount,
  replacement,
  groupChildConversationId,
}: {
  source: DelegationCardSource
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
  } = useDelegationCardModel(source)

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
              (agentType ? AGENT_LABELS[agentType] : t("unknownAgent"))}
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
            {AGENT_LABELS[activity.platform] ?? tDel("unknownAgent")}
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
