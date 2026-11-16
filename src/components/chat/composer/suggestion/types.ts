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
  /** This source is still loading; other groups may already be selectable. */
  loading?: boolean
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
  /** Invalid `re:` pattern (Task 10 supplies localized values). */
  invalidPattern?: string
  /** Per-group source failure. */
  sourceError?: string
  /** Profile-catalog failure (agent tab). */
  profileError?: string
}

/**
 * Resolves the `@` query into grouped suggestions. Async so an implementation
 * can hit the file tree / conversations / git log / skills APIs. The optional
 * AbortSignal is aborted when a newer query supersedes this one.
 *
 * `kinds` lists the tabs the panel is showing (the user can hide some). A group
 * outside it is never displayed, so an implementation may skip loading its
 * source and return it empty; omitted means every kind is wanted.
 *
 * `onUpdate` publishes partial snapshots as sources become ready. Groups still
 * waiting for their source set `loading`; the returned value is the final
 * snapshot. Consumers that omit the callback can await all groups as before.
 */
export type ReferenceSearch = (
  query: string,
  signal?: AbortSignal,
  kinds?: readonly ReferenceKind[],
  onUpdate?: (groups: SuggestionGroup[]) => void
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
