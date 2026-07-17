import { afterEach, describe, expect, it } from "vitest"
import type {
  LiveContentBlock,
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import type {
  DbConversationDetail,
  MessageTurn,
  SessionStats,
} from "@/lib/types"
import type { BackgroundOverlayEntry } from "@/stores/conversation-runtime-store"
import {
  buildStreamingTurnsFromLiveMessage,
  resetConversationRuntimeStore,
  selectDelegationActivities,
  selectHistoricalTimelineTurns,
  selectTimelineTurns,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"

const CID = 42
const OTHER_CID = 99

function userTurn(
  id: string,
  text = id,
  timestamp = "2026-05-28T00:00:00.000Z"
): MessageTurn {
  return {
    id,
    role: "user",
    blocks: [{ type: "text", text }],
    timestamp,
  }
}

function assistantTurn(
  id: string,
  text = id,
  timestamp = "2026-05-28T00:00:01.000Z"
): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks: [{ type: "text", text }],
    timestamp,
  }
}

function detailWithTurns(
  turns: MessageTurn[],
  overrides: Partial<DbConversationDetail> = {}
): DbConversationDetail {
  return {
    summary: {
      id: CID,
      folder_id: 1,
      agent_type: "codex",
      title: "t",
      title_locked: false,
      status: "in_progress",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "sid-1",
      message_count: turns.length,
      child_count: 0,
      created_at: "2026-05-28T00:00:00.000Z",
      updated_at: "2026-05-28T00:00:00.000Z",
      pinned_at: null,
    },
    turns,
    session_stats: null,
    ...overrides,
  }
}

function liveMessage(
  id: string,
  text: string,
  startedAt = 1_700_000_000_000
): LiveMessage {
  return {
    id,
    role: "assistant",
    content: [{ type: "text", text }],
    startedAt,
  }
}

type SeedInput = {
  detail?: DbConversationDetail | null
  localTurns?: MessageTurn[]
  backgroundTurns?: BackgroundOverlayEntry[]
  optimisticTurns?: MessageTurn[]
  liveMessage?: LiveMessage | null
  liveOwnsActiveTurn?: boolean
  delegationKickoffText?: string | null
  sessionStats?: SessionStats | null
  syncState?: "idle" | "awaiting_persist"
}

function seedRuntimeSession(input: SeedInput = {}) {
  useConversationRuntimeStore.setState({
    byConversationId: new Map([
      [
        CID,
        {
          conversationId: CID,
          externalId: "sid-1",
          dbConversationId: null,
          detail: input.detail ?? null,
          detailLoading: false,
          detailError: null,
          acpLoadError: null,
          localTurns: input.localTurns ?? [],
          backgroundTurns: input.backgroundTurns ?? [],
          optimisticTurns: input.optimisticTurns ?? [],
          liveMessage: input.liveMessage ?? null,
          syncState: input.syncState ?? "idle",
          activeTurnToken: null,
          liveOwnsActiveTurn: input.liveOwnsActiveTurn ?? false,
          delegationKickoffText: input.delegationKickoffText ?? null,
          sessionStats: input.sessionStats ?? null,
          pendingCleanup: false,
        },
      ],
    ]),
  })
}

function baseSeed(): SeedInput {
  return {
    detail: detailWithTurns([userTurn("u1"), assistantTurn("a1")]),
    optimisticTurns: [userTurn("u2")],
  }
}

function mutateHistoricalInput(
  kind: "detail" | "local" | "background" | "optimistic"
) {
  const actions = useConversationRuntimeStore.getState().actions
  switch (kind) {
    case "detail": {
      const current = useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)!
      useConversationRuntimeStore.setState({
        byConversationId: new Map([
          [
            CID,
            {
              ...current,
              detail: detailWithTurns([
                userTurn("u1"),
                assistantTurn("a1"),
                userTurn("u-new"),
              ]),
            },
          ],
        ]),
      })
      break
    }
    case "local":
      actions.completeTurn(CID, liveMessage("promoted", "done"))
      break
    case "background":
      actions.applyBackgroundActivity(
        CID,
        [assistantTurn("bg-1", "bg", "2026-05-28T00:00:02.000Z")],
        100
      )
      break
    case "optimistic":
      actions.appendOptimisticTurn(CID, userTurn("u3"), "tok-3")
      break
  }
}

afterEach(() => {
  resetConversationRuntimeStore()
})

