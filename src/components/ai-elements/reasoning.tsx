"use client"

import {
  createContext,
  memo,
  useContext,
  useState,
  useMemo,
  type ReactNode,
} from "react"
import { useTranslations } from "next-intl"
import { BrainIcon, ChevronRightIcon } from "lucide-react"

import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/instant-collapsible"
import { cn } from "@/lib/utils"
import { Shimmer } from "./shimmer"
import remarkBreaks from "remark-breaks"
import {
  Streamdown,
  defaultRehypePlugins,
  defaultRemarkPlugins,
} from "streamdown"

import { markdownLinkComponents } from "./markdown-link"
import { scrollportComponents } from "./markdown-scrollport"
import { mermaidComponents } from "./mermaid-block"
import { LIVE_REMEND, normalizeMathDelimiters } from "./message"
import { rehypePluginsAllowingCodeg } from "./rehype-allow-codeg"
import { rehypeListDirection } from "./rehype-list-direction"
import { remarkTrimCjkAutolinkTail } from "./remark-cjk-autolink-tail"
import { withRelativeFileLinks } from "./rehype-relative-file-links"
import { remarkRewriteFileUriLinks } from "./remark-file-uri-links"
import { remarkRestoreWindowsPaths } from "./remark-windows-paths"
import { useStreamdownPlugins } from "./streamdown-plugins"

// Reasoning is written line by line — steps, checks, half-sentences — not as
// wrapped paragraphs, so a line break stays one instead of folding into a
// space. Named on purpose: Streamdown caches a processor per plugin list,
// keyed by the plugins' function names, which a minified build cuts to a
// letter or two — so this list, as long as the reply's, was served the
// reply's processor and its breaks were lost. A name of its own keeps the two
// keys apart.
const remarkReasoningBreaks = Object.defineProperty(
  () => remarkBreaks(),
  "name",
  { value: "remarkReasoningBreaks" }
)

const remarkPlugins = [
  ...Object.values(defaultRemarkPlugins),
  // Before remarkRewriteFileUriLinks, which reshapes a drive path's url.
  remarkRestoreWindowsPaths,
  remarkRewriteFileUriLinks,
  remarkTrimCjkAutolinkTail,
  remarkReasoningBreaks,
]

// The same links survive as in MessageResponse: `codeg://` references keep
// their href through sanitize (rehype-allow-codeg), which would otherwise leave
// "@Codex [blocked]", and relative local links keep theirs through harden —
// without that `./a.md` would leave harden as `/a.md` and a bare `a.md` would
// be blocked (rehype-relative-file-links). Lists take their text's direction,
// as every block does here (rehype-list-direction).
const rehypePlugins = [
  ...rehypePluginsAllowingCodeg(withRelativeFileLinks(defaultRehypePlugins)),
  rehypeListDirection,
]

const reasoningComponents = {
  ...markdownLinkComponents,
  ...mermaidComponents,
  ...scrollportComponents,
}

/**
 * The reasoning text itself, rendered as Markdown — the activity timeline's
 * thought row draws a thought's body with it.
 *
 * Reasoning is a LIVE surface — a reader who opens it mid-turn keeps it
 * mounted across every delta of a block that routinely runs into the thousands
 * of tokens. `mode="static"` re-parses the whole text each time (streaming
 * splits it into blocks and re-parses only the tail), which measured ~2.9x
 * slower over a 120-delta stream and gets worse the longer the block runs. So
 * track the turn, exactly like the reply prose does: remend while the text is
 * still growing, static — and therefore free of remend's leftover `*` / `_` —
 * once it has settled.
 */
export const ReasoningMarkdown = memo(function ReasoningMarkdown({
  children,
  isStreaming,
}: {
  children: string
  isStreaming: boolean
}) {
  const normalized = useMemo(
    () => normalizeMathDelimiters(children),
    [children]
  )
  const plugins = useStreamdownPlugins(normalized)

  return (
    <Streamdown
      plugins={plugins}
      remarkPlugins={remarkPlugins}
      rehypePlugins={rehypePlugins}
      mode={isStreaming ? "streaming" : "static"}
      parseIncompleteMarkdown={isStreaming}
      // An unclosed link shows as text, as in MessageResponse — see
      // LIVE_REMEND for why the default placeholder cannot be used.
      remend={LIVE_REMEND}
      // Each block reads in the direction of its own text, as in
      // MessageResponse; an item keeps its own side against the UA's
      // `-webkit-match-parent`. Lists draw as a reply's do: numbered or
      // bulleted, the marker inside the text column — outside it, a bullet
      // would sit on the timeline's rail beside a thought.
      dir="auto"
      className="[&_ul]:list-inside [&_ol]:list-inside [&_ul]:list-disc [&_ol]:list-decimal [&_ul]:ps-2 [&_ol]:ps-2 [&_li]:text-start"
      // The link icon + safety override, and the diagram blocks.
      components={reasoningComponents}
    >
      {normalized}
    </Streamdown>
  )
})

/**
 * Collapsible thought the classic renderer and the live transcript still
 * mount. The body is `ReasoningMarkdown` (the timeline's thought text).
 */
interface ReasoningContextValue {
  isStreaming: boolean
  expandable: boolean
  isOpen: boolean
}

const ReasoningContext = createContext<ReasoningContextValue>({
  isStreaming: false,
  expandable: true,
  isOpen: false,
})

export const Reasoning = memo(function Reasoning({
  isStreaming = false,
  expandable = true,
  children,
  className,
}: {
  isStreaming?: boolean
  expandable?: boolean
  children?: ReactNode
  className?: string
}) {
  const [isOpen, setIsOpen] = useState(Boolean(isStreaming))
  return (
    <ReasoningContext.Provider value={{ isStreaming, expandable, isOpen }}>
      <Collapsible
        open={expandable ? isOpen : false}
        onOpenChange={setIsOpen}
        className={cn("w-full", className)}
      >
        {children}
      </Collapsible>
    </ReasoningContext.Provider>
  )
})

export const ReasoningTrigger = memo(function ReasoningTrigger({
  className,
  children,
}: {
  className?: string
  children?: ReactNode
}) {
  const t = useTranslations("Folder.chat.reasoning")
  const { isStreaming, expandable, isOpen } = useContext(ReasoningContext)
  return (
    <CollapsibleTrigger
      className={cn(
        "flex w-full items-center gap-2 text-sm text-muted-foreground transition-colors",
        expandable ? "hover:text-foreground" : "cursor-default",
        className
      )}
      disabled={!expandable}
    >
      {children ?? (
        <>
          <BrainIcon className="size-4" />
          {isStreaming ? (
            <Shimmer duration={1} shineColor="var(--primary)">
              {t("thinking")}
            </Shimmer>
          ) : (
            <span>{t("thoughtForFewSeconds")}</span>
          )}
          {expandable ? (
            <ChevronRightIcon
              className={cn(
                "size-4 transition-transform",
                isOpen ? "rotate-90" : "rotate-0"
              )}
            />
          ) : null}
        </>
      )}
    </CollapsibleTrigger>
  )
})

export const ReasoningContent = memo(function ReasoningContent({
  children,
}: {
  children: string
}) {
  const { isStreaming } = useContext(ReasoningContext)
  return (
    <CollapsibleContent>
      <ReasoningMarkdown isStreaming={isStreaming}>{children}</ReasoningMarkdown>
    </CollapsibleContent>
  )
})
