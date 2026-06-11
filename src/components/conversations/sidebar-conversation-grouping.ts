import type { DbConversationSummary } from "@/lib/types"
import type { SidebarSortMode } from "@/lib/sidebar-view-mode-storage"

export function parseTimestamp(value: string): number {
  const timestamp = Date.parse(value)
  return Number.isNaN(timestamp) ? 0 : timestamp
}

export function compareByUpdatedAtDesc(
  left: DbConversationSummary,
  right: DbConversationSummary
): number {
  const updatedDiff =
    parseTimestamp(right.updated_at) - parseTimestamp(left.updated_at)
  if (updatedDiff !== 0) return updatedDiff

  const createdDiff =
    parseTimestamp(right.created_at) - parseTimestamp(left.created_at)
  if (createdDiff !== 0) return createdDiff

  return right.id - left.id
}

export function compareByCreatedAtDesc(
  left: DbConversationSummary,
  right: DbConversationSummary
): number {
  const createdDiff =
    parseTimestamp(right.created_at) - parseTimestamp(left.created_at)
  if (createdDiff !== 0) return createdDiff

  const updatedDiff =
    parseTimestamp(right.updated_at) - parseTimestamp(left.updated_at)
  if (updatedDiff !== 0) return updatedDiff

  return right.id - left.id
}

/**
 * Most-recently-pinned first. Only ever applied to rows with a non-null
 * `pinned_at` (the pinned bucket), so the empty-string fallback is just a guard.
 */
export function compareByPinnedAtDesc(
  left: DbConversationSummary,
  right: DbConversationSummary
): number {
  const diff =
    parseTimestamp(right.pinned_at ?? "") - parseTimestamp(left.pinned_at ?? "")
  if (diff !== 0) return diff
  return right.id - left.id
}

/**
 * Relative time label (e.g. "5m", "3h", "2d"). `now` is passed in rather than
 * read from `Date.now()` so a whole render tick shares one value: every
 * unchanged row then produces an identical label string and the card `memo`
 * stays hit. The list refreshes `now` once a minute (see
 * `SidebarConversationList`), bounding label staleness without making a single
 * status event re-render every card.
 */
