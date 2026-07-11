"use client"

import { memo, useMemo } from "react"

import { ReferenceBadge } from "@/components/chat/composer/badges/reference-badge"
import { cn } from "@/lib/utils"

import { parseUserMessageSegments } from "./user-message-segments"

/**
 * Read-only renderer for a USER message's text: plain text with literal line
 * breaks, and the five built-in reference kinds (file / agent / session / commit
 * / skill) shown as inline colored badges. Everything else — including Markdown
 * syntax like `# heading`, `**bold**`, `- item`, code fences — renders VERBATIM.
 *
 * This is the transcript counterpart of the plain-text composer: what the user
 * typed is what they see. Assistant/agent output keeps full Markdown via
 * {@link "@/components/ai-elements/message".MessageResponse} and must NOT use this.
 *
 * `whitespace-pre-wrap` preserves the sender's newlines (replacing the old
 * `remark-breaks` path); `break-words` keeps long unbroken tokens from
 * overflowing the bubble.
 */
export const PlainTextWithBadges = memo(function PlainTextWithBadges({
  text,
  className,
}: {
  text: string
  className?: string
}) {
  const segments = useMemo(() => parseUserMessageSegments(text), [text])
  return (
    <div className={cn("whitespace-pre-wrap break-words", className)}>
      {segments.map((segment, index) =>
        segment.kind === "reference" ? (
          <ReferenceBadge key={index} data={segment.attrs} />
        ) : (
          // A fragment (not a wrapping span) so adjacent text and badges share
          // one inline flow and `whitespace-pre-wrap` collapses nothing.
          <span key={index}>{segment.text}</span>
        )
      )}
    </div>
  )
})
