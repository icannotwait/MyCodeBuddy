import type { ReferenceRegexRank } from "@/lib/types"

import type { ReferenceAttrs, ReferenceKind } from "../types"

/** One selectable row: the reference to insert plus display hints. */
export interface SuggestionItem {
  /**
   * The reference inserted when this row is chosen. Mention candidates always
   * carry a canonical non-null URI; broader `ReferenceAttrs.uri` stays nullable
   * for non-mention invocation kinds.
   */
  reference: ReferenceAttrs & { uri: string }
  /** Secondary line under the label (path, branch, commit message, …). */
  detail?: string | null
  /** Extra text matched against the query, in addition to the label. */
  keywords?: string
  /** Whether the row can be confirmed/inserted (false for continuity errors). */
  selectable: boolean
  /** How fresh the row metadata is relative to the current search generation. */
  freshness: "cache" | "fresh" | "validating"
  /** Stable source order within its origin (agent catalog or backend page). */
  sourceOrdinal: number
  /** Authoritative regex rank metadata; null for literal / local rows. */
  regexRank: ReferenceRegexRank | null
}

/** A labeled group of suggestions, one per reference kind / data source. */
export interface SuggestionGroup {
  kind: ReferenceKind
  /** Display heading for the group. */
  label: string
  items: SuggestionItem[]
  /**
   * True when more items matched than the per-group cap, so the panel shows a
   * non-selectable "keep typing to filter" hint rather than silently dropping
   * the overflow.
   */
  truncated?: boolean
}

/**
 * Localized chrome for the mention panel, injected by the host (English
 * fallbacks live in the popup). Kept together so callers wire it in one place.
 */
export interface MentionUiLabels {
  /** Shown when the query matches nothing. */
  empty: string
  /** Shown while a search is in flight. */
  loading: string
  /** Accessible name for the listbox. */
  listbox: string
  /** Per-group "more results, keep typing" hint. */
  more: string
  /** Builds the live-region result-count announcement (supports plurals). */
  count: (count: number) => string
}

/**
 * Resolves the `@` query into grouped suggestions. Async so an implementation
 * can hit the file tree / conversations / git log / skills APIs. The optional
 * AbortSignal is aborted when a newer query supersedes this one.
 *
 * Phase 2 ships the panel against this interface; Phase 3 supplies the real
 * implementation (wired to the live data hooks) when the composer replaces the
 * textarea in message-input.
 *
 * Task 8 preserves this transitional aggregate contract; Task 9 removes it after
 * every composer migrates to {@link ReferenceSearchController}.
 */
export type ReferenceSearch = (
  query: string,
  signal?: AbortSignal
) => SuggestionGroup[] | Promise<SuggestionGroup[]>

/** State the suggestion plugin pushes to React while the `@` panel is open. */
export interface SuggestionState {
  query: string
  /** Document range covering the trigger char + query, replaced on select. */
  range: { from: number; to: number }
  /** Caret rect for positioning the popup (viewport coords), if known. */
  clientRect: DOMRect | null
}

/** Imperative surface the popup exposes so forwarded key events can drive it. */
export interface SuggestionPopupHandle {
  /** Returns true if the popup consumed the key (caller should preventDefault). */
  onKeyDown: (event: KeyboardEvent) => boolean
}
