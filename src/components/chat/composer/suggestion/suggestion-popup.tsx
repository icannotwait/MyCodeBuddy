"use client"

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react"
import { createPortal } from "react-dom"

import { cn } from "@/lib/utils"

import { ReferenceIcon } from "../badges/reference-badge"
import { middleTruncateReferenceText } from "../reference-display"
import type {
  ReferenceSearchController,
  ReferenceGroupKind,
} from "../reference-search-controller"
import type { ReferenceAttrs, ReferenceKind } from "../types"
import type { MentionRenderState } from "./mention-suggestion"
import { placeMentionPopup } from "./popup-position"
import type { SuggestionItem, SuggestionPopupHandle } from "./types"

// Tab order in the panel: agent first (per product decision), then the rest in
// their usual order. This is a *display* order; the search provider keeps its
// own (file-first) group order, which other code/tests depend on. `skill` is
// intentionally absent — skills, commands and experts are inserted via the `/`
// and `$` triggers, not the `@` panel.
const TAB_ORDER: readonly ReferenceGroupKind[] = [
  "agent",
  "file",
  "session",
  "commit",
]

// English fallbacks for the tab labels; the host injects localized ones. `skill`
// is kept for type completeness (`ReferenceKind`) though it is not a shown tab.
const DEFAULT_TAB_LABELS: Record<ReferenceKind, string> = {
  agent: "Agents",
  delegation_profile: "Agents",
  file: "Files",
  session: "Sessions",
  commit: "Commits",
  skill: "Skills",
}

const DEFAULT_INVALID_PATTERN = "Invalid search pattern"
const DEFAULT_SOURCE_ERROR = "Search failed"
const DEFAULT_PROFILE_ERROR = "Could not load profiles"

// Commit-synchronous in the browser so the panel is positioned before paint (no
// flash at a stale spot); a no-op-safe passive effect during the static-export
// prerender where `useLayoutEffect` would warn.
const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect

/**
 * `id` of the listbox element and of each option. The editor's contentEditable
 * (which keeps DOM focus) points `aria-controls` at the listbox and
 * `aria-activedescendant` at the active option, the standard combobox pattern
 * for a popup that doesn't take focus. Option ids are namespaced by tab so the
 * id always resolves to a currently-mounted element (only the active tab's
 * options are rendered). Only one panel is open at a time, so ids never collide.
 */
export const MENTION_LISTBOX_ID = "mention-listbox"
export const mentionOptionId = (kind: ReferenceKind, index: number) =>
  `mention-option-${kind}-${index}`

/**
 * Preserve selection by URI across rank/page updates. When the URI is gone,
 * fall to the same index, then walk backward to the nearest selectable survivor.
 */
export function reconcileSelectedUri(
  previousUri: string | null,
  previousIndex: number,
  nextItems: SuggestionItem[]
): string | null {
  if (
    previousUri &&
    nextItems.some(
      (entry) => entry.reference.uri === previousUri && entry.selectable
    )
  ) {
    return previousUri
  }
  const sameIndex = nextItems[previousIndex]
  if (sameIndex?.selectable) return sameIndex.reference.uri
  for (
    let index = Math.min(previousIndex - 1, nextItems.length - 1);
    index >= 0;
    index--
  ) {
    if (nextItems[index].selectable) return nextItems[index].reference.uri
  }
  return null
}

function firstSelectableUri(items: SuggestionItem[]): string | null {
  return items.find((entry) => entry.selectable)?.reference.uri ?? null
}

function indexOfUri(items: SuggestionItem[], uri: string | null): number {
  if (!uri) return 0
  const index = items.findIndex((entry) => entry.reference.uri === uri)
  return index >= 0 ? index : 0
}

function visibleItemsForSelection(
  items: SuggestionItem[],
  selectedUri: string | null
): SuggestionItem[] {
  if (!selectedUri) return items
  const selected = items.find((entry) => entry.reference.uri === selectedUri)
  if (!selected) return items
  // Controller already caps membership; if a future path reorders past a local
  // display cap this keeps the active row visible by dropping the last other.
  if (items.some((entry) => entry.reference.uri === selectedUri)) {
    return items
  }
  return items
}

interface ConfirmationCapture {
  controller: ReferenceSearchController
  query: string
  from: number
  to: number
  uri: string
}

