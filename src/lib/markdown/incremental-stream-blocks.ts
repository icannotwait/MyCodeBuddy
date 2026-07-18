/**
 * Incremental streaming Markdown partitioner.
 *
 * Seals complete Streamdown blocks at safe boundaries (blank lines / closed
 * fences) while keeping an unfinished tail lightweight. Canonical source text
 * remains the authority; `valid: false` falls back to full Markdown rendering.
 */

import { parseMarkdownIntoBlocks } from "streamdown"
import { WeightedLruCache } from "@/lib/cache/weighted-lru"

export interface SealedMarkdownBlock {
  id: string
  markdown: string
}

export interface MarkdownLineScanner {
  pendingLine: string
  fence: null | {
    marker: "`" | "~"
    length: number
    language: string
    openingOffset: number
    bodyOffset: number
  }
  safeBoundarySeen: boolean
  safeBoundaryOffset: number
  safeBoundaryKind: "blank" | "closed_fence" | null
  closedFenceBoundaryOffset: number
  scannedLength: number
}

export type SplitMarkdownBlocks = (markdown: string) => string[]

export interface IncrementalStreamBlocks {
  segmentId: string
  sealed: SealedMarkdownBlock[]
  tail: string
  sourceLength: number
  nextBlockIndex: number
  scanner: MarkdownLineScanner
  splitBlocks: SplitMarkdownBlocks
  valid: boolean
}

