import { describe, expect, it } from "vitest"
import {
  coldHistoryFetchOptions,
  countUserTurns,
  historyFetchLimitForSession,
  loadOlderHistoryFetchOptions,
  prependHistoryPage,
  refetchHistoryFetchOptions,
  DEFAULT_HISTORY_USER_TURN_LIMIT,
} from "@/lib/history-window"
import type { DbConversationDetail, MessageTurn } from "@/lib/types"

function turn(id: string, role: MessageTurn["role"]): MessageTurn {
  return {
    id,
    role,
    blocks: [],
    timestamp: new Date().toISOString(),
  }
}

function detail(turns: MessageTurn[]): DbConversationDetail {
  return {
    summary: {
      id: 1,
      folder_id: 1,
      title: "t",
      title_locked: false,
      auto_title_finalized: true,
      agent_type: "codex",
      status: "idle",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "ext",
      message_count: turns.length,
      child_count: 0,
      created_at: "2026-05-28T00:00:00.000Z",
      updated_at: "2026-05-28T00:00:00.000Z",
      pinned_at: null,
    },
    turns,
    history_window: {
      has_more_before: true,
      total_turn_count: 100,
      total_user_turn_count: 40,
      user_turn_limit: DEFAULT_HISTORY_USER_TURN_LIMIT,
      returned_user_turn_count: countUserTurns(turns),
    },
  }
}

describe("history-window helpers", () => {
  it("counts user turns only", () => {
    expect(
      countUserTurns([
        turn("u1", "user"),
        turn("a1", "assistant"),
        turn("u2", "user"),
      ])
    ).toBe(2)
  })

  it("cold fetch uses default limit", () => {
    expect(coldHistoryFetchOptions()).toEqual({
      historyUserTurnLimit: DEFAULT_HISTORY_USER_TURN_LIMIT,
    })
  })

  it("refetch expands limit to loaded user turns", () => {
    const turns = Array.from({ length: 25 }, (_, i) => turn(`u${i}`, "user"))
    expect(refetchHistoryFetchOptions(detail(turns))).toEqual({
      historyUserTurnLimit: 25,
    })
    expect(historyFetchLimitForSession(null)).toBe(
      DEFAULT_HISTORY_USER_TURN_LIMIT
    )
  })

  it("refetch reserves one page when load-older is still in flight", () => {
    const turns = Array.from({ length: 25 }, (_, i) => turn(`u${i}`, "user"))
    expect(refetchHistoryFetchOptions(detail(turns), true)).toEqual({
      historyUserTurnLimit: 45,
    })
  })

  it("refetch reserves newly-sent user turns beyond the loaded detail", () => {
    const turns = Array.from({ length: 25 }, (_, i) => turn(`u${i}`, "user"))
    expect(refetchHistoryFetchOptions(detail(turns), false, 2)).toEqual({
      historyUserTurnLimit: 27,
    })
  })

  it("load older uses before cursor", () => {
    expect(loadOlderHistoryFetchOptions("turn-1")).toEqual({
      historyUserTurnLimit: DEFAULT_HISTORY_USER_TURN_LIMIT,
      historyBeforeTurnId: "turn-1",
    })
  })

  it("prepends older page without duplicating ids", () => {
    const current = detail([
      turn("u2", "user"),
      turn("a2", "assistant"),
      turn("u3", "user"),
    ])
    const page = detail([
      turn("u1", "user"),
      turn("a1", "assistant"),
      turn("u2", "user"),
    ])
    page.history_window = {
      has_more_before: false,
      total_turn_count: 10,
      total_user_turn_count: 3,
      user_turn_limit: 20,
      returned_user_turn_count: 2,
    }
    const merged = prependHistoryPage(current, page)
    expect(merged.turns.map((t) => t.id)).toEqual([
      "u1",
      "a1",
      "u2",
      "a2",
      "u3",
    ])
    expect(merged.history_window?.has_more_before).toBe(false)
    expect(merged.history_window?.returned_user_turn_count).toBe(3)
  })

  it("reports the effective merged limit after prepending a page", () => {
    const current = detail(
      Array.from({ length: 25 }, (_, i) => turn(`u${i}`, "user"))
    )
    current.history_window!.user_turn_limit = 25
    const page = detail([turn("older", "user")])
    page.history_window = {
      has_more_before: false,
      total_turn_count: 26,
      total_user_turn_count: 26,
      user_turn_limit: 20,
      returned_user_turn_count: 1,
    }

    const merged = prependHistoryPage(current, page)

    expect(merged.history_window?.returned_user_turn_count).toBe(26)
    expect(merged.history_window?.user_turn_limit).toBe(26)
  })

  it("keeps merged metadata consistent for a duplicate-only page", () => {
    const current = detail(
      Array.from({ length: 25 }, (_, i) => turn(`u${i}`, "user"))
    )
    current.history_window!.user_turn_limit = 25
    const page = detail([turn("u0", "user")])
    page.history_window = {
      has_more_before: false,
      total_turn_count: 25,
      total_user_turn_count: 25,
      user_turn_limit: 20,
      returned_user_turn_count: 1,
    }

    const merged = prependHistoryPage(current, page)

    expect(merged.history_window?.returned_user_turn_count).toBe(25)
    expect(merged.history_window?.user_turn_limit).toBe(25)
  })
})