describe("selectHistoricalTimelineTurns reference stability", () => {
  it("keeps historical arrays and entries identical across 500 live appends", () => {
    // Seed with live already started — identity/start is a one-shot cache
    // invalidation; these 500 iterations are content-only replacements of the
    // same live id/startedAt and must keep historical array + entry refs.
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("u1"), assistantTurn("a1")]),
      optimisticTurns: [userTurn("u2")],
      liveMessage: liveMessage("live-1", "x"),
    })
    const stateBefore = useConversationRuntimeStore.getState()
    const before = selectHistoricalTimelineTurns(stateBefore, CID)

    for (let index = 0; index < 500; index += 1) {
      useConversationRuntimeStore
        .getState()
        .actions.setLiveMessage(
          CID,
          liveMessage("live-1", "x".repeat(index + 1)),
          true
        )
      const current = selectHistoricalTimelineTurns(
        useConversationRuntimeStore.getState(),
        CID
      )
      expect(current).toBe(before)
      expect(current[0]).toBe(before[0])
      expect(current[1]).toBe(before[1])
    }
  })

  it.each(["detail", "local", "background", "optimistic"] as const)(
    "invalidates when %s history changes",
    (kind) => {
      seedRuntimeSession(baseSeed())
      const before = selectHistoricalTimelineTurns(
        useConversationRuntimeStore.getState(),
        CID
      )
      mutateHistoricalInput(kind)
      const after = selectHistoricalTimelineTurns(
        useConversationRuntimeStore.getState(),
        CID
      )
      expect(after).not.toBe(before)
    }
  )

  it("never includes a streaming phase", () => {
    seedRuntimeSession({ liveMessage: liveMessage("live-1", "answer") })
    expect(
      selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
    ).not.toContainEqual(expect.objectContaining({ phase: "streaming" }))
  })

  it("invalidates when live identity starts or ends", () => {
    seedRuntimeSession(baseSeed())
    const idle = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )

    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, liveMessage("live-1", "a"), true)
    const started = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    // Start may or may not change content for a plain seed, but key includes
    // liveMessageId so a recompute path is taken (new array).
    expect(started).not.toBe(idle)

    // Content-only append keeps the same historical array.
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, liveMessage("live-1", "ab"), true)
    const contentOnly = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(contentOnly).toBe(started)

    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, null, true)
    const ended = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(ended).not.toBe(started)
  })

  it("invalidates on delegation ownership / kickoff changes", () => {
    seedRuntimeSession({
      detail: detailWithTurns([assistantTurn("a1")]),
      liveMessage: liveMessage("live-1", "reply"),
    })
    const before = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    useConversationRuntimeStore
      .getState()
      .actions.setLiveOwnsActiveTurn(CID, true, "do the thing")
    const after = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(after).not.toBe(before)
    expect(after[0]?.key).toBe(`kickoff-${CID}`)
  })
})

