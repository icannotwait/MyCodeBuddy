"use client"

import type { ComponentProps, CSSProperties, HTMLAttributes } from "react"
import type {
  BundledLanguage,
  BundledTheme,
  HighlighterGeneric,
  ThemedToken,
} from "shiki"

import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { WeightedLruCache } from "@/lib/cache/weighted-lru"
import { cn, copyTextToClipboard } from "@/lib/utils"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"
import { CheckIcon, CopyIcon } from "lucide-react"
import {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react"

// Shiki uses bitflags for font styles: 1=italic, 2=bold, 4=underline
// biome-ignore lint/suspicious/noBitwiseOperators: shiki bitflag check
const isItalic = (fontStyle: number | undefined) => fontStyle && fontStyle & 1
// biome-ignore lint/suspicious/noBitwiseOperators: shiki bitflag check
// oxlint-disable-next-line eslint(no-bitwise)
const isBold = (fontStyle: number | undefined) => fontStyle && fontStyle & 2
const isUnderline = (fontStyle: number | undefined) =>
  // biome-ignore lint/suspicious/noBitwiseOperators: shiki bitflag check
  // oxlint-disable-next-line eslint(no-bitwise)
  fontStyle && fontStyle & 4

// Transform tokens to include pre-computed keys to avoid noArrayIndexKey lint
interface KeyedToken {
  token: ThemedToken
  key: string
}
interface KeyedLine {
  tokens: KeyedToken[]
  key: string
}

const addKeysToTokens = (lines: ThemedToken[][]): KeyedLine[] =>
  lines.map((line, lineIdx) => ({
    key: `line-${lineIdx}`,
    tokens: line.map((token, tokenIdx) => ({
      key: `line-${lineIdx}-${tokenIdx}`,
      token,
    })),
  }))

// Token rendering component
const TokenSpan = ({ token }: { token: ThemedToken }) => (
  <span
    className="dark:!bg-[var(--shiki-dark-bg)] dark:!text-[var(--shiki-dark)]"
    style={
      {
        backgroundColor: token.bgColor,
        color: token.color,
        fontStyle: isItalic(token.fontStyle) ? "italic" : undefined,
        fontWeight: isBold(token.fontStyle) ? "bold" : undefined,
        textDecoration: isUnderline(token.fontStyle) ? "underline" : undefined,
        ...token.htmlStyle,
      } as CSSProperties
    }
  >
    {token.content}
  </span>
)

// Line rendering component
const LineSpan = ({
  keyedLine,
  showLineNumbers,
}: {
  keyedLine: KeyedLine
  showLineNumbers: boolean
}) => (
  <span className={showLineNumbers ? LINE_NUMBER_CLASSES : "block"}>
    {keyedLine.tokens.length === 0
      ? "\n"
      : keyedLine.tokens.map(({ token, key }) => (
          <TokenSpan key={key} token={token} />
        ))}
  </span>
)

// Types
type CodeBlockProps = HTMLAttributes<HTMLDivElement> & {
  code: string
  language: BundledLanguage
  showLineNumbers?: boolean
}

export interface TokenizedCode {
  tokens: ThemedToken[][]
  fg: string
  bg: string
}

interface CodeBlockContextType {
  code: string
}

// Context
const CodeBlockContext = createContext<CodeBlockContextType>({
  code: "",
})

type HighlighterFactory = (
  language: BundledLanguage
) => Promise<HighlighterGeneric<BundledLanguage, BundledTheme>>

// Highlighter cache (singleton per language)
const highlighterCache = new Map<
  string,
  Promise<HighlighterGeneric<BundledLanguage, BundledTheme>>
>()

let highlighterFactoryOverride: HighlighterFactory | null = null

const tokenCacheUtf8Encoder = new TextEncoder()

function tokenCacheUtf8Bytes(value: string): number {
  return tokenCacheUtf8Encoder.encode(value).byteLength
}

function estimateTokenizedCodeBytes(value: TokenizedCode): number {
  return tokenCacheUtf8Bytes(JSON.stringify(value))
}

/** 128-entry / 8 MiB completed highlight token cache. */
const completedTokens = new WeightedLruCache<string, TokenizedCode>({
  maxEntries: 128,
  maxWeight: 8 * 1024 * 1024,
  weightOf: (value, key) =>
    tokenCacheUtf8Bytes(key) + estimateTokenizedCodeBytes(value),
})

/** One in-flight highlight job per full-source cache key. */
const inflightTokens = new Map<string, Promise<TokenizedCode>>()

// Subscribers for async token updates
const subscribers = new Map<string, Set<(result: TokenizedCode) => void>>()

function getTokensCacheKey(code: string, language: BundledLanguage): string {
  // Full source — length/prefix/suffix keys collide when only the middle changes.
  return `github-light+github-dark\0${language}\0${code}`
}

const defaultHighlighterFactory: HighlighterFactory = (language) =>
  // Load Shiki's engine lazily: a static `import` would pull it (and its
  // textmate machinery) into the first-paint bundle for every message list,
  // even a text-only conversation. Highlighting is already async with an
  // immediate raw-token fallback (see `createRawTokens`), so deferring the
  // engine import only extends that existing pre-highlight window slightly.
  import("shiki").then(({ createHighlighter }) =>
    createHighlighter({
      langs: [language],
      themes: ["github-light", "github-dark"],
    })
  )

const getHighlighter = (
  language: BundledLanguage
): Promise<HighlighterGeneric<BundledLanguage, BundledTheme>> => {
  const cached = highlighterCache.get(language)
  if (cached) {
    return cached
  }

  const factory = highlighterFactoryOverride ?? defaultHighlighterFactory
  const highlighterPromise = factory(language)

  highlighterCache.set(language, highlighterPromise)
  return highlighterPromise
}

// Create raw tokens for immediate display while highlighting loads
const createRawTokens = (code: string): TokenizedCode => ({
  bg: "transparent",
  fg: "inherit",
  tokens: code.split("\n").map((line) =>
    line === ""
      ? []
      : [
          {
            color: "inherit",
            content: line,
          } as ThemedToken,
        ]
  ),
})

function startHighlight(
  code: string,
  language: BundledLanguage,
  tokensCacheKey: string
): Promise<TokenizedCode | undefined> {
  const promise = getHighlighter(language)
    // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-then)
    .then((highlighter) => {
      const availableLangs = highlighter.getLoadedLanguages()
      const langToUse = availableLangs.includes(language) ? language : "text"

      const result = highlighter.codeToTokens(code, {
        lang: langToUse,
        themes: {
          dark: "github-dark",
          light: "github-light",
        },
      })

      return {
        bg: result.bg ?? "transparent",
        fg: result.fg ?? "inherit",
        tokens: result.tokens,
      } satisfies TokenizedCode
    })
    // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-then)
    .then((tokenized) => {
      completedTokens.set(tokensCacheKey, tokenized)
      const subs = subscribers.get(tokensCacheKey)
      if (subs) {
        for (const sub of subs) {
          sub(tokenized)
        }
        subscribers.delete(tokensCacheKey)
      }
      return tokenized
    })
    // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-then), eslint-plugin-promise(prefer-await-to-callbacks)
    .catch((error: unknown) => {
      console.error("Failed to highlight code:", error)
      // Drop subscribers so no stale callback fires; raw tokens stay visible.
      subscribers.delete(tokensCacheKey)
      return undefined
    })
    // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-then)
    .finally(() => {
      inflightTokens.delete(tokensCacheKey)
    })

  inflightTokens.set(tokensCacheKey, promise as Promise<TokenizedCode>)
  return promise
}

// Synchronous highlight with callback for async results
export const highlightCode = (
  code: string,
  language: BundledLanguage,
  // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-callbacks)
  callback?: (result: TokenizedCode) => void
): TokenizedCode | null => {
  const tokensCacheKey = getTokensCacheKey(code, language)

  // Return cached result if available
  const cached = completedTokens.get(tokensCacheKey)
  if (cached) {
    return cached
  }

  // Subscribe callback if provided
  if (callback) {
    if (!subscribers.has(tokensCacheKey)) {
      subscribers.set(tokensCacheKey, new Set())
    }
    subscribers.get(tokensCacheKey)?.add(callback)
  }

  // Share one in-flight job per full-source key
  if (!inflightTokens.has(tokensCacheKey)) {
    // Fire-and-forget; settle handlers above notify subscribers / clear state.
    void startHighlight(code, language, tokensCacheKey)
  }

  return null
}

/** Clear completed/in-flight highlight caches (backend reset / tests). */
export function clearHighlightCaches(): void {
  completedTokens.clear()
  inflightTokens.clear()
  subscribers.clear()
  highlighterCache.clear()
}

/** Alias used by streaming-performance cache ownership. */
export const resetHighlightCaches = clearHighlightCaches

registerBackendScopedStoreReset(clearHighlightCaches)

/** Test-only: override the Shiki highlighter factory. */
export function __setHighlighterFactoryForTest(
  factory: HighlighterFactory | null
): void {
  highlighterFactoryOverride = factory
  highlighterCache.clear()
}

/** Test-only: insert a completed token entry under an arbitrary key. */
export function __putHighlightCacheForTest(
  key: string,
  value: TokenizedCode
): void {
  completedTokens.set(key, value)
}

/** Content-free highlight cache stats (soak / memory assertions). */
export function getHighlightCacheStats(): {
  entries: number
  bytes: number
} {
  return {
    entries: completedTokens.size,
    bytes: completedTokens.totalWeight,
  }
}

/** Test-only alias for highlight cache stats. */
export function __getHighlightCacheStatsForTest(): {
  entries: number
  bytes: number
} {
  return getHighlightCacheStats()
}

/** Test-only: reset overrides, caches, and in-flight state. */
export function __resetHighlightCachesForTest(): void {
  highlighterFactoryOverride = null
  clearHighlightCaches()
}

// Line number styles using CSS counters
const LINE_NUMBER_CLASSES = cn(
  "block",
  "before:content-[counter(line)]",
  "before:inline-block",
  "before:[counter-increment:line]",
  "before:w-8",
  "before:mr-4",
  "before:text-right",
  "before:text-muted-foreground/50",
  "before:font-mono",
  "before:select-none"
)

const CodeBlockBody = memo(
  ({
    tokenized,
    showLineNumbers,
    className,
  }: {
    tokenized: TokenizedCode
    showLineNumbers: boolean
    className?: string
  }) => {
    const preStyle = useMemo(
      () => ({
        backgroundColor: tokenized.bg,
        color: tokenized.fg,
      }),
      [tokenized.bg, tokenized.fg]
    )

    const keyedLines = useMemo(
      () => addKeysToTokens(tokenized.tokens),
      [tokenized.tokens]
    )

    return (
      <pre
        className={cn(
          "dark:!bg-[var(--shiki-dark-bg)] dark:!text-[var(--shiki-dark)] m-0 p-4 text-sm",
          className
        )}
        style={preStyle}
      >
        <code
          className={cn(
            "font-mono text-sm",
            showLineNumbers && "[counter-increment:line_0] [counter-reset:line]"
          )}
        >
          {keyedLines.map((keyedLine) => (
            <LineSpan
              key={keyedLine.key}
              keyedLine={keyedLine}
              showLineNumbers={showLineNumbers}
            />
          ))}
        </code>
      </pre>
    )
  },
  (prevProps, nextProps) =>
    prevProps.tokenized === nextProps.tokenized &&
    prevProps.showLineNumbers === nextProps.showLineNumbers &&
    prevProps.className === nextProps.className
)

CodeBlockBody.displayName = "CodeBlockBody"

export const CodeBlockContainer = ({
  className,
  language,
  style,
  ...props
}: HTMLAttributes<HTMLDivElement> & { language: string }) => (
  <div
    className={cn(
      "group relative w-full overflow-hidden rounded-md border bg-background text-foreground",
      className
    )}
    data-language={language}
    style={{
      containIntrinsicSize: "auto 200px",
      contentVisibility: "auto",
      ...style,
    }}
    {...props}
  />
)

export const CodeBlockHeader = ({
  children,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement>) => (
  <div
    className={cn(
      "flex items-center justify-between border-b bg-muted/80 px-3 py-2 text-muted-foreground text-xs",
      className
    )}
    {...props}
  >
    {children}
  </div>
)

export const CodeBlockTitle = ({
  children,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement>) => (
  <div className={cn("flex items-center gap-2", className)} {...props}>
    {children}
  </div>
)

export const CodeBlockFilename = ({
  children,
  className,
  ...props
}: HTMLAttributes<HTMLSpanElement>) => (
  <span className={cn("font-mono", className)} {...props}>
    {children}
  </span>
)

export const CodeBlockActions = ({
  children,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement>) => (
  <div
    className={cn("-my-1 -mr-1 flex items-center gap-2", className)}
    {...props}
  >
    {children}
  </div>
)

export const CodeBlockContent = ({
  code,
  language,
  showLineNumbers = false,
}: {
  code: string
  language: BundledLanguage
  showLineNumbers?: boolean
}) => {
  // Memoized raw tokens for immediate display
  const rawTokens = useMemo(() => createRawTokens(code), [code])

  // Synchronous cached-or-raw value, recomputed when code/language changes
  const syncTokenized = useMemo(
    () => highlightCode(code, language) ?? rawTokens,
    [code, language, rawTokens]
  )

  // Async highlighted result, tagged with its source code/language.
  // An incrementing request version rejects stale callbacks after props change.
  const [asyncState, setAsyncState] = useState<{
    code: string
    language: string
    tokenized: TokenizedCode
  } | null>(null)
  const requestVersionRef = useRef(0)

  useEffect(() => {
    const requestVersion = ++requestVersionRef.current

    // Subscribe to async highlighting result
    highlightCode(code, language, (result) => {
      if (requestVersion !== requestVersionRef.current) return
      setAsyncState({ code, language, tokenized: result })
    })
  }, [code, language])

  // Use async result only if it matches current code/language (stale
  // versions never write asyncState, so code/language is sufficient here).
  const tokenized =
    asyncState?.code === code && asyncState?.language === language
      ? asyncState.tokenized
      : syncTokenized

  return (
    <div className="relative overflow-auto">
      <CodeBlockBody showLineNumbers={showLineNumbers} tokenized={tokenized} />
    </div>
  )
}

export const CodeBlock = ({
  code,
  language,
  showLineNumbers = false,
  className,
  children,
  ...props
}: CodeBlockProps) => {
  const contextValue = useMemo(() => ({ code }), [code])

  return (
    <CodeBlockContext.Provider value={contextValue}>
      <CodeBlockContainer className={className} language={language} {...props}>
        {children}
        <CodeBlockContent
          code={code}
          language={language}
          showLineNumbers={showLineNumbers}
        />
      </CodeBlockContainer>
    </CodeBlockContext.Provider>
  )
}

export type CodeBlockCopyButtonProps = ComponentProps<typeof Button> & {
  onCopy?: () => void
  onError?: (error: Error) => void
  timeout?: number
}

export const CodeBlockCopyButton = ({
  onCopy,
  onError,
  timeout = 2000,
  children,
  className,
  ...props
}: CodeBlockCopyButtonProps) => {
  const [isCopied, setIsCopied] = useState(false)
  const timeoutRef = useRef<number>(0)
  const { code } = useContext(CodeBlockContext)

  const copyToClipboard = useCallback(async () => {
    if (isCopied) return
    const ok = await copyTextToClipboard(code)
    if (!ok) {
      onError?.(new Error("Clipboard API not available"))
      return
    }
    setIsCopied(true)
    onCopy?.()
    timeoutRef.current = window.setTimeout(() => setIsCopied(false), timeout)
  }, [code, onCopy, onError, timeout, isCopied])

  useEffect(
    () => () => {
      window.clearTimeout(timeoutRef.current)
    },
    []
  )

  const Icon = isCopied ? CheckIcon : CopyIcon

  return (
    <Button
      className={cn("shrink-0", className)}
      onClick={copyToClipboard}
      size="icon"
      variant="ghost"
      {...props}
    >
      {children ?? <Icon size={14} />}
    </Button>
  )
}

export type CodeBlockLanguageSelectorProps = ComponentProps<typeof Select>

export const CodeBlockLanguageSelector = (
  props: CodeBlockLanguageSelectorProps
) => <Select {...props} />

export type CodeBlockLanguageSelectorTriggerProps = ComponentProps<
  typeof SelectTrigger
>

export const CodeBlockLanguageSelectorTrigger = ({
  className,
  ...props
}: CodeBlockLanguageSelectorTriggerProps) => (
  <SelectTrigger
    className={cn(
      "h-7 border-none bg-transparent px-2 text-xs shadow-none",
      className
    )}
    size="sm"
    {...props}
  />
)

export type CodeBlockLanguageSelectorValueProps = ComponentProps<
  typeof SelectValue
>

export const CodeBlockLanguageSelectorValue = (
  props: CodeBlockLanguageSelectorValueProps
) => <SelectValue {...props} />

export type CodeBlockLanguageSelectorContentProps = ComponentProps<
  typeof SelectContent
>

export const CodeBlockLanguageSelectorContent = ({
  align = "end",
  ...props
}: CodeBlockLanguageSelectorContentProps) => (
  <SelectContent align={align} {...props} />
)

export type CodeBlockLanguageSelectorItemProps = ComponentProps<
  typeof SelectItem
>

export const CodeBlockLanguageSelectorItem = (
  props: CodeBlockLanguageSelectorItemProps
) => <SelectItem {...props} />
