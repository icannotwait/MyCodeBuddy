"use client"

import { memo, useLayoutEffect, useMemo, useState } from "react"
import {
  Message,
  MessageContent,
  MessageResponse,
} from "@/components/ai-elements/message"
import {
  Reasoning,
  ReasoningContent,
  ReasoningTrigger,
} from "@/components/ai-elements/reasoning"
import { Shimmer } from "@/components/ai-elements/shimmer"
import {
  ContentPartsRenderer,
  ToolCallPart,
} from "@/components/message/content-parts-renderer"
import { GeneratedImagesBlock } from "@/components/message/generated-images-block"
import { PlanCard } from "@/components/message/plan-card"
import { StreamingMarkdownDocument } from "@/components/message/streaming-markdown-document"
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible"
import type { ToolCallInfo } from "@/contexts/acp-connections-context"
import type {
  AdaptedContentPart,
  AdaptedToolCallPart,
  UserImageDisplay,
} from "@/lib/adapters/ai-elements-adapter"
import {
  TOOL_KIND_ORDER,
  type ToolKindLabel,
} from "@/lib/adapters/tool-kind-classifier"
import { useStreamingPerformanceFlag } from "@/lib/acp/streaming-performance-config"
import type { IncrementalStreamBlocks } from "@/lib/markdown/incremental-stream-blocks"
import { streamingPerfRecorder } from "@/lib/perf/streaming-perf-recorder"
import {
  inferLiveToolName,
  normalizeToolName,
} from "@/lib/tool-call-normalization"
import type { AgentType, ToolCallStatus } from "@/lib/types"
import { cn } from "@/lib/utils"
import { ChevronRightIcon } from "lucide-react"
import { useTranslations } from "next-intl"
import { useMessageScroll } from "@/components/message/message-scroll-context"
import {
  getToolJoinedOutput,
  liveTranscriptStore,
  selectRunningOutputTail,
  useLiveTranscriptConversation,
  useLiveTranscriptSegment,
  useLiveTranscriptSegmentIds,
  useLiveTranscriptTool,
  useLiveTranscriptToolGroup,
  useLiveTranscriptToolGroupIds,
} from "@/stores/live-transcript-store"

export interface LiveTranscriptRowProps {
  conversationId: number
  agentType: AgentType
  /** Test / perf hook: called once per LiveToolCard render with toolCallId. */
  onToolRender?: (toolCallId: string) => void
}

const PendingTypingIndicator = memo(function PendingTypingIndicator() {
  return (
    <Message from="assistant">
      <MessageContent>
        <div className="flex items-center gap-1.5 py-1">
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-muted-foreground/60 animate-[pulse_1.4s_ease-in-out_infinite]" />
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-muted-foreground/60 animate-[pulse_1.4s_ease-in-out_0.2s_infinite]" />
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-muted-foreground/60 animate-[pulse_1.4s_ease-in-out_0.4s_infinite]" />
        </div>
      </MessageContent>
    </Message>
  )
})

function narrowToolCallStatus(status: string): ToolCallStatus | null {
  switch (status) {
    case "pending":
    case "in_progress":
    case "completed":
    case "failed":
      return status
    default:
      return null
  }
}

function extractRevisedPrompt(content: string | null): string | null {
  if (!content) return null
  const trimmed = content.trim()
  if (trimmed.length === 0) return null
  const PREFIX = "Revised prompt:"
  if (trimmed.startsWith(PREFIX)) {
    const rest = trimmed.slice(PREFIX.length).trim()
    return rest.length > 0 ? rest : null
  }
  return trimmed
}

function resolveToolInput(info: ToolCallInfo, toolName: string): string | null {
  if (info.raw_input && info.raw_input.trim().length > 0) {
    return info.raw_input
  }
  if (toolName === "read" && Array.isArray(info.locations)) {
    for (const loc of info.locations) {
      if (loc && typeof loc === "object") {
        const path = (loc as { path?: unknown }).path
        if (typeof path === "string" && path.length > 0) {
          return JSON.stringify({ file_path: path })
        }
      }
    }
  }
  return info.raw_input
}

function isRunningCommandToolName(toolName: string): boolean {
  const lower = normalizeToolName(toolName).toLowerCase()
  return lower === "bash" || lower === "exec_command"
}

/**
 * Map a live tool record to the historical AdaptedToolCallPart shape.
 * Running command tools carry only the 24k tail so adapt/join cost is bounded.
 */