describe("selectHistoricalTimelineTurns edge-case semantics", () => {
  it("suppresses persisted partial assistant turns while live is in hand", () => {
    seedRuntimeSession({
      detail: detailWithTurns(
        [
          userTurn("prompt-1"),
          assistantTurn("partial-a", "head"),
          userTurn("other"),
        ],
        { in_flight_user_turn_id: "prompt-1" }
      ),
      liveMessage: liveMessage("live-1", "full reply"),
    })
    const historical = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(historical.map((e) => e.turn.id)).toEqual(["prompt-1", "other"])
    expect(historical.every((e) => e.phase !== "streaming")).toBe(true)

    // Compatibility selector still surfaces the live stream.
    const full = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(full.some((e) => e.phase === "streaming")).toBe(true)
    expect(full.map((e) => e.turn.id)).toContain("live-42-live-1")
  })

  it("keeps first user id and last assistant id on collisions", () => {
    const sharedUser = userTurn("same-user", "first")
    const laterUser = userTurn("same-user", "second")
    const earlyAssistant = assistantTurn("same-asst", "early")
    const lateAssistant = assistantTurn("same-asst", "late")
    seedRuntimeSession({
      detail: detailWithTurns([sharedUser, earlyAssistant]),
      localTurns: [lateAssistant],
      optimisticTurns: [laterUser],
    })
    const historical = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    const users = historical.filter((e) => e.turn.id === "same-user")
    const assistants = historical.filter((e) => e.turn.id === "same-asst")
    expect(users).toHaveLength(1)
    expect(users[0].turn.blocks[0]).toMatchObject({ text: "first" })
    expect(assistants).toHaveLength(1)
    expect(assistants[0].turn.blocks[0]).toMatchObject({ text: "late" })
  })

  it("orders background overlay by timestamp with local turns", () => {
    seedRuntimeSession({
      detail: detailWithTurns([
        userTurn("u0", "u0", "2026-05-28T00:00:00.000Z"),
      ]),
      localTurns: [
        assistantTurn("local-1", "local", "2026-05-28T00:00:02.000Z"),
      ],
      backgroundTurns: [
        {
          turn: assistantTurn("bg-1", "bg", "2026-05-28T00:00:01.000Z"),
          watermark: 50,
        },
        {
          turn: assistantTurn("bg-2", "bg2", "2026-05-28T00:00:03.000Z"),
          watermark: 80,
        },
      ],
    })
    const ids = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    ).map((e) => e.turn.id)
    expect(ids).toEqual(["u0", "bg-1", "local-1", "bg-2"])
  })

  it("synthesizes delegation kickoff without streaming phase", () => {
    seedRuntimeSession({
      detail: detailWithTurns([assistantTurn("partial")]),
      liveMessage: liveMessage("live-1", "reply", 1_700_000_000_123),
      liveOwnsActiveTurn: true,
      delegationKickoffText: "do the thing",
    })
    const historical = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(historical.map((e) => e.phase)).not.toContain("streaming")
    expect(historical[0]).toMatchObject({
      key: `kickoff-${CID}`,
      phase: "persisted",
    })
    expect(historical[0].turn.blocks[0]).toMatchObject({
      text: "do the thing",
    })
    // Persisted assistant stripped while live/local reply owns the turn.
    expect(historical.some((e) => e.turn.id === "partial")).toBe(false)
    // Prefer detail.summary.created_at when present (unchanged semantics).
    expect(historical[0].turn.timestamp).toBe("2026-05-28T00:00:00.000Z")

    // Without detail, kickoff timestamp falls back to liveStartedAt.
    seedRuntimeSession({
      liveMessage: liveMessage("live-1", "reply", 1_700_000_000_123),
      liveOwnsActiveTurn: true,
      delegationKickoffText: "do the thing",
    })
    const noDetail = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(noDetail[0].turn.timestamp).toBe(
      new Date(1_700_000_000_123).toISOString()
    )
  })

  it("dedups optimistic user against same-id persisted user (keep first)", () => {
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("u-shared", "from-db")]),
      optimisticTurns: [userTurn("u-shared", "from-opt")],
    })
    const historical = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    const matches = historical.filter((e) => e.turn.id === "u-shared")
    expect(matches).toHaveLength(1)
    expect(matches[0].phase).toBe("persisted")
    expect(matches[0].turn.blocks[0]).toMatchObject({ text: "from-db" })
  })

  it("isolates historical caches across conversations", () => {
    seedRuntimeSession({
      ...baseSeed(),
      liveMessage: liveMessage("live-1", "x"),
    })
    const otherDetail = detailWithTurns([userTurn("other-u")])
    otherDetail.summary.id = OTHER_CID
    useConversationRuntimeStore.setState((state) => {
      const next = new Map(state.byConversationId)
      next.set(OTHER_CID, {
        conversationId: OTHER_CID,
        externalId: "sid-other",
        dbConversationId: null,
        detail: otherDetail,
        detailLoading: false,
        detailError: null,
        acpLoadError: null,
        localTurns: [],
        backgroundTurns: [],
        optimisticTurns: [],
        liveMessage: null,
        syncState: "idle",
        activeTurnToken: null,
        liveOwnsActiveTurn: false,
        delegationKickoffText: null,
        sessionStats: null,
        pendingCleanup: false,
      })
      return { byConversationId: next }
    })

    const a = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    const b = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      OTHER_CID
    )
    expect(a).not.toBe(b)
    expect(a.map((e) => e.turn.id)).toContain("u1")
    expect(b.map((e) => e.turn.id)).toEqual(["other-u"])

    // Content-only append on CID must not churn either conversation's cache.
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, liveMessage("live-1", "xy"), true)
    const aAfter = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    const bAfter = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      OTHER_CID
    )
    expect(aAfter).toBe(a)
    expect(bAfter).toBe(b)
  })

  it("drops cache on remove and reset so removed sessions do not retain history", () => {
    seedRuntimeSession(baseSeed())
    const before = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(before.length).toBeGreaterThan(0)

    useConversationRuntimeStore.getState().actions.removeConversation(CID)
    const afterRemove = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(afterRemove).toEqual([])

    seedRuntimeSession(baseSeed())
    const seeded = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(seeded.length).toBeGreaterThan(0)
    resetConversationRuntimeStore()
    expect(
      selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
    ).toEqual([])
  })

  it("does not carry historical cache across migrate ids", () => {
    seedRuntimeSession(baseSeed())
    const before = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    useConversationRuntimeStore
      .getState()
      .actions.migrateConversation(CID, OTHER_CID)

    expect(
      selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
    ).toEqual([])
    const migrated = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      OTHER_CID
    )
    // Recomputed under the new id (keys rewrite); not the old array reference.
    expect(migrated).not.toBe(before)
    expect(migrated.map((e) => e.turn.id)).toEqual(before.map((e) => e.turn.id))
    expect(migrated[0].key).toContain(String(OTHER_CID))
  })
})

