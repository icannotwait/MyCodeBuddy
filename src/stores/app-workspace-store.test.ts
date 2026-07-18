import { beforeEach, describe, expect, it } from "vitest"
import {
  resetAppWorkspaceStore,
  useAppWorkspaceStore,
} from "./app-workspace-store"
import type { DbConversationSummary } from "@/lib/types"

function makeSummary(
  overrides: Partial<DbConversationSummary> & { id: number }
): DbConversationSummary {
  return {
    folder_id: 1,
    title: null,
    title_locked: false,
    agent_type: "claude_code",
    status: "in_progress",
    awaiting_reply_token: null,
    kind: "regular",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
    pinned_at: null,
    parent_id: null,
    parent_tool_use_id: null,
    delegation_call_id: null,
    ...overrides,
  }
}

beforeEach(() => {
  resetAppWorkspaceStore()
})

describe("updateConversationLocal — stats reference stability", () => {
  function seedTwo() {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1, message_count: 3 }))
    store.applyConversationUpsert(makeSummary({ id: 2, message_count: 4 }))
  }

  it("reuses the stats reference on a status patch (no stat can change)", () => {
    seedTwo()
    const before = useAppWorkspaceStore.getState()
    const statsBefore = before.stats
    const conversationsBefore = before.conversations

    useAppWorkspaceStore
      .getState()
      .updateConversationLocal(1, { status: "pending_review" })

    const after = useAppWorkspaceStore.getState()
    // The regression guard: a turn-boundary status flip must NOT mint a fresh
    // `stats` object (which would re-render every stats subscriber for a no-op).
    expect(after.stats).toBe(statsBefore)
    // But the row's data genuinely changed, so `conversations` gets a new ref
    // (sidebar consumers must see the status update).
    expect(after.conversations).not.toBe(conversationsBefore)
    expect(after.conversations.find((c) => c.id === 1)?.status).toBe(
      "pending_review"
    )
  })

  it("reuses the stats reference on a title patch", () => {
    seedTwo()
    const statsBefore = useAppWorkspaceStore.getState().stats

    useAppWorkspaceStore
      .getState()
      .updateConversationLocal(2, { title: "Renamed" })

    const after = useAppWorkspaceStore.getState()
    expect(after.stats).toBe(statsBefore)
    expect(after.conversations.find((c) => c.id === 2)?.title).toBe("Renamed")
  })

  it("leaves state untouched (stable refs) for an unknown id", () => {
    seedTwo()
    const before = useAppWorkspaceStore.getState()

    before.updateConversationLocal(999, { status: "cancelled" })

    const after = useAppWorkspaceStore.getState()
    expect(after.stats).toBe(before.stats)
    expect(after.conversations).toBe(before.conversations)
  })

  it("still tracks stats when message_count actually changes (via upsert)", () => {
    seedTwo()
    // total_messages = 3 + 4
    expect(useAppWorkspaceStore.getState().stats?.total_messages).toBe(7)

    // A real message_count change flows through applyConversationUpsert (whose
    // recompute we intentionally left intact), so stats update as before.
    useAppWorkspaceStore
      .getState()
      .applyConversationUpsert(makeSummary({ id: 1, message_count: 10 }))

    expect(useAppWorkspaceStore.getState().stats?.total_messages).toBe(14)
  })
})

describe("applyConversationStatePatch — backend authority exactness", () => {
  it("applies backend conversation state without inventing updated_at", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({
        id: 1,
        status: "in_progress",
        awaiting_reply_token: null,
        updated_at: "2026-07-16T01:00:00.000Z",
      })
    )
    const statsBefore = useAppWorkspaceStore.getState().stats

    store.applyConversationStatePatch({
      id: 1,
      status: "pending_review",
      awaiting_reply_token: "generation-b",
      updated_at: "2026-07-16T02:03:04.000Z",
    })

    const state = useAppWorkspaceStore.getState()
    expect(state.conversations[0]).toMatchObject({
      status: "pending_review",
      awaiting_reply_token: "generation-b",
      updated_at: "2026-07-16T02:03:04.000Z",
    })
    expect(state.stats).toBe(statsBefore)
  })

  it("ignores a state patch for an unknown conversation", () => {
    const before = useAppWorkspaceStore.getState()
    before.applyConversationStatePatch({
      id: 999,
      status: "pending_review",
      awaiting_reply_token: "unknown",
      updated_at: "2026-07-16T02:03:04.000Z",
    })
    expect(useAppWorkspaceStore.getState().conversations).toBe(
      before.conversations
    )
  })
})