export function adaptLiveToolPart(info: ToolCallInfo): AdaptedToolCallPart {
  const toolName = inferLiveToolName({
    title: info.title,
    kind: info.kind,
    rawInput: info.raw_input,
    meta: info.meta,
  })
  const isFinal = info.status === "completed" || info.status === "failed"
  const resolvedOutput = isFinal
    ? getToolJoinedOutput(info) || info.content
    : isRunningCommandToolName(toolName)
      ? selectRunningOutputTail(info) || info.content
      : getToolJoinedOutput(info) || info.content
  const state: AdaptedToolCallPart["state"] = isFinal
    ? info.status === "failed"
      ? "output-error"
      : "output-available"
    : "input-available"

  return {
    type: "tool-call",
    toolCallId: info.tool_call_id,
    toolName,
    displayTitle: info.title || null,
    input: resolveToolInput(info, toolName),
    state,
    output: resolvedOutput,
    errorText:
      isFinal && info.status === "failed"
        ? (resolvedOutput ?? undefined)
        : undefined,
    meta: info.meta ?? null,
  }
}

function imageDisplayFromTool(info: ToolCallInfo): UserImageDisplay | null {
  const img = info.images?.[0]
  if (!img?.data || !img.mime_type) return null
  const ext = img.mime_type.split("/")[1]?.split("+")[0] ?? "image"
  return {
    name: img.uri?.trim() ? img.uri : `image.${ext}`,
    data: img.data,
    mime_type: img.mime_type,
    uri: img.uri ?? null,
  }
}

/** Drain delivery→paint samples after live-footer React commits (P2 path). */
function useLiveFooterPerfCommit() {
  useLayoutEffect(() => {
    streamingPerfRecorder.markReactCommit()
  })
}

const LiveTextSegment = memo(function LiveTextSegment({
  text,
}: {
  text: string
}) {
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()
  return (
    <div className='break-words text-sm prose prose-sm dark:prose-invert max-w-none [&_ul]:list-inside [&_ol]:list-inside [&_[data-streamdown="code-block-body"]]:max-h-96 [&_[data-streamdown="code-block-body"]]:overflow-auto'>
      <MessageResponse>{text}</MessageResponse>
    </div>
  )
})

/** P3 incremental Markdown: memoized sealed blocks + lightweight tail. */
const LiveIncrementalTextSegment = memo(function LiveIncrementalTextSegment({
  document,
}: {
  document: IncrementalStreamBlocks
}) {
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()
  return (
    <div className='break-words text-sm prose prose-sm dark:prose-invert max-w-none [&_ul]:list-inside [&_ol]:list-inside [&_[data-streamdown="code-block-body"]]:max-h-96 [&_[data-streamdown="code-block-body"]]:overflow-auto'>
      <StreamingMarkdownDocument document={document} />
    </div>
  )
})

const LiveThinkingSegment = memo(function LiveThinkingSegment({
  text,
  isStreaming,
}: {
  text: string
  isStreaming: boolean
}) {
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()
  return (
    <Reasoning isStreaming={isStreaming} expandable>
      <ReasoningTrigger />
      <ReasoningContent>{text}</ReasoningContent>
    </Reasoning>
  )
})

const LivePlanSegment = memo(function LivePlanSegment({
  entries,
  isStreaming,
}: {
  entries: import("@/lib/types").PlanEntryInfo[]
  isStreaming: boolean
}) {
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()
  return <PlanCard entries={entries} isStreaming={isStreaming} />
})

export interface LiveToolCardProps {
  conversationId: number
  toolCallId: string
  onToolRender?: (toolCallId: string) => void
  /**
   * When true, skip the outer ContentPartsRenderer group pipeline and render
   * the adapted tool card directly (used inside live tool groups).
   */
  direct?: boolean
}

/**
 * Per-tool live card: only this component re-renders when its tool updates.
 * Does not subscribe to the full transcript / tool map.
 */
export const LiveToolCard = memo(function LiveToolCard({
  conversationId,
  toolCallId,
  onToolRender,
  direct = false,
}: LiveToolCardProps) {
  const tool = useLiveTranscriptTool(conversationId, toolCallId)
  streamingPerfRecorder.countRender("toolCard")
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()
  onToolRender?.(toolCallId)
  const part = useMemo(() => (tool ? adaptLiveToolPart(tool) : null), [tool])
  if (!part) return null
  if (direct) {
    return <ToolCallPart part={part} live />
  }
  // Single-part renderer reuses the full tool card stack without rebuilding
  // historical message groups.
  const parts: AdaptedContentPart[] = [part]
  return <ContentPartsRenderer parts={parts} role="assistant" />
})