describe("selectTimelineTurns compatibility", () => {
  it("appends canonical streaming turns without mutating historical cache", () => {
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("u1"), assistantTurn("a1")]),
      liveMessage: liveMessage("live-1", "stream"),
    })
    const historical = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    const full = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(full.length).toBe(historical.length + 1)
    expect(full[full.length - 1].phase).toBe("streaming")
    // Historical array identity and contents stay intact.
    expect(
      selectHistoricalTimelineTurns(useConversationRuntimeStore.getState(), CID)
    ).toBe(historical)
    expect(historical.every((e) => e.phase !== "streaming")).toBe(true)
  })

  it("keeps streaming copy over promoted local snapshot with same live id", () => {
    const live = liveMessage("lm-dup", "streaming reply")
    seedRuntimeSession({ liveMessage: live })
    useConversationRuntimeStore.getState().actions.completeTurn(CID, live)
    useConversationRuntimeStore
      .getState()
      .actions.setLiveMessage(CID, live, true)

    const full = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    const ids = full.map((e) => e.turn.id)
    expect(ids.filter((id) => id === `live-${CID}-lm-dup`)).toHaveLength(1)
    expect(full.find((e) => e.turn.id === `live-${CID}-lm-dup`)?.phase).toBe(
      "streaming"
    )

    // Historical has the promoted local copy only (no streaming phase).
    const historical = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(historical.every((e) => e.phase !== "streaming")).toBe(true)
    expect(historical.map((e) => e.turn.id)).toContain(`live-${CID}-lm-dup`)
  })
})

function codexSpawnToolBlock(): LiveContentBlock {
  const info: ToolCallInfo = {
    tool_call_id: "spawn-call-1",
    title: "spawn_agent",
    kind: "other",
    status: "completed",
    content: null,
    raw_input: JSON.stringify({
      agent_type: "worker",
      message: "investigate flaky test",
    }),
    raw_output_chunks: [JSON.stringify({ agent_id: "agent-native-1" })],
    raw_output_total_bytes: 0,
    locations: null,
    meta: null,
    images: [],
  }
  info.raw_output_total_bytes = info.raw_output_chunks.join("").length
  return { type: "tool_call", info }
}

function buildRuntimeFromBlocks(blocks: LiveContentBlock[]) {
  const live: LiveMessage = {
    id: "msg-native",
    role: "assistant",
    content: blocks,
    startedAt: Date.parse("2026-07-16T10:00:00Z"),
  }
  return buildStreamingTurnsFromLiveMessage(CID, live, { agentType: "codex" })
}