describe("applyGitHead", () => {
  it("apply_git_head_updates_when_full_head_or_reference_epoch_changes_on_the_same_branch", () => {
    const store = useAppWorkspaceStore.getState()
    const first = {
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: null as string | null,
      canonical_repo: "/repo",
      head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      reference_source_epoch: "v1:epoch-a",
    }
    store.applyGitHead(1, first)
    expect(useAppWorkspaceStore.getState().gitHeads.get(1)).toEqual(first)

    const second = {
      ...first,
      head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      reference_source_epoch: "v1:epoch-b",
    }
    store.applyGitHead(1, second)
    expect(useAppWorkspaceStore.getState().gitHeads.get(1)).toEqual(second)
  })
})

describe("optimistic conversation activity", () => {
  it("does not invent updated_at for local title or status patches", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1 }))
    // Re-read after upsert: Zustand snapshots do not refresh row arrays.
    const baseline = useAppWorkspaceStore.getState().conversations[0].updated_at

    store.updateConversationLocal(1, { title: "Renamed" })
    store.updateConversationLocal(1, { status: "pending_review" })

    expect(useAppWorkspaceStore.getState().conversations[0].updated_at).toBe(
      baseline
    )
  })

  it("rolls back only the matching optimistic token", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(makeSummary({ id: 1 }))
    const first = store.beginConversationActivity(1)!
    const second = store.beginConversationActivity(1)!

    store.rollbackConversationActivity(1, first)
    expect(
      useAppWorkspaceStore.getState().optimisticActivityById.get(1)?.token
    ).toBe(second)

    store.rollbackConversationActivity(1, second)
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(1)).toBe(
      false
    )
  })

  it("ignores older state and acknowledges activity only past its baseline", () => {
    const store = useAppWorkspaceStore.getState()
    store.applyConversationUpsert(
      makeSummary({ id: 1, updated_at: "2026-07-18T02:00:00.000Z" })
    )
    const token = store.beginConversationActivity(1)
    expect(token).not.toBeNull()
    const sequence =
      useAppWorkspaceStore.getState().conversationActivitySequence

    store.applyConversationStatePatch({
      id: 1,
      status: "cancelled",
      awaiting_reply_token: null,
      updated_at: "2026-07-18T01:00:00.000Z",
    })
    expect(useAppWorkspaceStore.getState().conversations[0].status).toBe(
      "in_progress"
    )
    expect(useAppWorkspaceStore.getState().optimisticActivityById.has(1)).toBe(
      true
    )

    store.applyConversationStatePatch({
      id: 1,
      status: "pending_review",
      awaiting_reply_token: "generation-1",
      updated_at: "2026-07-18T03:00:00.000Z",
    })
    const after = useAppWorkspaceStore.getState()
    expect(after.optimisticActivityById.has(1)).toBe(false)
    expect(after.conversationActivitySequence).toBe(sequence + 1)
    expect(after.lastConversationActivityId).toBe(1)
  })

  it("returns null for unknown or non-root conversations", () => {
    const store = useAppWorkspaceStore.getState()
    expect(store.beginConversationActivity(999)).toBeNull()

    // Bypass root-only upsert so a child row can exist in state.
    useAppWorkspaceStore.setState({
      conversations: [makeSummary({ id: 2, parent_id: 1 })],
    })
    expect(
      useAppWorkspaceStore.getState().beginConversationActivity(2)
    ).toBeNull()
  })
})