/**
 * Collapsed live tool-group: summary from aggregate counts only.
 * LiveToolCard children mount only while expanded.
 */
export const LiveToolGroupCard = memo(function LiveToolGroupCard({
  conversationId,
  groupId,
  onToolRender,
}: {
  conversationId: number
  groupId: string
  onToolRender?: (toolCallId: string) => void
}) {
  const group = useLiveTranscriptToolGroup(conversationId, groupId)
  const t = useTranslations("Folder.chat.contentParts.toolGroup")
  const [open, setOpen] = useState(false)
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()

  const { phrases, errorPhrase } = useMemo(() => {
    if (!group) {
      return { phrases: [] as string[], errorPhrase: null as string | null }
    }
    const built: string[] = []
    for (const kind of TOOL_KIND_ORDER) {
      const count = group.counts[kind as ToolKindLabel]
      if (count <= 0) continue
      built.push(t(kind, { count }))
    }
    if (built.length === 0) {
      built.push(t("other", { count: group.toolCallIds.length }))
    }
    return {
      phrases: built,
      errorPhrase:
        group.errorCount > 0
          ? t("errorSuffix", { count: group.errorCount })
          : null,
    }
  }, [group, t])

  if (!group || group.toolCallIds.length === 0) return null

  const joiner = t("joiner")
  const titleText = phrases.join(joiner)
  const isStreaming = group.runningCount > 0

  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="w-full"
      data-testid={`live-tool-group-${groupId}`}
      data-group-open={open ? "true" : "false"}
    >
      <CollapsibleTrigger
        className={cn(
          "group inline-flex max-w-full items-center gap-1.5 rounded-full bg-muted/60 px-3.5 py-2 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        )}
      >
        <ChevronRightIcon
          aria-hidden="true"
          className={cn(
            "size-3 shrink-0 opacity-60 transition-transform",
            open && "rotate-90"
          )}
        />
        <span className="min-w-0 truncate">
          {isStreaming ? (
            <Shimmer as="span" duration={1} shineColor="var(--primary)">
              {titleText}
            </Shimmer>
          ) : (
            titleText
          )}
          {errorPhrase && (
            <span className="text-destructive">
              {joiner}
              {errorPhrase}
            </span>
          )}
        </span>
      </CollapsibleTrigger>
      {/* Map tool IDs → LiveToolCard only while open (collapsed = summary). */}
      {open ? (
        <CollapsibleContent
          className={cn(
            "w-full outline-none",
            "data-[state=open]:animate-in data-[state=closed]:animate-out",
            "data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
            "data-[state=closed]:slide-out-to-top-1 data-[state=open]:slide-in-from-top-1"
          )}
        >
          <div className="mt-3 w-full space-y-3">
            {group.toolCallIds.map((toolCallId) => (
              <LiveToolCard
                key={toolCallId}
                conversationId={conversationId}
                toolCallId={toolCallId}
                onToolRender={onToolRender}
                direct
              />
            ))}
          </div>
        </CollapsibleContent>
      ) : null}
    </Collapsible>
  )
})

const LiveGeneratedImageSegment = memo(function LiveGeneratedImageSegment({
  conversationId,
  toolCallId,
}: {
  conversationId: number
  toolCallId: string
}) {
  const tool = useLiveTranscriptTool(conversationId, toolCallId)
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()
  if (!tool) return null
  return (
    <GeneratedImagesBlock
      revisedPrompt={extractRevisedPrompt(tool.content)}
      image={imageDisplayFromTool(tool)}
      status={narrowToolCallStatus(tool.status)}
    />
  )
})

const LiveTranscriptSegmentView = memo(function LiveTranscriptSegmentView({
  conversationId,
  segmentId,
  onToolRender,
}: {
  conversationId: number
  segmentId: string
  agentType: AgentType
  onToolRender?: (toolCallId: string) => void
}) {
  const segment = useLiveTranscriptSegment(conversationId, segmentId)
  const deferredRich = useStreamingPerformanceFlag(
    "deferred_streaming_rich_content"
  )
  if (!segment) return null

  switch (segment.type) {
    case "text":
      return deferredRich ? (
        <LiveIncrementalTextSegment document={segment.document} />
      ) : (
        <LiveTextSegment text={segment.text} />
      )
    case "thinking":
      return <LiveThinkingSegment text={segment.text} isStreaming />
    case "plan":
      return <LivePlanSegment entries={segment.entries} isStreaming />
    case "tool":
      return (
        <LiveToolCard
          conversationId={conversationId}
          toolCallId={segment.toolCallId}
          onToolRender={onToolRender}
        />
      )
    case "generated-image":
      return (
        <LiveGeneratedImageSegment
          conversationId={conversationId}
          toolCallId={segment.toolCallId}
        />
      )
    default:
      return null
  }
})

type LiveFooterItem =
  | { kind: "segment"; segmentId: string }
  | { kind: "group"; groupId: string }

/**
 * Interleave non-group segments with structural tool-group summaries so
 * consecutive groupable tools collapse into one summary chip in the footer.
 */
function buildLiveFooterItems(
  conversationId: number,
  segmentIds: readonly string[],
  groupIds: readonly string[]
): LiveFooterItem[] {
  // Only multi-tool runs collapse into a summary chip. Lone tools keep a
  // dedicated LiveToolCard so running command tails stay visible mid-stream.
  const toolToGroup = new Map<string, string>()
  const firstToolOfGroup = new Set<string>()
  for (const groupId of groupIds) {
    const group = liveTranscriptStore.getToolGroup(conversationId, groupId)
    if (!group || group.toolCallIds.length < 2) continue
    for (let i = 0; i < group.toolCallIds.length; i++) {
      const toolCallId = group.toolCallIds[i]
      toolToGroup.set(toolCallId, groupId)
      if (i === 0) firstToolOfGroup.add(toolCallId)
    }
  }

  const items: LiveFooterItem[] = []
  for (const segmentId of segmentIds) {
    const segment = liveTranscriptStore.getSegment(conversationId, segmentId)
    if (segment?.type === "tool") {
      const groupId = toolToGroup.get(segment.toolCallId)
      if (groupId) {
        if (firstToolOfGroup.has(segment.toolCallId)) {
          items.push({ kind: "group", groupId })
        }
        // Skip non-first members of a structural group.
        continue
      }
    }
    items.push({ kind: "segment", segmentId })
  }
  return items
}

/**
 * Live assistant reply footer: outside Virtua history, with narrow
 * per-segment / per-tool / per-group subscriptions so historical rows stay cold.
 */
export const LiveTranscriptRow = memo(function LiveTranscriptRow({
  conversationId,
  agentType,
  onToolRender,
}: LiveTranscriptRowProps) {
  const segmentIds = useLiveTranscriptSegmentIds(conversationId)
  const groupIds = useLiveTranscriptToolGroupIds(conversationId)
  const conversation = useLiveTranscriptConversation(conversationId)
  const messageScroll = useMessageScroll()
  const publicationVersion = conversation?.lastAppliedSeq ?? 0
  streamingPerfRecorder.countRender("liveRow")
  useLiveFooterPerfCommit()

  // Footer-only publication follow owner: each lastAppliedSeq bump schedules
  // at most one RAF-coalesced correction while follow intent is held.
  // VirtualizedMessageThread does not also schedule on seq — only ResizeObserver
  // height growth (expand / sealed block) schedules from the shell.
  useLayoutEffect(() => {
    if (publicationVersion <= 0) return
    messageScroll?.footerScroll?.scheduleFollow(publicationVersion)
  }, [publicationVersion, messageScroll])

  const items = useMemo(
    () => buildLiveFooterItems(conversationId, segmentIds, groupIds),
    [conversationId, segmentIds, groupIds]
  )

  if (segmentIds.length === 0) {
    return <PendingTypingIndicator />
  }

  return (
    <Message from="assistant" data-testid="live-transcript-row">
      <MessageContent>
        <div className="space-y-4">
          {items.map((item) =>
            item.kind === "group" ? (
              <LiveToolGroupCard
                key={item.groupId}
                conversationId={conversationId}
                groupId={item.groupId}
                onToolRender={onToolRender}
              />
            ) : (
              <LiveTranscriptSegmentView
                key={item.segmentId}
                conversationId={conversationId}
                segmentId={item.segmentId}
                agentType={agentType}
                onToolRender={onToolRender}
              />
            )
          )}
        </div>
      </MessageContent>
    </Message>
  )
})
