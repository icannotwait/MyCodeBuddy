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
 * the child's full conversation via `SubAgentSessionDialog` ("查看会话").
 *
 * Native rows are informational only (`authoritative=false`): origin label +
 * timestamps, no Broker cancel action, no session dialog unless Codeg-backed.
 *
 * Defaults to expanded so historical sub-agents are visible without an extra
 * click; the parent supplies the full conversation's delegation list.
 */

import { memo, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { BotIcon, ChevronDownIcon } from "lucide-react"

import { AgentIcon } from "@/components/agent-icon"
import { CollapsedOverlayChip } from "@/components/chat/collapsed-overlay-chip"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { StatusBadge } from "@/components/message/delegation-status-badge"
import { SubAgentSessionDialog } from "@/components/message/sub-agent-session-dialog"
import {
  useDelegationCardModel,
  type DelegationCardSource,
} from "@/hooks/use-delegation-card-model"
import { AGENT_LABELS, type DelegationActivityView } from "@/lib/types"

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

  const codegActivities = useMemo(
    () => activities.filter((a) => a.origin === "codeg"),
    [activities]
  )
  const nativeActivities = useMemo(
    () => activities.filter((a) => a.origin === "native"),
    [activities]
  )

  // Prefer full Codeg delegation-card rows (session dialog / existing actions)
  // when `delegations` is present. Fall back to activity views only when the
  // parent supplies Codeg activities without tool-call sources. Native rows
  // are always additive and informational.
  const showDelegationRows = delegations.length > 0
  const showCodegActivityRows =
    !showDelegationRows && codegActivities.length > 0
  const count =
    (showDelegationRows ? delegations.length : 0) +
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
    <div className="pointer-events-none flex max-w-[min(22rem,calc(100%-2rem))]">
      <div className="pointer-events-auto w-72 max-w-full rounded-xl border bg-card/60 hover:bg-card/95 shadow-lg backdrop-blur transition-colors supports-[backdrop-filter]:bg-card/50 supports-[backdrop-filter]:hover:bg-card/85">
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

        <div className="max-h-96 space-y-2 overflow-y-auto p-2">
          {showDelegationRows && (
            <section
              className="space-y-1.5"
              data-testid="sub-agent-origin-codeg"
            >
              <div className="px-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                {t("originCodeg")}
              </div>
              {delegations.map((source) => (
                <SubAgentOverlayRow
                  key={source.parentToolUseId}
                  source={source}
                />
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
      </div>
    </div>
  )
})

const SubAgentOverlayRow = memo(function SubAgentOverlayRow({
  source,
}: {
  source: DelegationCardSource
}) {
  const t = useTranslations("Folder.chat.delegation")
  const [dialogOpen, setDialogOpen] = useState(false)
  const {
    agentType,
    agentDisplayLabel,
    task,
    taskId,
    status,
    errorCode,
    childConversationId,
    childConnectionId,
  } = useDelegationCardModel(source)

  // Unlike the inline DelegatedSubThread (which falls through to the generic
  // tool renderer when nothing resolves), the overlay always renders one row
  // per real delegation so the collapsed count never disagrees with the list,
  // and meta/output-only states (e.g. after a refresh) still surface. Rows
  // degrade gracefully: unknown agent → neutral dot + "Sub-agent" label,
  // missing child id → non-clickable.
  const clickable = childConversationId != null

  const rowBody = (
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
      </div>
      {task && (
        <div className="break-words text-[11px] text-muted-foreground">
          {task}
        </div>
      )}
    </div>
  )

  return (
    <>
      {clickable ? (
        <button
          type="button"
          data-testid="sub-agent-row"
          data-origin="codeg"
          onClick={() => setDialogOpen(true)}
          className="flex w-full items-center gap-2 rounded-lg border bg-transparent px-2 py-1.5 text-left transition-colors hover:bg-muted/60"
          // No aria-label: let the row content (agent name + task) name the
          // button so screen readers can tell rows apart. `title` stays for the
          // pointer tooltip.
          title={t("openDetail")}
        >
          {rowBody}
        </button>
      ) : (
        <div
          data-testid="sub-agent-row"
          data-origin="codeg"
          className="flex w-full items-center gap-2 rounded-lg border bg-transparent px-2 py-1.5"
        >
          {rowBody}
        </div>
      )}
      {childConversationId != null && (
        <SubAgentSessionDialog
          open={dialogOpen}
          onOpenChange={setDialogOpen}
          childConversationId={childConversationId}
          childConnectionId={childConnectionId}
          agentType={agentType}
          kickoffTask={task}
        />
      )}
    </>
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
