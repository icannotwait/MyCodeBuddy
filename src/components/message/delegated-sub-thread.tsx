"use client"

/**
 * Inline rendering of a delegated child sub-session under the parent's
 * `delegate_to_agent` ToolCallBlock. Default state is a single-row header
 * (agent type + status badge + last-message snippet); clicking the chevron
 * expands a scrollable preview of the child's turns.
 *
 * Scope intentionally lean for Phase 8:
 *   * Only `text` and `thinking` content blocks are rendered in the
 *     preview body — tool_use / tool_result / image are summarized as a
 *     compact "tool" line. Phase 9 may upgrade to the full
 *     content-parts-renderer if the user-facing value justifies the size.
 *   * No virtualization — typical delegated sessions are small (≤ ~20
 *     turns); if a user delegates a long-running task, the parent block
 *     stays scrollable and the user can navigate to the child conversation
 *     directly.
 *   * `loading` is only shown for the first fetch. The binding's status
 *     transition (running → ok/err) does NOT trigger a re-fetch — callers
 *     who want the latest turns can collapse + re-expand.
 */

import { useState } from "react"
import { ChevronDown, ChevronRight, Loader2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { useDelegatedSubSession } from "@/hooks/use-delegated-sub-session"
import {
  AGENT_COLORS,
  AGENT_LABELS,
  type ContentBlock,
  type MessageTurn,
} from "@/lib/types"
import { cn } from "@/lib/utils"

interface Props {
  parentToolUseId: string
}

function blocksToText(blocks: ContentBlock[]): string {
  for (const b of blocks) {
    if (b.type === "text" && b.text.trim().length > 0) return b.text
    if (b.type === "thinking" && b.text.trim().length > 0) return b.text
  }
  return ""
}

function turnSummary(turns: MessageTurn[] | undefined): string {
  if (!turns || turns.length === 0) return ""
  // Walk back to find the most recent assistant turn with substantive text;
  // fall back to the last turn's text if every assistant turn was tool-only.
  for (let i = turns.length - 1; i >= 0; i--) {
    if (turns[i].role !== "assistant") continue
    const text = blocksToText(turns[i].blocks)
    if (text) return text
  }
  return blocksToText(turns[turns.length - 1].blocks)
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s
  return s.slice(0, max).trimEnd() + "…"
}

export function DelegatedSubThread({ parentToolUseId }: Props) {
  const t = useTranslations("Folder.chat.delegation")
  const [expanded, setExpanded] = useState(false)
  const { binding, detail, loading, error } = useDelegatedSubSession(
    parentToolUseId,
    { enabled: expanded }
  )

  if (!binding) {
    return null
  }

  const summary = truncate(turnSummary(detail?.turns), 120)
  const turnCount = detail?.turns?.length ?? 0

  return (
    <div
      data-testid="delegated-sub-thread"
      className="rounded-md border border-border bg-muted/30 text-xs"
    >
      <button
        type="button"
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-muted/50 transition-colors"
        aria-expanded={expanded}
      >
        {expanded ? (
          <ChevronDown className="h-3 w-3 shrink-0" />
        ) : (
          <ChevronRight className="h-3 w-3 shrink-0" />
        )}
        <span
          className={cn(
            "inline-flex h-4 w-4 shrink-0 rounded-sm",
            AGENT_COLORS[binding.agentType]
          )}
          aria-hidden
        />
        <span className="font-medium shrink-0">
          {AGENT_LABELS[binding.agentType]}
        </span>
        <StatusBadge status={binding.status} errorCode={binding.errorCode} />
        {summary && (
          <span className="ml-2 truncate text-muted-foreground">{summary}</span>
        )}
        {!summary && turnCount === 0 && binding.status === "running" && (
          <span className="ml-2 text-muted-foreground">{t("inFlight")}</span>
        )}
      </button>
      {expanded && (
        <div className="border-t border-border px-3 py-2 max-h-96 overflow-auto">
          {loading && (
            <div className="flex items-center gap-2 text-muted-foreground">
              <Loader2 className="h-3 w-3 animate-spin" />
              <span>{t("loading")}</span>
            </div>
          )}
          {error && (
            <div className="text-destructive">
              {t("loadFailed", { detail: error })}
            </div>
          )}
          {!loading && !error && detail && (
            <SubThreadPreview turns={detail.turns} />
          )}
          {!loading && !error && !detail && (
            <div className="text-muted-foreground">{t("noDetail")}</div>
          )}
        </div>
      )}
    </div>
  )
}

function StatusBadge({
  status,
  errorCode,
}: {
  status: "running" | "ok" | "err"
  errorCode?: string
}) {
  // next-intl's template-literal-typed t() blows up on dynamic keys, so
  // every label is fetched with a static key string. The known error codes
  // mirror the Rust `DelegationError` taxonomy.
  const t = useTranslations("Folder.chat.delegation.status")
  if (status === "running") {
    return (
      <span className="inline-flex items-center gap-1 rounded-sm bg-amber-500/15 px-1.5 py-0.5 text-amber-700 dark:text-amber-300">
        <Loader2 className="h-2.5 w-2.5 animate-spin" />
        {t("running")}
      </span>
    )
  }
  if (status === "ok") {
    return (
      <span className="rounded-sm bg-emerald-500/15 px-1.5 py-0.5 text-emerald-700 dark:text-emerald-300">
        {t("ok")}
      </span>
    )
  }
  return (
    <span
      className="rounded-sm bg-destructive/15 px-1.5 py-0.5 text-destructive"
      title={errorCode ?? undefined}
    >
      <ErrorLabel code={errorCode} />
    </span>
  )
}

function ErrorLabel({ code }: { code?: string }) {
  const t = useTranslations("Folder.chat.delegation.status.err")
  switch (code) {
    case "delegation_disabled":
      return <>{t("delegation_disabled")}</>
    case "depth_limit":
      return <>{t("depth_limit")}</>
    case "invalid_agent_type":
      return <>{t("invalid_agent_type")}</>
    case "spawn_failed":
      return <>{t("spawn_failed")}</>
    case "send_failed":
      return <>{t("send_failed")}</>
    case "timeout":
      return <>{t("timeout")}</>
    case "canceled":
      return <>{t("canceled")}</>
    default:
      return <>{t("default")}</>
  }
}

function SubThreadPreview({ turns }: { turns: MessageTurn[] }) {
  if (turns.length === 0) {
    return <span className="text-muted-foreground">— no messages yet —</span>
  }
  return (
    <div className="space-y-2">
      {turns.map((turn) => (
        <TurnRow key={turn.id} turn={turn} />
      ))}
    </div>
  )
}

function TurnRow({ turn }: { turn: MessageTurn }) {
  const roleLabel =
    turn.role === "user"
      ? "User"
      : turn.role === "assistant"
        ? "Assistant"
        : "System"
  return (
    <div>
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {roleLabel}
      </div>
      {turn.blocks.map((b, i) => (
        <BlockLine key={i} block={b} />
      ))}
    </div>
  )
}

function BlockLine({ block }: { block: ContentBlock }) {
  if (block.type === "text") {
    return (
      <div className="whitespace-pre-wrap text-foreground/90">{block.text}</div>
    )
  }
  if (block.type === "thinking") {
    return (
      <div className="whitespace-pre-wrap text-muted-foreground italic">
        {block.text}
      </div>
    )
  }
  if (block.type === "tool_use") {
    return (
      <div className="text-muted-foreground">
        <span className="font-mono">⚙ {block.tool_name}</span>
      </div>
    )
  }
  if (block.type === "tool_result") {
    return (
      <div className="text-muted-foreground">
        <span className="font-mono">{block.is_error ? "✕" : "✓"} result</span>
      </div>
    )
  }
  // image / image_generation — silently omitted in preview
  return null
}