function scanCompleteLine(
  scanner: MarkdownLineScanner,
  line: string,
  lineOffset: number,
  nextLineOffset: number
): MarkdownLineScanner {
  const marker = line.match(/^ {0,3}(`{3,}|~{3,})([^\r\n]*)$/)
  if (marker) {
    const token = marker[1]
    const kind = token[0] as "`" | "~"
    if (!scanner.fence) {
      return {
        ...scanner,
        fence: {
          marker: kind,
          length: token.length,
          language: marker[2].trim().split(/\s+/)[0] || "text",
          openingOffset: lineOffset,
          bodyOffset: nextLineOffset,
        },
      }
    }
    if (
      scanner.fence.marker === kind &&
      token.length >= scanner.fence.length &&
      marker[2].trim().length === 0
    ) {
      return {
        ...scanner,
        fence: null,
        safeBoundarySeen: true,
        safeBoundaryOffset: nextLineOffset,
        safeBoundaryKind: "closed_fence",
        closedFenceBoundaryOffset: nextLineOffset,
      }
    }
  }
  if (!scanner.fence && line.trim().length === 0) {
    return {
      ...scanner,
      safeBoundarySeen: true,
      safeBoundaryOffset: nextLineOffset,
      safeBoundaryKind: "blank",
    }
  }
  return scanner
}

function scanMarkdownAppend(
  scanner: MarkdownLineScanner,
  delta: string
): MarkdownLineScanner {
  const buffered = scanner.pendingLine + delta
  const bufferedOffset = scanner.scannedLength - scanner.pendingLine.length
  let next: MarkdownLineScanner = {
    ...scanner,
    pendingLine: "",
    scannedLength: scanner.scannedLength + delta.length,
  }
  let cursor = 0
  while (cursor < buffered.length) {
    const newline = buffered.indexOf("\n", cursor)
    if (newline === -1) {
      next.pendingLine = buffered.slice(cursor)
      return next
    }
    const line = buffered.slice(cursor, newline).replace(/\r$/, "")
    next = scanCompleteLine(
      next,
      line,
      bufferedOffset + cursor,
      bufferedOffset + newline + 1
    )
    cursor = newline + 1
  }
  next.pendingLine = ""
  return next
}

function scanMarkdownFromScratch(text: string): MarkdownLineScanner {
  return scanMarkdownAppend(
    {
      pendingLine: "",
      fence: null,
      safeBoundarySeen: false,
      safeBoundaryOffset: 0,
      safeBoundaryKind: null,
      closedFenceBoundaryOffset: 0,
      scannedLength: 0,
    },
    text
  )
}

export function createIncrementalStreamBlocks(
  segmentId: string,
  splitBlocks: SplitMarkdownBlocks = parseMarkdownIntoBlocks
): IncrementalStreamBlocks {
  return {
    segmentId,
    sealed: [],
    tail: "",
    sourceLength: 0,
    nextBlockIndex: 0,
    scanner: scanMarkdownFromScratch(""),
    splitBlocks,
    valid: true,
  }
}

export function joinStreamingMarkdown(
  document: IncrementalStreamBlocks
): string {
  return document.sealed.map((block) => block.markdown).join("") + document.tail
}

function sealAvailableBlocks(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks {
  const closedFenceOffset = document.scanner.closedFenceBoundaryOffset
  if (closedFenceOffset > 0) {
    const prefix = document.tail.slice(0, closedFenceOffset)
    const tail = document.tail.slice(closedFenceOffset)
    const blocks = document.splitBlocks(prefix)
    if (blocks.join("") !== prefix) {
      return { ...document, valid: false }
    }
    const sealed = blocks.map((markdown, offset) => ({
      id: `${document.segmentId}:block:${document.nextBlockIndex + offset}`,
      markdown,
    }))
    return {
      ...document,
      sealed: [...document.sealed, ...sealed],
      tail,
      nextBlockIndex: document.nextBlockIndex + sealed.length,
      scanner: scanMarkdownFromScratch(tail),
    }
  }
  if (!document.scanner.safeBoundarySeen || document.scanner.fence) {
    return document
  }
  const blocks = document.splitBlocks(document.tail)
  if (blocks.join("") !== document.tail) {
    return { ...document, valid: false }
  }
  if (blocks.length <= 1) {
    return {
      ...document,
      scanner: {
        ...document.scanner,
        safeBoundarySeen: false,
        safeBoundaryOffset: 0,
        safeBoundaryKind: null,
      },
    }
  }
  const tail = blocks[blocks.length - 1] ?? ""
  const sealed = blocks.slice(0, -1).map((markdown, offset) => ({
    id: `${document.segmentId}:block:${document.nextBlockIndex + offset}`,
    markdown,
  }))
  return {
    ...document,
    sealed: [...document.sealed, ...sealed],
    tail,
    nextBlockIndex: document.nextBlockIndex + sealed.length,
    scanner: scanMarkdownFromScratch(tail),
  }
}

export function appendStreamingMarkdown(
  document: IncrementalStreamBlocks,
  delta: string
): IncrementalStreamBlocks {
  if (delta.length === 0) return document
  if (!document.valid) {
    return {
      ...document,
      tail: document.tail + delta,
      sourceLength: document.sourceLength + delta.length,
      scanner: scanMarkdownAppend(document.scanner, delta),
    }
  }
  return sealAvailableBlocks({
    ...document,
    tail: document.tail + delta,
    sourceLength: document.sourceLength + delta.length,
    scanner: scanMarkdownAppend(document.scanner, delta),
  })
}

export function sealStreamingMarkdownBoundary(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks {
  if (document.scanner.fence) return document
  return sealAvailableBlocks({
    ...document,
    scanner: { ...document.scanner, safeBoundarySeen: true },
  })
}

export function completeStreamingMarkdown(
  document: IncrementalStreamBlocks
): IncrementalStreamBlocks {
  if (!document.valid || document.tail.length === 0) return document
  const original = joinStreamingMarkdown(document)
  const blocks = document.splitBlocks(document.tail)
  const appended = blocks.map((markdown, offset) => ({
    id: `${document.segmentId}:block:${document.nextBlockIndex + offset}`,
    markdown,
  }))
  const completed: IncrementalStreamBlocks = {
    ...document,
    sealed: [...document.sealed, ...appended],
    tail: "",
    nextBlockIndex: document.nextBlockIndex + appended.length,
    scanner: scanMarkdownFromScratch(""),
  }
  if (
    original.length === document.sourceLength &&
    joinStreamingMarkdown(completed) === original
  ) {
    return completed
  }
  return {
    ...document,
    sealed: [],
    tail: original,
    scanner: scanMarkdownFromScratch(original),
    valid: false,
  }
}

// ── Live→history one-shot partition handoff cache ─────────────────────

const utf8Encoder = new TextEncoder()

function utf8Bytes(value: string): number {
  return utf8Encoder.encode(value).byteLength
}

/** 32-entry / 2 MiB memory-only cache; each partition is consumed once. */
const completedPartitions = new WeightedLruCache<
  string,
  IncrementalStreamBlocks
>({
  maxEntries: 32,
  maxWeight: 2 * 1024 * 1024,
  weightOf: (document, canonicalTextKey) =>
    utf8Bytes(canonicalTextKey) + utf8Bytes(joinStreamingMarkdown(document)),
})

/** Cache a completed partition keyed by exact canonical text. */
export function cacheCompletedStreamingPartition(
  canonicalText: string,
  document: IncrementalStreamBlocks
): boolean {
  return completedPartitions.set(canonicalText, document)
}

/** Take (consume once) a cached partition for historical TextPart handoff. */
export function takeCompletedStreamingPartition(
  canonicalText: string
): IncrementalStreamBlocks | undefined {
  return completedPartitions.take(canonicalText)
}

/** Clear handoff cache (backend reset / tests / memory pressure). */
export function clearCompletedStreamingPartitions(): void {
  completedPartitions.clear()
}

/** Alias used by streaming-performance cache ownership. */
export const resetCompletedMarkdownPartitions =
  clearCompletedStreamingPartitions

/** Content-free size/weight for tests and perf reports. */
export function getCompletedStreamingPartitionsStats(): {
  size: number
  totalWeight: number
} {
  return {
    size: completedPartitions.size,
    totalWeight: completedPartitions.totalWeight,
  }
}