export interface SuggestionPopupProps {
  /** Live trigger state (query/range/caret rect). */
  state: MentionRenderState
  /** Independent-source controller that owns search + confirmation. */
  controller: ReferenceSearchController
  /** Insert the chosen reference, replacing the trigger range. */
  onSelect: (
    reference: ReferenceAttrs,
    range: { from: number; to: number }
  ) => void
  /** Dismiss the panel without inserting. */
  onClose: () => void
  emptyLabel?: string
  loadingLabel?: string
  /** Accessible name for the listbox / tablist. */
  listboxLabel?: string
  /** Builds the live-region result count announcement. */
  countLabel?: (count: number) => string
  /** Non-selectable hint shown under a tab whose matches were capped. */
  moreLabel?: string
  /** Localized per-kind tab labels (English fallbacks apply when omitted). */
  tabLabels?: Partial<Record<ReferenceKind, string>>
  /** Optional invalid-regex chrome (Task 10 supplies localized values). */
  invalidPatternLabel?: string
  /** Optional per-group source failure chrome. */
  sourceErrorLabel?: string
  /** Optional profile-catalog failure chrome. */
  profileErrorLabel?: string
  /**
   * Reports the active option's element id (or null when nothing is
   * selectable), so the host can mirror it onto the editor's
   * `aria-activedescendant`. Must be referentially stable.
   */
  onActiveOptionChange?: (optionId: string | null) => void
  /**
   * True while the host editor has an IME composition in flight. Pointer
   * confirmation has no KeyboardEvent, so the host supplies this check.
   */
  isEditorComposing?: () => boolean
}

/**
 * The unified `@` panel: tabbed, keyboard-navigable suggestions positioned at
 * the caret. Selection is URI-stable across independent source pages and
 * re-ranks; confirmation goes through {@link ReferenceSearchController}.
 */
export const SuggestionPopup = forwardRef<
  SuggestionPopupHandle,
  SuggestionPopupProps