describe("buildStreamingTurnsFromLiveMessage — native activity projection", () => {
  it("keeps the original native tool call while adding one activity view", () => {
    const result = buildRuntimeFromBlocks([codexSpawnToolBlock()])
    // Runtime MessageTurn blocks use ContentBlock shape (`tool_use`).
    // Activity is derived alongside — the source tool block is never removed.
    const toolBlocks = result.turns.flatMap((turn) =>
      turn.blocks.filter((b) => b.type === "tool_use")
    )
    expect(toolBlocks).toHaveLength(1)
    expect(toolBlocks[0]).toMatchObject({
      type: "tool_use",
      tool_name: expect.stringMatching(/spawn_agent|agent|collab/i),
    })
    expect(result.delegationActivities).toHaveLength(1)
    expect(result.delegationActivities[0]).toMatchObject({
      origin: "native",
      authoritative: false,
      platform: "codex",
      operation: "spawn",
      task_id: "agent-native-1",
    })
  })

  it("projects ambiguous Agent only with correct agentType hint", () => {
    const agentBlock: LiveContentBlock = {
      type: "tool_call",
      info: {
        tool_call_id: "agent-call-1",
        title: "Agent",
        kind: "other",
        status: "in_progress",
        content: null,
        raw_input: JSON.stringify({
          subagent_type: "Explore",
          description: "scan",
        }),
        raw_output_chunks: [],
        raw_output_total_bytes: 0,
        locations: null,
        meta: null,
        images: [],
      },
    }
    const live: LiveMessage = {
      id: "msg-agent",
      role: "assistant",
      content: [agentBlock],
      startedAt: Date.parse("2026-07-16T10:00:00Z"),
    }
    const withClaude = buildStreamingTurnsFromLiveMessage(CID, live, {
      agentType: "claude_code",
    })
    expect(withClaude.delegationActivities).toHaveLength(1)
    expect(withClaude.delegationActivities[0]).toMatchObject({
      platform: "claude_code",
      operation: "spawn",
    })

    const withBuddy = buildStreamingTurnsFromLiveMessage(CID, live, {
      agentType: "code_buddy",
    })
    expect(withBuddy.delegationActivities).toHaveLength(1)
    expect(withBuddy.delegationActivities[0]).toMatchObject({
      platform: "code_buddy",
      operation: "spawn",
    })

    const withoutHint = buildStreamingTurnsFromLiveMessage(CID, live)
    expect(withoutHint.delegationActivities).toHaveLength(0)
  })
})

describe("runtime store — production agentType + delegationActivities", () => {
  afterEach(() => {
    resetConversationRuntimeStore()
  })

  it("COMPLETE_TURN persists delegationActivities with session agentType", () => {
    const { actions } = useConversationRuntimeStore.getState()
    actions.fetchDetail(CID)
    // Seed detail so agentType resolves to claude_code.
    useConversationRuntimeStore.setState((s) => {
      const session = s.byConversationId.get(CID)!
      const next = new Map(s.byConversationId)
      next.set(CID, {
        ...session,
        detail: detailWithTurns([], {
          summary: {
            ...detailWithTurns([]).summary,
            agent_type: "claude_code",
          },
        }),
        detailLoading: false,
      })
      return { byConversationId: next }
    })

    const live: LiveMessage = {
      id: "lm-agent",
      role: "assistant",
      content: [
        {
          type: "tool_call",
          info: {
            tool_call_id: "a1",
            title: "Agent",
            kind: "other",
            status: "completed",
            content: null,
            raw_input: JSON.stringify({
              subagent_type: "Explore",
              description: "x",
            }),
            raw_output_chunks: [JSON.stringify({ task_id: "task-from-agent" })],
            raw_output_total_bytes: 0,
            locations: null,
            meta: null,
            images: [],
          },
        },
      ],
      startedAt: Date.parse("2026-07-16T10:00:00Z"),
    }
    // Fix raw_output_total_bytes
    const block = live.content[0]
    if (block.type === "tool_call") {
      block.info.raw_output_total_bytes =
        block.info.raw_output_chunks.join("").length
    }

    actions.setLiveMessage(CID, live, true)
    const mid = selectDelegationActivities(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(mid).toHaveLength(1)
    expect(mid[0]).toMatchObject({
      platform: "claude_code",
      operation: "spawn",
      authoritative: false,
    })

    actions.completeTurn(CID, live)
    const after = selectDelegationActivities(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(after).toHaveLength(1)
    expect(after[0]?.platform).toBe("claude_code")
    // Live cleared but activities remain for overlay consumers.
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.liveMessage
    ).toBeNull()
  })

  it("does not project Agent without session agentType", () => {
    const { actions } = useConversationRuntimeStore.getState()
    // No detail → no agentType → Agent is ambiguous.
    const live: LiveMessage = {
      id: "lm-nohint",
      role: "assistant",
      content: [
        {
          type: "tool_call",
          info: {
            tool_call_id: "a1",
            title: "Agent",
            kind: "other",
            status: "in_progress",
            content: null,
            raw_input: JSON.stringify({ description: "x" }),
            raw_output_chunks: [],
            raw_output_total_bytes: 0,
            locations: null,
            meta: null,
            images: [],
          },
        },
      ],
      startedAt: Date.parse("2026-07-16T10:00:00Z"),
    }
    actions.setLiveMessage(CID, live, true)
    expect(
      selectDelegationActivities(useConversationRuntimeStore.getState(), CID)
    ).toHaveLength(0)
  })
})
