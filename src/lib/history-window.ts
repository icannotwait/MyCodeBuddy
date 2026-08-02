import type { GetFolderConversationOptions } from "@/lib/api"
import type { DbConversationDetail, MessageTurn } from "@/lib/types"

/** Default user-turn window for cold open / refetch (mirrors Rust default). */
export const DEFAULT_HISTORY_USER_TURN_LIMIT = 20

/** Count user-role turns in a list (history window unit). */
export function countUserTurns(
  turns: readonly MessageTurn[] | null | undefined
): number {
  if (!turns || turns.length === 0) return 0
  let n = 0
  for (const t of turns) {
    if (t.role === "user") n += 1
  }
  return n
}

/**
 * Limit for refetch/reload: at least the default window, expanded to cover
 * any older history the client already loaded so refetch does not drop it.
 */
export function historyFetchLimitForSession(
  detail: DbConversationDetail | null | undefined
): number {
  const loaded = countUserTurns(detail?.turns)
  return Math.max(DEFAULT_HISTORY_USER_TURN_LIMIT, loaded)
}

/** Options for a cold open (tail window only). */
export function coldHistoryFetchOptions(): GetFolderConversationOptions {
  return { historyUserTurnLimit: DEFAULT_HISTORY_USER_TURN_LIMIT }
}

/** Options for refetch/reload that preserve already-loaded older history. */
export function refetchHistoryFetchOptions(
  detail: DbConversationDetail | null | undefined,
  reservePendingOlderPage = false,
  additionalUserTurns = 0
): GetFolderConversationOptions {
  const loadedLimit = historyFetchLimitForSession(detail)
  let historyUserTurnLimit = loadedLimit + Math.max(0, additionalUserTurns)
  if (reservePendingOlderPage) {
    historyUserTurnLimit += DEFAULT_HISTORY_USER_TURN_LIMIT
  }
  return { historyUserTurnLimit }
}

/** Options for a "load older" page strictly before the current oldest turn. */
export function loadOlderHistoryFetchOptions(
  oldestTurnId: string
): GetFolderConversationOptions {
  return {
    historyUserTurnLimit: DEFAULT_HISTORY_USER_TURN_LIMIT,
    historyBeforeTurnId: oldestTurnId,
  }
}

/**
 * Preserve the loaded left boundary when an append-only transcript advances
 * remotely between windowed refetches. The incoming suffix remains
 * authoritative; only the stable prefix before its first overlapping turn is
 * retained.
 */
export function preserveLoadedHistoryOnRefetch(
  current: DbConversationDetail | null | undefined,
  incoming: DbConversationDetail
): DbConversationDetail {
  const incomingWindow = incoming.history_window
  if (!current?.turns.length || !incomingWindow) return incoming

  const previousTotalUserTurns =
    current.history_window?.total_user_turn_count ??
    countUserTurns(current.turns)
  if (incomingWindow.total_user_turn_count <= previousTotalUserTurns) {
    return incoming
  }

  const incomingIds = new Set(incoming.turns.map((turn) => turn.id))
  if (incomingIds.has(current.turns[0].id)) return incoming

  const firstCoveredIndex = current.turns.findIndex((turn) =>
    incomingIds.has(turn.id)
  )
  const prefix =
    firstCoveredIndex >= 0
      ? current.turns.slice(0, firstCoveredIndex)
      : current.turns
  if (prefix.length === 0) return incoming

  const turns = [...prefix, ...incoming.turns]
  const returnedUserTurnCount = countUserTurns(turns)
  return {
    ...incoming,
    summary: {
      ...incoming.summary,
      message_count: Math.max(
        current.summary.message_count,
        incoming.summary.message_count,
        incomingWindow.total_turn_count
      ),
    },
    turns,
    history_window: {
      ...incomingWindow,
      has_more_before: current.history_window?.has_more_before ?? false,
      user_turn_limit: Math.max(
        current.history_window?.user_turn_limit ?? 0,
        incomingWindow.user_turn_limit,
        returnedUserTurnCount
      ),
      returned_user_turn_count: returnedUserTurnCount,
    },
  }
}

/**
 * Prepend an older page onto an existing detail without duplicating turn ids.
 * Updates `history_window` from the page response.
 */
export function prependHistoryPage(
  current: DbConversationDetail,
  page: DbConversationDetail
): DbConversationDetail {
  const existingIds = new Set(current.turns.map((t) => t.id))
  const older = page.turns.filter((t) => !existingIds.has(t.id))
  if (older.length === 0) {
    const returnedUserTurnCount = countUserTurns(current.turns)
    return {
      ...current,
      history_window: page.history_window
        ? {
            ...page.history_window,
            // Keep totals from page (authoritative full-transcript stats).
            has_more_before: page.history_window.has_more_before,
            returned_user_turn_count: returnedUserTurnCount,
            user_turn_limit: Math.max(
              page.history_window.user_turn_limit,
              current.history_window?.user_turn_limit ?? 0,
              returnedUserTurnCount
            ),
          }
        : current.history_window,
    }
  }
  const mergedTurns = [...older, ...current.turns]
  const pageWindow = page.history_window
  const returnedUserTurnCount = countUserTurns(mergedTurns)
  return {
    ...current,
    turns: mergedTurns,
    // Prefer full-transcript stats from the page response when present.
    summary: {
      ...current.summary,
      message_count: Math.max(
        current.summary.message_count,
        page.summary.message_count,
        pageWindow?.total_turn_count ?? 0
      ),
    },
    history_window: pageWindow
      ? {
          ...pageWindow,
          has_more_before: pageWindow.has_more_before,
          returned_user_turn_count: returnedUserTurnCount,
          user_turn_limit: Math.max(
            pageWindow.user_turn_limit,
            current.history_window?.user_turn_limit ?? 0,
            returnedUserTurnCount
          ),
        }
      : {
          has_more_before: false,
          total_turn_count: mergedTurns.length,
          total_user_turn_count: returnedUserTurnCount,
          user_turn_limit: 0,
          returned_user_turn_count: returnedUserTurnCount,
        },
  }
}
