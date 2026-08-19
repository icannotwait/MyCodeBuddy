import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { parseDelegationMeta } from "@/lib/delegation-card"
import type { ConversationChange, DbConversationSummary } from "@/lib/types"

const mocks = vi.hoisted(() => ({
  listChildConversations: vi.fn(),
  subscribe: vi.fn(),
  onTransportReconnect: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  listChildConversations: mocks.listChildConversations,
}))

vi.mock("@/lib/platform", () => ({
  subscribe: mocks.subscribe,
  onTransportReconnect: mocks.onTransportReconnect,
}))

import { useDurableDelegationSources } from "@/hooks/use-durable-delegation-sources"

function child(
  overrides: Partial<DbConversationSummary> & Pick<DbConversationSummary, "id">
): DbConversationSummary {
  return {
    folder_id: 1,
    title: `child-${overrides.id}`,
    title_locked: false,
    auto_title_finalized: false,
    agent_type: "codex",
    status: "pending_review",
    awaiting_reply_token: null,
    kind: "delegate",
    model: null,
    git_branch: null,
    external_id: null,
    message_count: 0,
    child_count: 0,
    created_at: "2026-08-19T11:30:08.000Z",
    updated_at: "2026-08-19T11:41:46.000Z",
    pinned_at: null,
    parent_id: 3866,
    parent_tool_use_id: `exec-${overrides.id}`,
    delegation_call_id: `task-${overrides.id}`,
    delegation_task_status: "completed",
    ...overrides,
  }
}