export function formatRelative(iso: string, now: number): string {
  const ts = parseTimestamp(iso)
  if (!ts) return ""
  const diff = Math.max(0, now - ts)
  const m = Math.floor(diff / 60000)
  if (m < 1) return "now"
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h`
  const d = Math.floor(h / 24)
  if (d < 30) return `${d}d`
  const mo = Math.floor(d / 30)
  if (mo < 12) return `${mo}mo`
  const y = Math.floor(mo / 12)
  return `${y}y`
}

function arraysShallowEqual<T>(a: readonly T[], b: readonly T[]): boolean {
  if (a === b) return true
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false
  }
  return true
}

/**
 * Return `prev` when `next` has identical string membership, else `next`.
 *
 * `tabs` is rebuilt (new array) on every `conversations` change (tab-context
 * re-derives titles/status), so `openTabKeys` recomputes every status event.
 * Without this reuse the freshly-built Set would be a new reference each time
 * and would defeat the `FolderGroupItem` memo for *every* folder. Content
 * equality keeps the reference stable when the open-tab set is actually
 * unchanged.
 */
export function reuseSet(prev: Set<string>, next: Set<string>): Set<string> {
  if (prev === next) return prev
  if (prev.size !== next.size) return next
  for (const key of next) {
    if (!prev.has(key)) return next
  }
  return prev
}

export interface SelectedConversationRef {
  id: number
  agentType: string
}

/**
 * Return `prev` when it denotes the same conversation as `next`, else `next`.
 * Same motivation as {@link reuseSet}: keeps `selectedConversation` reference
 * stable across the `tabs` churn so unaffected folders stay memoized.
 */
export function reuseSelected(
  prev: SelectedConversationRef | null,
  next: SelectedConversationRef | null
): SelectedConversationRef | null {
  if (
    prev &&
    next &&
    prev.id === next.id &&
    prev.agentType === next.agentType
  ) {
    return prev
  }
  return next
}

/**
 * Group conversations by folder, sorting each bucket, while reusing the
 * previous render's bucket array whenever a folder's sorted membership is
 * referentially unchanged.
 *
 * Reference stability is the whole point: a single `conversation_status_changed`
 * event replaces exactly one summary object (slice + spread in
 * `updateConversationLocal`), so only the touched folder's bucket fails the
 * shallow-equality check and gets a fresh array. Every sibling folder keeps its
 * old array reference, letting a memoized `FolderGroupItem` bail out — and
 * inside the one folder that did change, every unchanged summary keeps its
 * object identity so the card `memo` still bails out for all but the one
 * affected row.
 *
 * `prev` is the map returned by the last call (the caller threads it via a ref).
 *
 * `childToParent` (optional) merges worktree child folders into their parent: a
 * conversation whose `folder_id` is a key is bucketed under the mapped parent
 * id instead, so the parent group renders the main repo's and all its worktrees'
 * conversations together (sorted as one bucket). The conversation objects
 * themselves are untouched — only the grouping key is redirected, never
 * `folder_id` — so per-conversation cwd resolution stays correct.
 */
export function groupByFolderWithReuse(
  filtered: readonly DbConversationSummary[],
  sortMode: SidebarSortMode,
  prev: Map<number, DbConversationSummary[]>,
  childToParent?: ReadonlyMap<number, number>
): Map<number, DbConversationSummary[]> {
  const next = new Map<number, DbConversationSummary[]>()
  for (const conv of filtered) {
    const groupId = childToParent?.get(conv.folder_id) ?? conv.folder_id
    const list = next.get(groupId)
    if (list) list.push(conv)
    else next.set(groupId, [conv])
  }

  const comparator =
    sortMode === "updated" ? compareByUpdatedAtDesc : compareByCreatedAtDesc
  for (const [folderId, list] of next) {
    list.sort(comparator)
    const prevList = prev.get(folderId)
    // Replacing an existing key's value mid-iteration is safe (we never add or
    // remove keys here).
    if (prevList && arraysShallowEqual(prevList, list)) {
      next.set(folderId, prevList)
    }
  }
  return next
}

/**
 * Select the pinned conversations (those with a non-null `pinned_at`), sorted
 * most-recently-pinned first, reusing the previous array reference when the
 * sorted membership is referentially unchanged.
 *
 * Same reference-stability motivation as {@link groupByFolderWithReuse}: a
 * single status event replaces exactly one summary object, so this would
 * otherwise build a fresh array each tick and defeat the Pinned section's memo.
 * Built from the FULL `conversations` list (never the completed-filtered one): a
 * pinned conversation stays in the Pinned section even when "Show completed" is
 * off — pinning is an explicit "keep this handy" override of that filter.
 *
 * `prev` is the array returned by the last call (the caller threads it via a
 * ref).
 */
export function selectPinnedWithReuse(
  conversations: readonly DbConversationSummary[],
  prev: DbConversationSummary[]
): DbConversationSummary[] {
  const next: DbConversationSummary[] = []
  for (const conv of conversations) {
    if (conv.pinned_at != null) next.push(conv)
  }
  next.sort(compareByPinnedAtDesc)
  return arraysShallowEqual(prev, next) ? prev : next
}

/**
 * Select the folderless "chat mode" conversations — those whose `folder_id` is a
 * hidden `is_chat` folder — for the flat "Chat" sidebar section. Sorted
 * most-recently-updated first, with reference reuse (same motivation as
 * {@link selectPinnedWithReuse}).
 *
 * Excludes pinned conversations (they surface in the Pinned section, an explicit
 * override) and — unless `showCompleted` — completed ones, matching how
 * `folderConversations` is filtered for the folders section.
 *
 * `chatFolderIds` is the set of hidden chat folder ids (from
 * `allFolders.filter(f => f.is_chat)`). `prev` is the array returned last call
 * (threaded via a ref by the caller).
 */
export function selectChatConversationsWithReuse(
  conversations: readonly DbConversationSummary[],
  chatFolderIds: ReadonlySet<number>,
  showCompleted: boolean,
  prev: DbConversationSummary[]
): DbConversationSummary[] {
  const next: DbConversationSummary[] = []
  for (const conv of conversations) {
    if (conv.pinned_at != null) continue
    if (!chatFolderIds.has(conv.folder_id)) continue
    if (!showCompleted && conv.status === "completed") continue
    next.push(conv)
  }
  next.sort(compareByUpdatedAtDesc)
  return arraysShallowEqual(prev, next) ? prev : next
}

// ── Flat row model (Phase 2 virtualization) ─────────────────────────────────
// The sidebar tree (folders → their conversation rows) is flattened into a
// single linear array so it can be windowed by `virtua`. Each visible folder
// contributes one header row, and — when expanded — either one empty-hint row
// or its sorted conversation rows.

export interface FolderHeaderRow {
  kind: "folder"
  folderId: number
}

export interface ConversationRow {
  kind: "conversation"
  /**
   * The summary object reference is passed through untouched (never copied), so
   * a status event that replaces exactly one summary keeps every other row's
   * `conversation` identity — the linchpin that lets the card `memo` bail out
   * through the virtualized render. See {@link groupByFolderWithReuse}.
   */
  conversation: DbConversationSummary
}

export interface EmptyHintRow {
  kind: "empty"
  folderId: number
  /**
   * Total (unfiltered, pinned-excluded) conversation count for this folder, used
   * by the renderer to pick between the "empty folder" and "no unfinished
   * conversations" hints.
   */
  totalConversationCount: number
}

/**
 * The single empty-state hint shown under an expanded but empty "Chat" section
 * ("No chats yet"). Unlike {@link EmptyHintRow} it is folderless — chat
 * conversations are a flat list — so it carries no folder id and renders with a
 * flat (non-rail) indent.
 */
export interface ChatsEmptyRow {
  kind: "chats-empty"
}

/**
 * A collapsible section heading. Three exist: "pinned" (above the folders, shown
 * only when there are pinned conversations), "folders" (wraps the whole folder
 * list), and "chats" (below the folders, a flat list of folderless chat-mode
 * conversations, shown only when there are any). All live in the same flat row
 * array so the single Virtualizer windows them like any other row — there is no
 * separate, un-virtualized list.
 */
export interface SectionHeaderRow {
  kind: "section"
  section: "pinned" | "folders" | "chats"
  expanded: boolean
  /** Pinned count, folder count, or chat-conversation count — shown beside the title. */
  count: number
}

export type SidebarRow =
  | SectionHeaderRow
  | FolderHeaderRow
  | ConversationRow
  | EmptyHintRow
  | ChatsEmptyRow

/**
 * Flatten the (optional) pinned section and the folders section into a single
 * linear row list for windowing by the one Virtualizer — pinned conversations
 * are ordinary conversation rows in the SAME array, never a separate list.
 *
 * Pure and deliberately **does not take `now`**: the per-minute `now` tick that
 * refreshes relative time labels must not rebuild this array (that would defeat
 * the Phase 1 memo chain). `timeLabel` stays computed at the row renderer from
 * the shared `now` against the row's `conversation`.
 *
 * Structure:
 * - The "Pinned" section header + its conversations appear only when `pinned`
 *   is non-empty, and its rows only when `pinnedExpanded`.
 * - The "Folders" section header appears whenever there are folders; its folder
 *   rows appear only when `foldersExpanded`. Within it, order follows
 *   `orderedFolderIds`: a collapsed folder contributes only its header; an
 *   expanded empty folder contributes header + one empty-hint row; an expanded
 *   non-empty folder contributes header + its (already sorted) bucket. `byFolder`
 *   / `folderTotalCounts` exclude pinned conversations (they live in the Pinned
 *   section), so a folder whose only conversations are pinned reads as empty.
 * - The "Chat" section header ALWAYS appears (even with zero chat
 *   conversations), so the section is a permanent entry point — its New-chat
 *   affordance and an empty hint stay reachable. When expanded and empty it
 *   contributes a single `chats-empty` hint row; otherwise its (flat, folderless)
 *   conversation rows. Pinned chat conversations live in the Pinned section, so
 *   they are excluded from `chatConversations`.
 */
export function buildRows(args: {
  pinned: readonly DbConversationSummary[]
  pinnedExpanded: boolean
  orderedFolderIds: readonly number[]
  byFolder: Map<number, DbConversationSummary[]>
  folderExpanded: Record<number, boolean>
  folderTotalCounts: Map<number, number>
  foldersExpanded: boolean
  chatConversations: readonly DbConversationSummary[]
  chatsExpanded: boolean
}): SidebarRow[] {
  const {
    pinned,
    pinnedExpanded,
    orderedFolderIds,
    byFolder,
    folderExpanded,
    folderTotalCounts,
    foldersExpanded,
    chatConversations,
    chatsExpanded,
  } = args
  const rows: SidebarRow[] = []

  if (pinned.length > 0) {
    rows.push({
      kind: "section",
      section: "pinned",
      expanded: pinnedExpanded,
      count: pinned.length,
    })
    if (pinnedExpanded) {
      for (const conv of pinned) {
        rows.push({ kind: "conversation", conversation: conv })
      }
    }
  }

  if (orderedFolderIds.length > 0) {
    rows.push({
      kind: "section",
      section: "folders",
      expanded: foldersExpanded,
      count: orderedFolderIds.length,
    })
    if (foldersExpanded) {
      for (const folderId of orderedFolderIds) {
        rows.push({ kind: "folder", folderId })
        const expanded = folderExpanded[folderId] ?? true
        if (!expanded) continue
        const convs = byFolder.get(folderId)
        if (!convs || convs.length === 0) {
          rows.push({
            kind: "empty",
            folderId,
            totalConversationCount: folderTotalCounts.get(folderId) ?? 0,
          })
          continue
        }
        for (const conv of convs) {
          rows.push({ kind: "conversation", conversation: conv })
        }
      }
    }
  }

  // The Chat section header is always present (a permanent entry point), unlike
  // the conditional Pinned/Folders headers above.
  rows.push({
    kind: "section",
    section: "chats",
    expanded: chatsExpanded,
    count: chatConversations.length,
  })
  if (chatsExpanded) {
    if (chatConversations.length === 0) {
      rows.push({ kind: "chats-empty" })
    } else {
      for (const conv of chatConversations) {
        rows.push({ kind: "conversation", conversation: conv })
      }
    }
  }

  return rows
}

/**
 * Flat index of the conversation row for `(id, agentType)`, or -1 if absent
 * (folder collapsed, filtered out, or unknown). Used by `scrollToActive` to
 * drive `VirtualizerHandle.scrollToIndex` — off-screen virtualized rows are not
 * in the DOM, so a querySelector-based lookup no longer works.
 */
export function flatIndexOfConversation(
  rows: readonly SidebarRow[],
  id: number,
  agentType: string
): number {
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i]
    if (
      row.kind === "conversation" &&
      row.conversation.id === id &&
      row.conversation.agent_type === agentType
    ) {
      return i
    }
  }
  return -1
}

// ── Folder drag index math (Phase 2 custom pointer reorder) ──────────────────

/**
 * Map a pointer's Y position over the (fixed row height) collapsed drag surface
 * to a target folder slot, clamped to `[0, count - 1]`.
 *
 * @param pointerY   `clientY` of the pointer
 * @param surfaceTop `getBoundingClientRect().top` of the scroll viewport
 * @param scrollTop  current scroll offset of the viewport
 * @param rowHeight  height of one folder header row in px (fixed, 32)
 * @param count      number of folder rows on the surface
 */
export function pointerYToTargetIndex(
  pointerY: number,
  surfaceTop: number,
  scrollTop: number,
  rowHeight: number,
  count: number
): number {
  if (count <= 0) return 0
  if (rowHeight <= 0) return 0
  const raw = Math.floor((pointerY - surfaceTop + scrollTop) / rowHeight)
  return Math.max(0, Math.min(count - 1, raw))
}

/**
 * Move the item at `from` to `to`, returning a new array. Out-of-range indices
 * are clamped; a no-op move still returns a fresh array copy.
 */
export function applyReorder<T>(
  order: readonly T[],
  from: number,
  to: number
): T[] {
  const next = order.slice()
  if (from < 0 || from >= next.length) return next
  const clampedTo = Math.max(0, Math.min(next.length - 1, to))
  if (from === clampedTo) return next
  const [moved] = next.splice(from, 1)
  next.splice(clampedTo, 0, moved)
  return next
}

// ── Sticky folder header (floating overlay) ─────────────────────────────────
// virtua renders every row as `position:absolute; top:<offset>` inside a
// `contain:strict` container and unmounts off-screen rows, so CSS
// `position:sticky` cannot pin a folder header. Instead a single floating
// overlay stands in for the folder currently scrolled through. These pure
// helpers resolve "which folder" and the iOS-style handoff offset from the
// virtua handle's measured pixel offsets — see the wiring in
// `SidebarConversationList`.

/**
 * For every flat row, the index of the folder header that owns it: a folder
 * header owns itself; a conversation/empty row owns the nearest folder header
 * above it (or -1 if none precedes it, which `buildRows` never produces). Lets
 * the scroll handler resolve the active folder in O(1) from the topmost visible
 * row index, instead of an O(folder span) backward scan that would jank in very
 * large folders.
 */
export function buildOwnerHeaderIndex(rows: readonly SidebarRow[]): Int32Array {
  const out = new Int32Array(rows.length)
  let current = -1
  for (let i = 0; i < rows.length; i++) {
    if (rows[i].kind === "folder") current = i
    out[i] = current
  }
  return out
}

/** Flat indices of every folder header row, in ascending order. */
export function folderHeaderFlatIndices(rows: readonly SidebarRow[]): number[] {
  const indices: number[] = []
  for (let i = 0; i < rows.length; i++) {
    if (rows[i].kind === "folder") indices.push(i)
  }
  return indices
}

/**
 * The next folder header flat index strictly after `activeHeaderIndex`, or
 * `null` when `activeHeaderIndex` is the last folder. `headerIndices` must be
 * ascending (as produced by {@link folderHeaderFlatIndices}).
 */
export function nextHeaderAfter(
  headerIndices: readonly number[],
  activeHeaderIndex: number
): number | null {
  for (let i = 0; i < headerIndices.length; i++) {
    if (headerIndices[i] > activeHeaderIndex) return headerIndices[i]
  }
  return null
}

/**
 * Flat index of the folder header row for `folderId`, or -1 if absent. Used
 * after a collapse-from-overlay toggle to scroll that header to the top.
 */
export function headerIndexForFolder(
  rows: readonly SidebarRow[],
  folderId: number
): number {
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i]
    if (row.kind === "folder" && row.folderId === folderId) return i
  }
  return -1
}

/**
 * Pure geometry for the floating sticky folder header. All inputs are measured
 * pixel offsets from the virtua handle; no DOM access.
 *
 * - `visible`: the active folder's own header has scrolled above the viewport
 *   top, so the overlay should stand in for it. (At the very top, where
 *   `scrollOffset === activeHeaderOffset`, the real header is shown instead.)
 * - `translateY`: iOS-style handoff — once the next folder's header is within
 *   one header height of the top it pushes the overlay up so the incoming header
 *   displaces it. Rounded to whole pixels to avoid sub-pixel shimmer against the
 *   real (still-mounted within the buffer) header underneath.
 */
export function computeStickyState(args: {
  scrollOffset: number
  activeHeaderOffset: number
  nextHeaderOffset: number | null
  headerHeight: number
}): { visible: boolean; translateY: number } {
  const { scrollOffset, activeHeaderOffset, nextHeaderOffset, headerHeight } =
    args
  const visible = scrollOffset > activeHeaderOffset
  let translateY = 0
  if (visible && nextHeaderOffset != null) {
    const d = nextHeaderOffset - scrollOffset
    if (d >= 0 && d < headerHeight) {
      translateY = Math.round(d - headerHeight)
    }
  }
  return { visible, translateY }
}