>(function SuggestionPopup(
  {
    state,
    controller,
    onSelect,
    onClose,
    emptyLabel = "No matches",
    loadingLabel = "Searching…",
    listboxLabel = "Mentions",
    countLabel = (count) => `${count} results`,
    moreLabel = "More results — keep typing to filter",
    tabLabels = DEFAULT_TAB_LABELS,
    invalidPatternLabel = DEFAULT_INVALID_PATTERN,
    sourceErrorLabel = DEFAULT_SOURCE_ERROR,
    profileErrorLabel = DEFAULT_PROFILE_ERROR,
    onActiveOptionChange,
    isEditorComposing,
  },
  ref
) {
  const subscribe = useCallback(
    (listener: () => void) => controller.subscribe(listener),
    [controller]
  )
  const getSnapshot = useCallback(() => controller.getSnapshot(), [controller])
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, getSnapshot)

  const [selectedUri, setSelectedUri] = useState<string | null>(null)
  // The tab the user explicitly chose (via Tab/click), or null to auto-follow
  // the first non-empty tab while no selectable selection anchors the current one.
  const [pinnedTab, setPinnedTab] = useState<ReferenceGroupKind | null>(null)
  const [confirmingUri, setConfirmingUri] = useState<string | null>(null)
  const [pos, setPos] = useState<{
    left: number
    top: number
    placement: "above" | "below"
  } | null>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const selectedIndexRef = useRef(0)
  const autoTabRef = useRef<ReferenceGroupKind>(TAB_ORDER[0])
  const captureRef = useRef<ConfirmationCapture | null>(null)
  const confirmingUriRef = useRef<string | null>(null)
  const stateRef = useRef(state)
  const controllerRef = useRef(controller)
  const onSelectRef = useRef(onSelect)
  const onCloseRef = useRef(onClose)

  useEffect(() => {
    stateRef.current = state
    controllerRef.current = controller
    onSelectRef.current = onSelect
    onCloseRef.current = onClose
  })

  // Publish the exact query synchronously so bare `@` paints catalog rows
  // before the first paint — no legacy aggregate debounce.
  useIsomorphicLayoutEffect(() => {
    controller.setQuery(state.query)
  }, [controller, state.query])

  const groupByKind = useMemo(
    () =>
      new Map(
        (Object.keys(snapshot.groups) as ReferenceGroupKind[]).map((kind) => [
          kind,
          snapshot.groups[kind],
        ])
      ),
    [snapshot.groups]
  )

  const firstNonEmpty = useMemo(
    () =>
      TAB_ORDER.find(
        (kind) => (groupByKind.get(kind)?.items.length ?? 0) > 0
      ) ?? TAB_ORDER[0],
    [groupByKind]
  )

  // Auto-target the first non-empty tab until the user pins one, but never jump
  // away from a group that still holds a selectable selection (R-selection).
  const activeTab: ReferenceGroupKind = useMemo(() => {
    if (pinnedTab) return pinnedTab
    const current = autoTabRef.current
    const currentItems = groupByKind.get(current)?.items ?? []
    const hasSelectableSelection =
      selectedUri != null &&
      currentItems.some(
        (entry) => entry.reference.uri === selectedUri && entry.selectable
      )
    if (hasSelectableSelection) return current
    autoTabRef.current = firstNonEmpty
    return firstNonEmpty
  }, [pinnedTab, firstNonEmpty, groupByKind, selectedUri])

  const activeGroup = groupByKind.get(activeTab) ?? null
  const flat = useMemo(
    () => visibleItemsForSelection(activeGroup?.items ?? [], selectedUri),
    [activeGroup?.items, selectedUri]
  )
  const loading = Boolean(activeGroup?.loading) && flat.length === 0

  // Reconcile URI selection whenever the active tab's items change.
  useIsomorphicLayoutEffect(() => {
    const previousUri = selectedUri
    const previousIndex = selectedIndexRef.current
    const next = reconcileSelectedUri(previousUri, previousIndex, flat)
    selectedIndexRef.current = indexOfUri(flat, next)
    if (next !== previousUri) {
      setSelectedUri(next)
    }
  }, [flat, activeTab])

  // Feed selection into the controller for cache pins + validation.
  useEffect(() => {
    controller.setSelectedUri(selectedUri)
  }, [controller, selectedUri])

  // Scroll the active option into view.
  useEffect(() => {
    listRef.current
      ?.querySelector('[role="option"][data-active="true"]')
      ?.scrollIntoView({ block: "nearest" })
  }, [selectedUri, activeTab])

  const selectedIndex = useMemo(
    () => indexOfUri(flat, selectedUri),
    [flat, selectedUri]
  )

  // Mirror the active option's id to the host (→ editor `aria-activedescendant`).
  useEffect(() => {
    const active =
      selectedUri &&
      flat.some(
        (entry) => entry.reference.uri === selectedUri && entry.selectable
      )
    onActiveOptionChange?.(
      active ? mentionOptionId(activeTab, selectedIndex) : null
    )
  }, [activeTab, selectedIndex, selectedUri, flat, onActiveOptionChange])

  useIsomorphicLayoutEffect(() => {
    if (typeof window === "undefined") return
    const reposition = () => {
      const panel = listRef.current
      if (!panel) return
      const rect = panel.getBoundingClientRect()
      const caret = state.getClientRect?.() ?? null
      setPos(
        placeMentionPopup(
          caret
            ? { left: caret.left, top: caret.top, bottom: caret.bottom }
            : null,
          { width: rect.width, height: rect.height },
          { width: window.innerWidth, height: window.innerHeight }
        )
      )
    }
    reposition()
    window.addEventListener("resize", reposition)
    window.addEventListener("scroll", reposition, true)
    return () => {
      window.removeEventListener("resize", reposition)
      window.removeEventListener("scroll", reposition, true)
    }
  }, [state, loading, flat.length, activeTab, snapshot.patternError])

  // Invalidate in-flight confirmation when the owning controller/query/range
  // identity changes (a settled old promise must not insert at a remapped range).
  useEffect(() => {
    const capture = captureRef.current
    if (!capture) return
    if (
      capture.controller !== controller ||
      capture.query !== state.query ||
      capture.from !== state.range.from ||
      capture.to !== state.range.to
    ) {
      captureRef.current = null
    }
  }, [controller, state.query, state.range.from, state.range.to])

  useEffect(() => {
    return () => {
      captureRef.current = null
      confirmingUriRef.current = null
    }
  }, [])

  const selectCandidate = useCallback(
    async (uri: string, range: { from: number; to: number }) => {
      if (confirmingUriRef.current != null) return
      // Pointer path has no KeyboardEvent — refuse while IME is composing.
      if (isEditorComposing?.()) return
      const live = stateRef.current
      const activeController = controllerRef.current
      const target = flat.find((entry) => entry.reference.uri === uri)
      if (!target?.selectable) return

      const capture: ConfirmationCapture = {
        controller: activeController,
        query: live.query,
        from: range.from,
        to: range.to,
        uri,
      }
      captureRef.current = capture
      confirmingUriRef.current = uri
      setConfirmingUri(uri)

      let result: ReferenceAttrs | null = null
      try {
        result = await activeController.confirmCandidate(uri)
      } catch {
        result = null
      }

      // Clear confirming only when this exact attempt settles.
      if (confirmingUriRef.current === uri) {
        confirmingUriRef.current = null
        setConfirmingUri(null)
      }

      // Drop if the capture was invalidated (controller/query/range remapped).
      if (captureRef.current !== capture) return
      captureRef.current = null

      const current = stateRef.current
      const currentController = controllerRef.current
      if (
        currentController !== capture.controller ||
        current.query !== capture.query ||
        current.range.from !== capture.from ||
        current.range.to !== capture.to
      ) {
        return
      }

      if (result) {
        onSelectRef.current(result, {
          from: capture.from,
          to: capture.to,
        })
      }
      // Known-negative (null): keep picker open; snapshot reconcile moves
      // selection to the nearest survivor.
    },
    [flat, isEditorComposing]
  )

  const moveSelection = useCallback(
    (delta: number) => {
      const selectable = flat
        .map((entry, index) => ({ entry, index }))
        .filter(({ entry }) => entry.selectable)
      if (selectable.length === 0) return
      const currentPos = selectable.findIndex(
        ({ entry }) => entry.reference.uri === selectedUri
      )
      const from = currentPos >= 0 ? currentPos : 0
      const next =
        selectable[(from + delta + selectable.length) % selectable.length]
      selectedIndexRef.current = next.index
      setSelectedUri(next.entry.reference.uri)
    },
    [flat, selectedUri]
  )

  const switchTab = useCallback(
    (kind: ReferenceGroupKind) => {
      setPinnedTab(kind)
      const items = groupByKind.get(kind)?.items ?? []
      const uri = firstSelectableUri(items)
      selectedIndexRef.current = indexOfUri(items, uri)
      setSelectedUri(uri)
    },
    [groupByKind]
  )

  useImperativeHandle(
    ref,
    (): SuggestionPopupHandle => ({
      onKeyDown: (event) => {
        // Return control to IME: no navigation, confirm, or dismiss mid-compose.
        if (
          event.isComposing ||
          event.keyCode === 229 ||
          isEditorComposing?.()
        ) {
          return false
        }
        switch (event.key) {
          case "ArrowDown":
            moveSelection(1)
            return true
          case "ArrowUp":
            moveSelection(-1)
            return true
          case "Tab": {
            const dir = event.shiftKey ? -1 : 1
            const at = TAB_ORDER.indexOf(activeTab)
            const next =
              TAB_ORDER[(at + dir + TAB_ORDER.length) % TAB_ORDER.length]
            switchTab(next)
            return true
          }
          case "Enter": {
            // Consume while confirming or empty so the editor never submits.
            if (confirmingUriRef.current != null) return true
            if (selectedUri) {
              void selectCandidate(selectedUri, state.range)
            }
            return true
          }
          case "Escape":
            onClose()
            return true
          default:
            return false
        }
      },
    }),
    [
      moveSelection,
      activeTab,
      switchTab,
      selectedUri,
      selectCandidate,
      onClose,
      isEditorComposing,
      state.range,
    ]
  )

  const activeLabel = tabLabels[activeTab] ?? DEFAULT_TAB_LABELS[activeTab]
  const truncated = activeGroup?.truncated === true
  const groupError = activeGroup?.error
  const liveStatus = loading
    ? loadingLabel
    : flat.length === 0
      ? `${activeLabel}: ${emptyLabel}`
      : truncated
        ? `${activeLabel}: ${countLabel(flat.length)} ${moreLabel}`
        : `${activeLabel}: ${countLabel(flat.length)}`

  return createPortal(
    <div
      style={{
        position: "fixed",
        left: pos?.left ?? 0,
        top: pos?.top ?? 0,
        // Hidden until the first measure positions it (avoids a flash at 0,0).
        visibility: pos ? "visible" : "hidden",
        zIndex: 50,
      }}
      data-placement={pos?.placement}
    >
      <div
        ref={listRef}
        data-testid="mention-popup"
        className="flex max-h-[min(18rem,calc(100dvh_-_1rem))] w-[52rem] max-w-[calc(100vw_-_1rem)] flex-col overflow-hidden rounded-xl border border-border bg-popover text-popover-foreground shadow-lg"
      >
        <div
          role="tablist"
          aria-label={listboxLabel}
          aria-orientation="horizontal"
          className="flex shrink-0 gap-0.5 overflow-x-auto border-b border-border p-1"
        >
          {TAB_ORDER.map((kind) => {
            const isActive = kind === activeTab
            const count = groupByKind.get(kind)?.items.length ?? 0
            return (
              <button
                key={kind}
                type="button"
                role="tab"
                tabIndex={-1}
                aria-selected={isActive}
                aria-controls={MENTION_LISTBOX_ID}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => switchTab(kind)}
                className={cn(
                  "flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs font-medium",
                  isActive
                    ? "bg-accent text-accent-foreground"
                    : "text-muted-foreground hover:bg-accent/50"
                )}
              >
                <span>{tabLabels[kind] ?? DEFAULT_TAB_LABELS[kind]}</span>
                {count > 0 && (
                  <span className="rounded bg-muted px-1 text-[0.7rem] tabular-nums text-muted-foreground">
                    {count}
                  </span>
                )}
              </button>
            )
          })}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-1">
          {snapshot.patternError && (
            <div className="px-2 py-1 text-xs text-destructive">
              {invalidPatternLabel}
            </div>
          )}
          {groupError === "profile" && (
            <div className="px-2 py-1 text-xs text-destructive">
              {profileErrorLabel}
            </div>
          )}
          {groupError === "source" && (
            <div className="px-2 py-1 text-xs text-destructive">
              {sourceErrorLabel}
            </div>
          )}
          {loading ? (
            <div className="px-2 py-3 text-sm text-muted-foreground">
              {loadingLabel}
            </div>
          ) : flat.length === 0 ? (
            <div className="px-2 py-3 text-sm text-muted-foreground">
              {emptyLabel}
            </div>
          ) : null}
          <div
            id={MENTION_LISTBOX_ID}
            role="listbox"
            aria-label={`${listboxLabel}: ${activeLabel}`}
          >
            {flat.map((entry, index) => {
              const active = entry.reference.uri === selectedUri
              const disabled = !entry.selectable
              const label = entry.reference.label || entry.reference.id
              const displayLabel = middleTruncateReferenceText(label)
              const displayDetail = entry.detail
                ? middleTruncateReferenceText(entry.detail)
                : null
              return (
                <button
                  key={entry.reference.uri}
                  type="button"
                  id={mentionOptionId(activeTab, index)}
                  role="option"
                  aria-selected={active}
                  aria-disabled={disabled || undefined}
                  data-active={active}
                  data-uri={entry.reference.uri}
                  data-confirming={
                    confirmingUri === entry.reference.uri || undefined
                  }
                  disabled={disabled}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm",
                    disabled && "cursor-not-allowed opacity-50",
                    active && !disabled
                      ? "bg-accent text-accent-foreground"
                      : !disabled && "hover:bg-accent/50"
                  )}
                  onMouseDown={(event) => {
                    event.preventDefault()
                    if (disabled) return
                    void selectCandidate(entry.reference.uri, state.range)
                  }}
                  onMouseEnter={() => {
                    if (disabled) return
                    selectedIndexRef.current = index
                    setSelectedUri(entry.reference.uri)
                  }}
                >
                  <ReferenceIcon data={entry.reference} variant="option" />
                  <span className="min-w-0 flex-1 truncate" title={label}>
                    {displayLabel}
                  </span>
                  {displayDetail && (
                    <span
                      className="max-w-[18rem] truncate text-xs text-muted-foreground"
                      title={entry.detail ?? undefined}
                    >
                      {displayDetail}
                    </span>
                  )}
                </button>
              )
            })}
          </div>
          {truncated && (
            <div
              aria-hidden
              className="px-2 py-1 text-xs italic text-muted-foreground"
            >
              {moreLabel}
            </div>
          )}
        </div>
      </div>
      <div role="status" aria-live="polite" className="sr-only">
        {liveStatus}
      </div>
    </div>,
    document.body
  )
})