describe("useDurableDelegationSources", () => {
  let changeHandler: ((change: ConversationChange) => void) | null = null

  beforeEach(() => {
    changeHandler = null
    mocks.listChildConversations.mockReset()
    mocks.subscribe.mockReset()
    mocks.onTransportReconnect.mockReset()
    mocks.listChildConversations.mockResolvedValue([])
    mocks.subscribe.mockImplementation(async (_event, handler) => {
      changeHandler = handler
      return () => {}
    })
    mocks.onTransportReconnect.mockImplementation(() => () => {})
  })

  it("loads durable children even when the transcript has no delegate cards", async () => {
    mocks.listChildConversations.mockResolvedValue([
      child({ id: 3868, parent_tool_use_id: "exec-newer" }),
      child({
        id: 3867,
        parent_tool_use_id: "exec-older",
        created_at: "2026-08-19T11:30:00.000Z",
        delegation_started_at: "2026-08-19T11:30:00.000Z",
      }),
    ])

    const { result } = renderHook(() => useDurableDelegationSources(3866))

    await waitFor(() => {
      expect(result.current.map((row) => row.parentToolUseId)).toEqual([
        "exec-older",
        "exec-newer",
      ])
    })
    expect(mocks.listChildConversations).toHaveBeenCalledWith(3866)
  })

  it("refetches when a child of this parent is upserted", async () => {
    mocks.listChildConversations
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([
        child({ id: 3879, delegation_task_status: "running" }),
      ])

    const { result } = renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(result.current).toEqual([]))

    await act(async () => {
      changeHandler?.({
        kind: "upsert",
        summary: child({ id: 3879, delegation_task_status: "running" }),
      })
    })

    await waitFor(() => {
      expect(result.current).toHaveLength(1)
      expect(result.current[0].parentToolUseId).toBe("exec-3879")
    })
  })

  it("does not refetch for an unrelated conversation upsert", async () => {
    mocks.listChildConversations.mockResolvedValue([])
    renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() =>
      expect(mocks.listChildConversations).toHaveBeenCalledTimes(1)
    )

    await act(async () => {
      changeHandler?.({
        kind: "upsert",
        summary: child({ id: 99, parent_id: 12 }),
      })
    })

    expect(mocks.listChildConversations).toHaveBeenCalledTimes(1)
  })

  it("subscribes before the first child list fetch", async () => {
    const order: string[] = []
    mocks.subscribe.mockImplementation(async (_event, handler) => {
      order.push("subscribe")
      changeHandler = handler
      return () => {}
    })
    mocks.listChildConversations.mockImplementation(async () => {
      order.push("list")
      return []
    })

    renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(order).toEqual(["subscribe", "list"]))
  })

  it("refetches when this parent conversation is upserted", async () => {
    mocks.listChildConversations
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([child({ id: 3867 })])

    const { result } = renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(result.current).toEqual([]))

    await act(async () => {
      changeHandler?.({
        kind: "upsert",
        summary: child({
          id: 3866,
          parent_id: null,
          kind: "regular",
          parent_tool_use_id: null,
          delegation_call_id: null,
        }),
      })
    })

    await waitFor(() => expect(result.current).toHaveLength(1))
  })

  it("refetches when a previously loaded child is deleted", async () => {
    mocks.listChildConversations
      .mockResolvedValueOnce([child({ id: 3867 }), child({ id: 3868 })])
      .mockResolvedValueOnce([child({ id: 3868 })])

    const { result } = renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(result.current).toHaveLength(2))

    await act(async () => {
      changeHandler?.({ kind: "deleted", id: 3867 })
    })

    await waitFor(() => {
      expect(result.current.map((row) => row.parentToolUseId)).toEqual([
        "exec-3868",
      ])
    })
  })

  it("refetches when a loaded child receives a state patch", async () => {
    mocks.listChildConversations
      .mockResolvedValueOnce([
        child({ id: 3879, delegation_task_status: "running" }),
      ])
      .mockResolvedValueOnce([
        child({ id: 3879, delegation_task_status: "completed" }),
      ])

    const { result } = renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(result.current).toHaveLength(1))

    await act(async () => {
      changeHandler?.({
        kind: "state",
        patch: {
          id: 3879,
          status: "pending_review",
          awaiting_reply_token: null,
          updated_at: "2026-08-19T12:00:00.000Z",
        },
      })
    })

    await waitFor(() => {
      expect(parseDelegationMeta(result.current[0]?.meta)?.status).toBe("ok")
    })
  })

  it("does not refetch for a state patch of an unknown conversation", async () => {
    mocks.listChildConversations.mockResolvedValue([])
    renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() =>
      expect(mocks.listChildConversations).toHaveBeenCalledTimes(1)
    )

    await act(async () => {
      changeHandler?.({
        kind: "state",
        patch: {
          id: 99,
          status: "pending_review",
          awaiting_reply_token: null,
          updated_at: "2026-08-19T12:00:00.000Z",
        },
      })
    })

    expect(mocks.listChildConversations).toHaveBeenCalledTimes(1)
  })

  it("keeps the last successful snapshot when a refresh fails", async () => {
    mocks.listChildConversations
      .mockResolvedValueOnce([child({ id: 3867 })])
      .mockRejectedValueOnce(new Error("offline"))

    const { result } = renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(result.current).toHaveLength(1))

    await act(async () => {
      changeHandler?.({
        kind: "upsert",
        summary: child({ id: 3868 }),
      })
    })

    await waitFor(() =>
      expect(mocks.listChildConversations).toHaveBeenCalledTimes(2)
    )
    expect(result.current).toHaveLength(1)
    expect(result.current[0]?.parentToolUseId).toBe("exec-3867")
  })

  it("does not expose the previous conversation's children while the next id loads", async () => {
    mocks.listChildConversations.mockImplementation(async (id: number) => {
      if (id === 3866) return [child({ id: 3867 })]
      return []
    })

    const { result, rerender } = renderHook(
      ({ id }) => useDurableDelegationSources(id),
      { initialProps: { id: 3866 } }
    )
    await waitFor(() => expect(result.current).toHaveLength(1))

    rerender({ id: 12 })
    expect(result.current).toEqual([])
    await waitFor(() =>
      expect(mocks.listChildConversations).toHaveBeenCalledWith(12)
    )
  })

  it("refetches after a transport reconnect", async () => {
    let reconnect: (() => void) | null = null
    mocks.onTransportReconnect.mockImplementation((callback: () => void) => {
      reconnect = callback
      return () => {
        reconnect = null
      }
    })
    mocks.listChildConversations
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([child({ id: 3867 })])

    const { result } = renderHook(() => useDurableDelegationSources(3866))
    await waitFor(() => expect(result.current).toEqual([]))

    await act(async () => {
      reconnect?.()
    })

    await waitFor(() => expect(result.current).toHaveLength(1))
  })
})
