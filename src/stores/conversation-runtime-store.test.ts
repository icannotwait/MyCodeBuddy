import { afterEach, describe, expect, it, vi } from "vitest"
import { resetJsonParseCacheForTests } from "@/lib/try-parse-json"
import type {
  LiveContentBlock,
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import type {
  ContentBlock,
  DbConversationDetail,
  MessageTurn,
  SessionStats,
} from "@/lib/types"
import type { BackgroundOverlayEntry } from "@/stores/conversation-runtime-store"
import {
  buildStreamingTurnsFromLiveMessage,
  completeLiveTranscriptTurn,
  getConversationIdByExternalIdFromStore,
  resetConversationRuntimeStore,
  selectDelegationActivities,
  selectHistoricalTimelineTurns,
  selectTimelineTurns,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import {
  __resetLiveTranscriptStoreForTests,
  liveTranscriptStore,
} from "@/stores/live-transcript-store"
import { getFolderConversation, saveTurnGenerationStat } from "@/lib/api"
import { publishRequestUsage } from "@/lib/request-usage-live"
import { EMPTY_REQUEST_USAGE } from "@/lib/request-usage-speed"

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>()
  return {
    ...actual,
    getFolderConversation: vi.fn(actual.getFolderConversation),
    saveTurnGenerationStat: vi.fn(async () => undefined),
  }
})

const mockGetFolderConversation = vi.mocked(getFolderConversation)
const mockSaveTurnGenerationStat = vi.mocked(saveTurnGenerationStat)

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

function assistantTurnWithBlocks(
  id: string,
  blocks: ContentBlock[],
  timestamp = "2026-05-28T00:00:01.000Z"
): MessageTurn {
  return { id, role: "assistant", blocks, timestamp }
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
      auto_title_finalized: false,
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
  externalId?: string | null
  detail?: DbConversationDetail | null
  localTurns?: MessageTurn[]
  backgroundTurns?: BackgroundOverlayEntry[]
  optimisticTurns?: MessageTurn[]
  queuedOptimisticTurnIds?: string[]
  liveMessage?: LiveMessage | null
  liveOwnsActiveTurn?: boolean
  delegationKickoffText?: string | null
  sessionStats?: SessionStats | null
  syncState?: "idle" | "awaiting_persist"
  activeTurnToken?: string | null
  lastTurnOwned?: boolean
  historyAssistantBaseline?: number | null
  batchBoundaryIndex?: number | null
  batchBoundaryPrefixHash?: string | null
  detailHistoryLoadingOlder?: boolean
  loadingOlderTurns?: boolean
}

function seedRuntimeSession(input: SeedInput = {}) {
  useConversationRuntimeStore.setState({
    byConversationId: new Map([
      [
        CID,
        {
          conversationId: CID,
          externalId:
            input.externalId === undefined ? "sid-1" : input.externalId,
          dbConversationId: null,
          detail: input.detail ?? null,
          detailLoading: false,
          detailError: null,
          detailHistoryLoadingOlder: input.detailHistoryLoadingOlder ?? false,
          acpLoadError: null,
          localTurns: input.localTurns ?? [],
          backgroundTurns: input.backgroundTurns ?? [],
          pendingBackgroundSettlements: [],
          optimisticTurns: input.optimisticTurns ?? [],
          queuedOptimisticTurnIds: input.queuedOptimisticTurnIds ?? [],
          liveMessage: input.liveMessage ?? null,
          syncState: input.syncState ?? "idle",
          activeTurnToken: input.activeTurnToken ?? null,
          lastTurnOwned: input.lastTurnOwned ?? false,
          liveOwnsActiveTurn: input.liveOwnsActiveTurn ?? false,
          delegationKickoffText: input.delegationKickoffText ?? null,
          sessionStats: input.sessionStats ?? null,
          delegationActivities: [],
          historyAssistantBaseline: input.historyAssistantBaseline ?? null,
          batchBoundaryIndex: input.batchBoundaryIndex ?? null,
          batchBoundaryPrefixHash: input.batchBoundaryPrefixHash ?? null,
          loadingOlderTurns: input.loadingOlderTurns ?? false,
          olderTurnsPrependEpoch: 0,
          pendingCleanup: false,
          delegateSyncError: null,
          pendingCancel: null,
          softFence: false,
          ownerPreserve: false,
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
  __resetLiveTranscriptStoreForTests()
  resetJsonParseCacheForTests()
  vi.restoreAllMocks()
})

describe("completeLiveTranscriptTurn", () => {
  it("keeps the live transcript when the canonical turn was not promoted", () => {
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("u1"), assistantTurn("a1")]),
      syncState: "idle",
    })
    const final = liveMessage("latest", "latest Codex reply")
    liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)
    vi.spyOn(console, "warn").mockImplementation(() => {})

    completeLiveTranscriptTurn(CID)

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns
    ).toEqual([])
    expect(liveTranscriptStore.getConversation(CID)).toMatchObject({
      messageId: "latest",
      status: "completing",
    })
  })

  it("keeps the live projection when an owner final does not land", () => {
    const final = liveMessage("latest", "complete latest Codex reply")
    const staleFinal: LiveMessage = { ...final, content: [] }
    seedRuntimeSession({
      optimisticTurns: [userTurn("wire-latest-user", "continue diagnosing")],
      liveMessage: final,
      syncState: "awaiting_persist",
    })
    liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)

    completeLiveTranscriptTurn(CID, staleFinal)

    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.localTurns.some((turn) => turn.role === "assistant")
    ).toBe(false)
    expect(liveTranscriptStore.getConversation(CID)).toMatchObject({
      messageId: "latest",
      status: "completing",
    })
  })

  it("keeps one complete owner final over a cached parser partial", () => {
    const final = liveMessage(
      "latest",
      "complete latest Codex reply",
      Date.parse("2026-05-28T00:00:03.000Z")
    )
    seedRuntimeSession({
      detail: detailWithTurns(
        [
          userTurn("turn-100", "earlier prompt"),
          assistantTurn(
            "turn-101",
            "earlier reply",
            "2026-05-28T00:00:01.000Z"
          ),
          userTurn(
            "turn-102",
            "continue diagnosing",
            "2026-05-28T00:00:02.000Z"
          ),
          assistantTurn(
            "turn-103",
            "partial latest Codex reply",
            "2026-05-28T00:00:03.000Z"
          ),
        ],
        { turns_total: 4 }
      ),
      optimisticTurns: [
        userTurn(
          "wire-latest-user",
          "continue diagnosing",
          "2026-05-28T00:00:02.000Z"
        ),
      ],
      liveMessage: final,
      syncState: "awaiting_persist",
      batchBoundaryIndex: 2,
    })
    liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)

    completeLiveTranscriptTurn(CID, final)

    const state = useConversationRuntimeStore.getState()
    const runtime = state.byConversationId.get(CID)!
    expect(runtime.localTurns.map((turn) => turn.id)).toEqual([
      "wire-latest-user",
      `live-${CID}-latest`,
    ])
    const latestTurns = selectTimelineTurns(state, CID).filter(
      (entry) =>
        entry.turn.timestamp === "2026-05-28T00:00:02.000Z" ||
        entry.turn.timestamp === "2026-05-28T00:00:03.000Z"
    )
    expect(latestTurns.map((entry) => entry.turn.id)).toEqual([
      "wire-latest-user",
      `live-${CID}-latest`,
    ])
    expect(latestTurns[1]?.turn.blocks).toEqual([
      { type: "text", text: "complete latest Codex reply" },
    ])
    expect(liveTranscriptStore.getConversation(CID)).toBeNull()
  })

  it.each(["newer prompt", "owned prompt"])(
    "replaces only the owned round before a later persisted %s",
    (newerPrompt) => {
      const final = liveMessage(
        "latest",
        "complete owned reply",
        Date.parse("2026-05-28T00:00:03.000Z")
      )
      seedRuntimeSession({
        detail: detailWithTurns(
          [
            userTurn("turn-100", "earlier prompt"),
            assistantTurn(
              "turn-101",
              "earlier reply",
              "2026-05-28T00:00:01.000Z"
            ),
            userTurn("turn-102", "owned prompt", "2026-05-28T00:00:02.000Z"),
            assistantTurn(
              "turn-103",
              "partial owned reply",
              "2026-05-28T00:00:03.000Z"
            ),
            {
              id: "turn-system",
              role: "system",
              blocks: [{ type: "text", text: "system notice" }],
              timestamp: "2026-05-28T00:00:03.250Z",
            },
            {
              ...assistantTurn(
                "turn-autonomous",
                "background completion",
                "2026-05-28T00:00:03.500Z"
              ),
              autonomous_origin: "background_task",
            },
            userTurn("turn-104", newerPrompt, "2026-05-28T00:00:04.000Z"),
            assistantTurn(
              "turn-105",
              "newer reply",
              "2026-05-28T00:00:05.000Z"
            ),
          ],
          { turns_total: 8 }
        ),
        optimisticTurns: [
          userTurn(
            "wire-latest-user",
            "owned prompt",
            "2026-05-28T00:00:02.500Z"
          ),
        ],
        liveMessage: final,
        syncState: "awaiting_persist",
        batchBoundaryIndex: 2,
      })
      liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)

      completeLiveTranscriptTurn(CID, final)

      const timelineIds = selectTimelineTurns(
        useConversationRuntimeStore.getState(),
        CID
      ).map((entry) => entry.turn.id)
      expect(timelineIds).toEqual([
        "turn-100",
        "turn-101",
        "wire-latest-user",
        `live-${CID}-latest`,
        "turn-system",
        "turn-autonomous",
        "turn-104",
        "turn-105",
      ])
    }
  )

  it("retires an older covered overlay while preserving the current owner round", () => {
    const final = liveMessage(
      "latest",
      "complete latest Codex reply",
      Date.parse("2026-05-28T00:00:03.000Z")
    )
    seedRuntimeSession({
      detail: detailWithTurns(
        [
          userTurn("turn-100", "earlier prompt"),
          assistantTurn(
            "turn-101",
            "earlier reply",
            "2026-05-28T00:00:01.000Z"
          ),
          userTurn(
            "turn-102",
            "continue diagnosing",
            "2026-05-28T00:00:02.000Z"
          ),
          assistantTurn(
            "turn-103",
            "partial latest Codex reply",
            "2026-05-28T00:00:03.000Z"
          ),
        ],
        { turns_total: 4 }
      ),
      localTurns: [
        userTurn("wire-old-user", "earlier prompt"),
        assistantTurn(
          "live-old-assistant",
          "earlier reply",
          "2026-05-28T00:00:01.000Z"
        ),
      ],
      optimisticTurns: [
        userTurn(
          "wire-latest-user",
          "continue diagnosing",
          "2026-05-28T00:00:02.000Z"
        ),
      ],
      liveMessage: final,
      syncState: "awaiting_persist",
      batchBoundaryIndex: 0,
    })
    liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)

    completeLiveTranscriptTurn(CID, final)

    const state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-latest-user", `live-${CID}-latest`])
    expect(
      selectTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual([
      "turn-100",
      "turn-101",
      "wire-latest-user",
      `live-${CID}-latest`,
    ])
  })

  it("does not match an older persisted user by empty timestamps", () => {
    const final = liveMessage(
      "latest",
      "complete latest Codex reply",
      Date.parse("2026-05-28T00:00:03.000Z")
    )
    seedRuntimeSession({
      detail: detailWithTurns([
        userTurn("turn-100", "earlier prompt", ""),
        assistantTurn("turn-101", "earlier reply", "2026-05-28T00:00:01.000Z"),
      ]),
      optimisticTurns: [userTurn("wire-latest-user", "new prompt", "")],
      liveMessage: final,
      syncState: "awaiting_persist",
      batchBoundaryIndex: null,
    })
    liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)

    completeLiveTranscriptTurn(CID, final)

    expect(
      selectTimelineTurns(useConversationRuntimeStore.getState(), CID).map(
        (entry) => entry.turn.id
      )
    ).toEqual([
      "turn-100",
      "turn-101",
      "wire-latest-user",
      `live-${CID}-latest`,
    ])
  })

  it("silently ignores a recovery completion after the turn was promoted", () => {
    const final = liveMessage("latest", "latest Codex reply")
    seedRuntimeSession({
      optimisticTurns: [userTurn("u1")],
      liveMessage: final,
      syncState: "awaiting_persist",
    })
    liveTranscriptStore.rebuild(CID, "owner-conn", final, 3)

    completeLiveTranscriptTurn(CID, final)
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
    completeLiveTranscriptTurn(CID, final)

    expect(warn).not.toHaveBeenCalled()
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.localTurns.filter((turn) => turn.role === "assistant")
    ).toHaveLength(1)
  })
})

describe("external session index", () => {
  it("re-elects an alias when late DB binding makes it durable", () => {
    const { actions } = useConversationRuntimeStore.getState()
    actions.setExternalId(-1, "shared-session")
    actions.setExternalId(-2, "shared-session")
    expect(getConversationIdByExternalIdFromStore("shared-session")).toBe(-1)

    actions.setDbConversationId(-2, 42)

    expect(getConversationIdByExternalIdFromStore("shared-session")).toBe(-2)
  })

  it("clears background overlays when the external session is rebound", () => {
    seedRuntimeSession({
      backgroundTurns: [
        {
          turn: assistantTurn("old-session-overlay"),
          watermark: 10,
        },
      ],
    })

    useConversationRuntimeStore.getState().actions.setExternalId(CID, "sid-2")

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.backgroundTurns
    ).toEqual([])
  })

  it("keeps background overlays on initial and repeated identity writes", () => {
    const backgroundTurns = [
      {
        turn: assistantTurn("current-session-overlay"),
        watermark: 10,
      },
    ]
    seedRuntimeSession({ externalId: null, backgroundTurns })
    const actions = useConversationRuntimeStore.getState().actions

    actions.setExternalId(CID, "sid-1")
    actions.setExternalId(CID, "sid-1")

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.backgroundTurns
    ).toBe(backgroundTurns)
  })
})

describe("live request usage persistence boundary", () => {
  function completeWith(snapshot: {
    outputTokens: number
    generationMs: number
    tps: number
    sampleCount: number
    estimatedSampleCount: number
  }) {
    mockSaveTurnGenerationStat.mockClear()
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("u1")]),
      optimisticTurns: [userTurn("u2")],
    })
    publishRequestUsage(CID, snapshot)
    useConversationRuntimeStore
      .getState()
      .actions.completeTurn(CID, liveMessage("live", "done"))
  }

  it.each([
    {
      name: "empty",
      snapshot: EMPTY_REQUEST_USAGE,
    },
    {
      name: "non-positive tokens",
      snapshot: {
        outputTokens: 0,
        generationMs: 1_000,
        tps: 0,
        sampleCount: 1,
        estimatedSampleCount: 0,
      },
    },
    {
      name: "non-positive duration",
      snapshot: {
        outputTokens: 10,
        generationMs: 0,
        tps: 0,
        sampleCount: 1,
        estimatedSampleCount: 0,
      },
    },
    {
      name: "estimated only",
      snapshot: {
        outputTokens: 10,
        generationMs: 1_000,
        tps: 10,
        sampleCount: 1,
        estimatedSampleCount: 1,
      },
    },
    {
      name: "mixed exact and estimated",
      snapshot: {
        outputTokens: 30,
        generationMs: 2_000,
        tps: 15,
        sampleCount: 2,
        estimatedSampleCount: 1,
      },
    },
  ])("does not persist $name snapshots", ({ snapshot }) => {
    completeWith(snapshot)
    expect(mockSaveTurnGenerationStat).not.toHaveBeenCalled()
  })

  it.each([
    {
      name: "estimated-only",
      snapshot: {
        outputTokens: 10,
        generationMs: 1_000,
        tps: 10,
        sampleCount: 1,
        estimatedSampleCount: 1,
      },
    },
    {
      name: "mixed",
      snapshot: {
        outputTokens: 30,
        generationMs: 2_000,
        tps: 15,
        sampleCount: 2,
        estimatedSampleCount: 1,
      },
    },
  ])("keeps $name usage out of promoted turns", ({ snapshot }) => {
    completeWith(snapshot)

    const promotedAssistant = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)!
      .localTurns.find((turn) => turn.role === "assistant")
    expect(promotedAssistant).toBeDefined()
    expect(promotedAssistant).not.toHaveProperty("generation_ms")
    expect(promotedAssistant).not.toHaveProperty("generation_tokens")
  })

  it("persists a positive all-exact snapshot unchanged", () => {
    completeWith({
      outputTokens: 77,
      generationMs: 1_500,
      tps: 77 / 1.5,
      sampleCount: 1,
      estimatedSampleCount: 0,
    })

    expect(mockSaveTurnGenerationStat).toHaveBeenCalledWith({
      conversationId: CID,
      userOrdinal: 1,
      generationMs: 1_500,
      generationTokens: 77,
    })
  })

  it("allows exact persistence after an empty hydrate publication", () => {
    publishRequestUsage(CID, {
      outputTokens: 30,
      generationMs: 2_000,
      tps: 15,
      sampleCount: 2,
      estimatedSampleCount: 1,
    })
    publishRequestUsage(CID, EMPTY_REQUEST_USAGE)
    completeWith({
      outputTokens: 25,
      generationMs: 500,
      tps: 50,
      sampleCount: 1,
      estimatedSampleCount: 0,
    })

    expect(mockSaveTurnGenerationStat).toHaveBeenCalledTimes(1)
    expect(mockSaveTurnGenerationStat).toHaveBeenCalledWith(
      expect.objectContaining({ generationTokens: 25, generationMs: 500 })
    )
  })
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
        detailHistoryLoadingOlder: false,
        acpLoadError: null,
        localTurns: [],
        backgroundTurns: [],
        pendingBackgroundSettlements: [],
        optimisticTurns: [],
        liveMessage: null,
        syncState: "idle",
        activeTurnToken: null,
        lastTurnOwned: false,
        liveOwnsActiveTurn: false,
        delegationKickoffText: null,
        sessionStats: null,
        delegationActivities: [],
        historyAssistantBaseline: null,
        batchBoundaryIndex: null,
        batchBoundaryPrefixHash: null,
        loadingOlderTurns: false,
        olderTurnsPrependEpoch: 0,
        pendingCleanup: false,
        delegateSyncError: null,
        pendingCancel: null,
        softFence: false,
        ownerPreserve: false,
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

  it("clears an invalidated load-older spinner when the runtime id migrates", () => {
    seedRuntimeSession(baseSeed())
    useConversationRuntimeStore.setState((state) => {
      const current = state.byConversationId.get(CID)!
      const next = new Map(state.byConversationId)
      next.set(CID, { ...current, detailHistoryLoadingOlder: true })
      return { byConversationId: next }
    })

    useConversationRuntimeStore
      .getState()
      .actions.migrateConversation(CID, OTHER_CID)

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(OTHER_CID)
        ?.detailHistoryLoadingOlder
    ).toBe(false)
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

  it("parses a large Write raw_input only once across rebuilds of the same tool", () => {
    const info: ToolCallInfo = {
      tool_call_id: "write-1",
      title: "tool",
      kind: "tool",
      status: "completed",
      content: null,
      raw_input: JSON.stringify({
        content: "x".repeat(128 * 1024),
        file_path: "a.ts",
      }),
      raw_output_chunks: [],
      raw_output_total_bytes: 0,
      locations: null,
      meta: null,
      images: [],
    }
    const live: LiveMessage = {
      id: "msg-write",
      role: "assistant",
      content: [{ type: "tool_call", info }],
      startedAt: Date.parse("2026-07-16T10:00:00Z"),
    }
    const spy = vi.spyOn(JSON, "parse")
    buildStreamingTurnsFromLiveMessage(CID, live)
    buildStreamingTurnsFromLiveMessage(CID, live)
    expect(spy).toHaveBeenCalledTimes(1)
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

  it("COMPLETE_TURN drops same-id persisted turns and keeps unpersisted history", () => {
    const { actions } = useConversationRuntimeStore.getState()
    actions.fetchDetail(CID)
    useConversationRuntimeStore.setState((s) => {
      const session = s.byConversationId.get(CID)!
      const next = new Map(s.byConversationId)
      next.set(CID, {
        ...session,
        detail: detailWithTurns([
          assistantTurn("live-42-keep-me", "already persisted"),
        ]),
        detailLoading: false,
      })
      return { byConversationId: next }
    })

    const covered: LiveMessage = {
      id: "keep-me",
      role: "assistant",
      content: [{ type: "text", text: "already persisted" }],
      startedAt: Date.parse("2026-07-16T10:00:00Z"),
    }
    actions.setLiveMessage(CID, covered, true)
    actions.completeTurn(CID, covered)
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.localTurns.some((t) => t.id === "live-42-keep-me")
    ).toBe(false)

    for (let i = 0; i < 81; i++) {
      const live: LiveMessage = {
        id: `cap-${i}`,
        role: "assistant",
        content: [{ type: "text", text: `t${i}` }],
        startedAt: Date.parse("2026-07-16T10:00:00Z") + i,
      }
      actions.setLiveMessage(CID, live, true)
      actions.completeTurn(CID, live)
    }
    const local =
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns ?? []
    expect(local.length).toBe(81)
    expect(local.some((t) => t.id === "live-42-cap-0")).toBe(true)
    expect(local.some((t) => t.id === "live-42-cap-80")).toBe(true)
  })

  it("idle FETCH_DETAIL_SUCCESS retires only localTurns already in detail", async () => {
    const { actions } = useConversationRuntimeStore.getState()
    actions.fetchDetail(CID)
    useConversationRuntimeStore.setState((s) => {
      const session = s.byConversationId.get(CID)!
      const next = new Map(s.byConversationId)
      next.set(CID, {
        ...session,
        detail: detailWithTurns([
          assistantTurn("live-42-keep-me", "already persisted"),
        ]),
        detailLoading: false,
        syncState: "idle",
      })
      return { byConversationId: next }
    })

    const covered: LiveMessage = {
      id: "keep-me",
      role: "assistant",
      content: [{ type: "text", text: "already persisted" }],
      startedAt: Date.parse("2026-07-16T10:00:00Z"),
    }
    actions.setLiveMessage(CID, covered, true)
    actions.completeTurn(CID, covered)

    for (let i = 0; i < 81; i++) {
      const live: LiveMessage = {
        id: `cap-${i}`,
        role: "assistant",
        content: [{ type: "text", text: `t${i}` }],
        startedAt: Date.parse("2026-07-16T10:00:00Z") + i,
      }
      actions.setLiveMessage(CID, live, true)
      actions.completeTurn(CID, live)
    }
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns.length
    ).toBe(81)

    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        assistantTurn("live-42-keep-me", "already persisted"),
        assistantTurn("live-42-cap-0", "t0"),
      ])
    )
    actions.refetchDetail(CID, { preserveLive: false })
    await Promise.resolve()
    await Promise.resolve()

    const local =
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns ?? []
    expect(local.some((t) => t.id === "live-42-keep-me")).toBe(false)
    expect(local.some((t) => t.id === "live-42-cap-0")).toBe(false)
    expect(local.length).toBe(80)
    expect(local.some((t) => t.id === "live-42-cap-1")).toBe(true)
    expect(local.some((t) => t.id === "live-42-cap-80")).toBe(true)
  })
})

describe("owner overlay retirement without live-* persist ids", () => {
  it("keeps an owned final when a late refetch contains only its parser partial", async () => {
    const userTimestamp = "2026-09-01T08:50:14.923Z"
    const assistantTimestamp = "2026-09-01T08:50:42.568Z"
    seedRuntimeSession({
      detail: detailWithTurns([
        userTurn("u-old", "old prompt", "2026-09-01T08:40:00.000Z"),
        assistantTurn("a-old", "old reply", "2026-09-01T08:40:01.000Z"),
      ]),
      localTurns: [
        userTurn("wire-latest-user", "continue", userTimestamp),
        assistantTurn(
          "live-42-latest",
          "complete latest reply",
          assistantTimestamp
        ),
      ],
      syncState: "idle",
      lastTurnOwned: true,
      batchBoundaryIndex: 2,
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns(
        [
          userTurn("u-old", "old prompt", "2026-09-01T08:40:00.000Z"),
          assistantTurn("a-old", "old reply", "2026-09-01T08:40:01.000Z"),
          userTurn("parser-user", "continue", userTimestamp),
          assistantTurn(
            "parser-assistant",
            "partial latest reply",
            assistantTimestamp
          ),
        ],
        { turns_total: 4 }
      )
    )

    useConversationRuntimeStore
      .getState()
      .actions.refetchDetail(CID, { preserveLive: true })
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-latest-user", "live-42-latest"])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["u-old", "a-old", "wire-latest-user", "live-42-latest"])
    expect(selectHistoricalTimelineTurns(state, CID)[3]?.turn.blocks).toEqual([
      { type: "text", text: "complete latest reply" },
    ])
  })

  it("keeps consecutive owned finals ordered across completion and another stale refetch", async () => {
    const previousUserTimestamp = "2026-09-01T08:50:14.923Z"
    const previousAssistantTimestamp = "2026-09-01T08:50:42.568Z"
    seedRuntimeSession({
      detail: detailWithTurns(
        [
          userTurn("parser-user", "continue", previousUserTimestamp),
          assistantTurn(
            "parser-assistant",
            "partial previous reply",
            previousAssistantTimestamp
          ),
        ],
        { turns_total: 2 }
      ),
      localTurns: [
        userTurn("wire-previous-user", "continue", previousUserTimestamp),
        assistantTurn(
          "live-42-previous",
          "complete previous reply",
          previousAssistantTimestamp
        ),
      ],
      optimisticTurns: [
        userTurn("wire-current-user", "next prompt", "2026-09-01T08:51:00Z"),
      ],
      liveMessage: liveMessage(
        "current",
        "complete current reply",
        Date.parse("2026-09-01T08:51:10Z")
      ),
      syncState: "awaiting_persist",
      lastTurnOwned: true,
      batchBoundaryIndex: 0,
    })

    useConversationRuntimeStore.getState().actions.completeTurn(CID)

    const expectedIds = [
      "wire-previous-user",
      "live-42-previous",
      "wire-current-user",
      "live-42-current",
    ]
    let state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(expectedIds)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(expectedIds)

    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns(
        [
          userTurn("parser-user", "continue", previousUserTimestamp),
          assistantTurn(
            "parser-assistant",
            "partial previous reply",
            previousAssistantTimestamp
          ),
        ],
        { turns_total: 2 }
      )
    )
    useConversationRuntimeStore
      .getState()
      .actions.refetchDetail(CID, { preserveLive: true })
    await Promise.resolve()
    await Promise.resolve()

    state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(expectedIds)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(expectedIds)
  })

  it("does not reuse a retired owner boundary for the newly completed round", async () => {
    const previous = [
      userTurn("wire-user-1", "continue", "2026-09-01T08:52:00Z"),
      assistantTurn("wire-a-1", "done", "2026-09-01T08:52:01Z"),
    ]
    const persistedPrevious = detailWithTurns(previous)
    seedRuntimeSession({
      detail: persistedPrevious,
      localTurns: previous,
      optimisticTurns: [
        userTurn("wire-user-2", "continue", "2026-09-01T08:53:00Z"),
      ],
      liveMessage: liveMessage(
        "current",
        "done",
        Date.parse("2026-09-01T08:53:01Z")
      ),
      syncState: "awaiting_persist",
      lastTurnOwned: true,
      batchBoundaryIndex: 0,
    })

    const actions = useConversationRuntimeStore.getState().actions
    actions.completeTurn(CID)
    mockGetFolderConversation.mockResolvedValueOnce(persistedPrevious)
    actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-user-2", "live-42-current"])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["wire-user-1", "wire-a-1", "wire-user-2", "live-42-current"])
  })

  it("clears a retired boundary while the next owner round is in flight", async () => {
    const previous = [
      userTurn("wire-user-1", "continue", "2026-09-01T08:54:00Z"),
      assistantTurn("wire-a-1", "done", "2026-09-01T08:54:01Z"),
    ]
    const persistedPrevious = detailWithTurns(previous)
    seedRuntimeSession({
      detail: detailWithTurns([]),
      localTurns: previous,
      optimisticTurns: [
        userTurn("wire-user-2", "continue", "2026-09-01T08:55:00Z"),
      ],
      liveMessage: liveMessage(
        "current",
        "done",
        Date.parse("2026-09-01T08:55:01Z")
      ),
      syncState: "awaiting_persist",
      lastTurnOwned: true,
      batchBoundaryIndex: 0,
    })
    mockGetFolderConversation
      .mockResolvedValueOnce(persistedPrevious)
      .mockResolvedValueOnce(persistedPrevious)

    const actions = useConversationRuntimeStore.getState().actions
    actions.refetchDetail(CID, { preserveLive: true })
    await Promise.resolve()
    await Promise.resolve()
    actions.completeTurn(CID)
    actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-user-2", "live-42-current"])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["wire-user-1", "wire-a-1", "wire-user-2", "live-42-current"])
  })

  it("keeps a previous owner final when a viewer completes the next turn", () => {
    const previousUserTimestamp = "2026-09-01T08:50:14.923Z"
    const previousAssistantTimestamp = "2026-09-01T08:50:42.568Z"
    seedRuntimeSession({
      detail: detailWithTurns([
        userTurn("parser-user", "continue", previousUserTimestamp),
        assistantTurn(
          "parser-assistant",
          "partial previous reply",
          previousAssistantTimestamp
        ),
      ]),
      localTurns: [
        userTurn("wire-previous-user", "continue", previousUserTimestamp),
        assistantTurn(
          "live-42-previous",
          "complete previous reply",
          previousAssistantTimestamp
        ),
      ],
      optimisticTurns: [
        userTurn("wire-viewer-user", "viewer prompt", "2026-09-01T08:51:00Z"),
      ],
      liveMessage: liveMessage(
        "viewer",
        "complete viewer reply",
        Date.parse("2026-09-01T08:51:10Z")
      ),
      syncState: "idle",
      lastTurnOwned: true,
      batchBoundaryIndex: 0,
    })

    useConversationRuntimeStore.getState().actions.completeTurn(CID)

    const state = useConversationRuntimeStore.getState()
    const expectedIds = [
      "wire-previous-user",
      "live-42-previous",
      "wire-viewer-user",
      "live-42-viewer",
    ]
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(expectedIds)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(expectedIds)
  })

  it("keeps the complete body when an exact-id persisted turn only has its final text", async () => {
    const localTurns = [
      userTurn("shared-user", "inspect everything", "2026-09-01T09:00:00Z"),
      assistantTurnWithBlocks(
        "shared-assistant",
        [
          { type: "thinking", text: "full reasoning" },
          {
            type: "tool_use",
            tool_use_id: "tool-1",
            tool_name: "Read",
            input_preview: '{"path":"a.ts"}',
          },
          {
            type: "tool_result",
            tool_use_id: "tool-1",
            output_preview: "file contents",
            is_error: false,
            images: [
              { data: "image-bytes", mime_type: "image/png", uri: null },
            ],
          },
          { type: "text", text: "same final text" },
        ],
        "2026-09-01T09:00:10Z"
      ),
    ]
    seedRuntimeSession({
      detail: detailWithTurns([]),
      localTurns,
      lastTurnOwned: true,
      batchBoundaryIndex: 0,
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("shared-user", "inspect everything", "2026-09-01T09:00:00Z"),
        assistantTurn(
          "shared-assistant",
          "same final text",
          "2026-09-01T09:00:10Z"
        ),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["shared-user", "shared-assistant"])
    expect(selectHistoricalTimelineTurns(state, CID)[1]?.turn.blocks).toEqual(
      localTurns[1]?.blocks
    )
  })

  it.each([
    [
      "tool-only",
      [
        {
          type: "tool_use" as const,
          tool_use_id: "tool-1",
          tool_name: "Read",
          input_preview: '{"path":"a.ts"}',
        },
        {
          type: "tool_result" as const,
          tool_use_id: "tool-1",
          output_preview: "done",
          is_error: false,
        },
      ],
    ],
    [
      "image-only",
      [
        {
          type: "image" as const,
          data: "image-bytes",
          mime_type: "image/png",
          uri: null,
        },
      ],
    ],
  ])("retires a fully persisted %s owner round", async (_, blocks) => {
    const localTurns = [
      userTurn("wire-user", "show result", "2026-09-01T09:10:00Z"),
      assistantTurnWithBlocks("live-assistant", blocks, "2026-09-01T09:10:10Z"),
    ]
    seedRuntimeSession({
      detail: detailWithTurns([]),
      localTurns,
      lastTurnOwned: true,
      batchBoundaryIndex: 0,
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("parser-user", "show result", "2026-09-01T10:10:00Z"),
        assistantTurnWithBlocks(
          "parser-assistant",
          blocks,
          "2026-09-01T10:10:10Z"
        ),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual([])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["parser-user", "parser-assistant"])
  })

  it("retires a fully persisted assistant-only overlay", async () => {
    const blocks: ContentBlock[] = [
      { type: "thinking", text: "analysis" },
      { type: "text", text: "final" },
    ]
    seedRuntimeSession({
      detail: detailWithTurns([]),
      localTurns: [
        assistantTurnWithBlocks(
          "live-assistant",
          blocks,
          "2026-09-01T09:20:00Z"
        ),
      ],
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        assistantTurnWithBlocks(
          "parser-assistant",
          blocks,
          "2026-09-01T09:20:00Z"
        ),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual([])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["parser-assistant"])
  })

  it("keeps an assistant-only overlay when only its content matches", async () => {
    const blocks: ContentBlock[] = [
      { type: "thinking", text: "analysis" },
      { type: "text", text: "final" },
    ]
    const localTurns = [
      assistantTurnWithBlocks("live-assistant", blocks, "2026-09-01T09:21:00Z"),
    ]
    seedRuntimeSession({ detail: detailWithTurns([]), localTurns })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        assistantTurnWithBlocks(
          "parser-assistant",
          blocks,
          "2026-09-01T10:21:00Z"
        ),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["parser-assistant", "live-assistant"])
  })

  it("replaces every parser partial in a proven assistant-only window", async () => {
    const localTurns = [
      userTurn("wire-user", "inspect", "2026-09-01T09:22:00Z"),
      assistantTurn(
        "wire-assistant",
        "complete answer",
        "2026-09-01T09:22:10Z"
      ),
    ]
    seedRuntimeSession({
      detail: detailWithTurns([], {
        turns_offset: 120,
        turns_total: 120,
        prefix_hash: "0000000000000120",
      }),
      localTurns,
      batchBoundaryIndex: 120,
      batchBoundaryPrefixHash: "0000000000000120",
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns(
        [
          assistantTurn("parser-head", "partial head", "2026-09-01T09:22:05Z"),
          assistantTurn("parser-tail", "partial tail", "2026-09-01T09:22:06Z"),
        ],
        {
          turns_offset: 120,
          turns_total: 122,
          prefix_hash: "0000000000000120",
        }
      )
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID, {
      preserveLive: true,
    })
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["wire-user", "wire-assistant"])
  })

  it("keeps an interrupted assistant when persisted detail ends at its user", async () => {
    const prompt = userTurn(
      "shared-user",
      "cancel this",
      "2026-09-01T09:25:00Z"
    )
    const interrupted: MessageTurn = {
      id: "cancel-outcome",
      role: "assistant",
      blocks: [],
      timestamp: "2026-09-01T09:25:01Z",
      outcome: {
        status: "interrupted",
        stop_reason: "cancelled",
        source: "user_stop",
        provider_turn_id: "provider-turn-1",
      },
    }
    const localTurns = [prompt, interrupted]
    seedRuntimeSession({ detail: detailWithTurns([]), localTurns })
    mockGetFolderConversation.mockResolvedValueOnce(detailWithTurns([prompt]))

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["shared-user", "cancel-outcome"])
  })

  it("does not append a timestamp-aligned local user after its persisted reply", async () => {
    const localPrompt = userTurn(
      "wire-user",
      "finish this",
      "2026-09-01T09:25:30Z"
    )
    seedRuntimeSession({
      detail: detailWithTurns([]),
      localTurns: [localPrompt],
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("parser-user", "finish this", "2026-09-01T09:25:30Z"),
        assistantTurn(
          "parser-assistant",
          "persisted reply",
          "2026-09-01T09:25:31Z"
        ),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual([localPrompt])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["parser-user", "parser-assistant"])
  })

  it("does not reinsert an aligned user-only group before a later local replacement", async () => {
    const localTurns = [
      userTurn("wire-user-1", "first", "2026-09-01T09:25:30Z"),
      userTurn("wire-user-2", "second", "2026-09-01T09:26:30Z"),
      assistantTurn("wire-a-2", "complete second", "2026-09-01T09:26:31Z"),
    ]
    seedRuntimeSession({ detail: detailWithTurns([]), localTurns })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("parser-user-1", "first", "2026-09-01T09:25:30Z"),
        assistantTurn("parser-a-1", "persisted first", "2026-09-01T09:25:31Z"),
        userTurn("parser-user-2", "second", "2026-09-01T09:26:30Z"),
        assistantTurn("parser-a-2", "partial second", "2026-09-01T09:26:31Z"),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["parser-user-1", "parser-a-1", "wire-user-2", "wire-a-2"])
  })

  it("does not retire a later aligned round before an earlier local round", async () => {
    const localTurns = [
      userTurn("local-user-1", "first prompt", "2026-09-01T09:26:00Z"),
      assistantTurn("local-a-1", "first complete", "2026-09-01T09:26:01Z"),
      userTurn("shared-user-2", "second prompt", "2026-09-01T09:27:00Z"),
      assistantTurn("shared-a-2", "second complete", "2026-09-01T09:27:01Z"),
    ]
    seedRuntimeSession({ detail: detailWithTurns([]), localTurns })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("shared-user-2", "second prompt", "2026-09-01T09:27:00Z"),
        assistantTurn("shared-a-2", "second complete", "2026-09-01T09:27:01Z"),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["local-user-1", "local-a-1", "shared-user-2", "shared-a-2"])
  })

  it("keeps local rounds when their persisted identities cross", async () => {
    const localTurns = [
      userTurn("local-user-2", "second prompt", "2026-09-01T09:29:00Z"),
      assistantTurn("local-a-2", "second reply", "2026-09-01T09:29:01Z"),
      userTurn("local-user-1", "first prompt", "2026-09-01T09:28:00Z"),
      assistantTurn("local-a-1", "first reply", "2026-09-01T09:28:01Z"),
    ]
    seedRuntimeSession({ detail: detailWithTurns([]), localTurns })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("parser-user-1", "first prompt", "2026-09-01T09:28:00Z"),
        assistantTurn("parser-a-1", "first reply", "2026-09-01T09:28:01Z"),
        userTurn("parser-user-2", "second prompt", "2026-09-01T09:29:00Z"),
        assistantTurn("parser-a-2", "second reply", "2026-09-01T09:29:01Z"),
      ])
    )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual(localTurns)
    const timelineIds = selectHistoricalTimelineTurns(state, CID).map(
      (entry) => entry.turn.id
    )
    expect(timelineIds.indexOf("local-user-2")).toBeLessThan(
      timelineIds.indexOf("local-user-1")
    )
  })

  it("does not reuse a retired batch boundary for an identical newer round", async () => {
    const older = [
      userTurn("wire-user-1", "continue", "2026-09-01T09:34:00Z"),
      assistantTurn("wire-a-1", "done", "2026-09-01T09:34:01Z"),
    ]
    const newer = [
      userTurn("wire-user-2", "continue", "2026-09-01T09:35:00Z"),
      assistantTurn("wire-a-2", "done", "2026-09-01T09:35:01Z"),
    ]
    const persistedOlder = detailWithTurns([
      userTurn("wire-user-1", "continue", "2026-09-01T09:34:00Z"),
      assistantTurn("wire-a-1", "done", "2026-09-01T09:34:01Z"),
    ])
    seedRuntimeSession({
      detail: detailWithTurns([]),
      localTurns: [...older, ...newer],
      batchBoundaryIndex: 0,
    })
    mockGetFolderConversation
      .mockResolvedValueOnce(persistedOlder)
      .mockResolvedValueOnce(persistedOlder)

    const actions = useConversationRuntimeStore.getState().actions
    actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-user-2", "wire-a-2"])

    actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(
      state.byConversationId.get(CID)?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-user-2", "wire-a-2"])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["wire-user-1", "wire-a-1", "wire-user-2", "wire-a-2"])
  })

  it("does not trust a batch boundary after its prefix was rewritten", async () => {
    const localTurns = [
      userTurn("wire-user", "continue", "2026-09-01T09:34:10Z"),
      assistantTurn("wire-a", "done", "2026-09-01T09:34:11Z"),
    ]
    seedRuntimeSession({
      detail: detailWithTurns([], {
        turns_offset: 2,
        turns_total: 2,
        prefix_hash: "00000000000000aa",
      }),
      localTurns,
      batchBoundaryIndex: 2,
      batchBoundaryPrefixHash: "00000000000000aa",
    })
    mockGetFolderConversation
      .mockResolvedValueOnce(
        detailWithTurns([], {
          turns_offset: 2,
          turns_total: 4,
          prefix_hash: "00000000000000bb",
        })
      )
      .mockResolvedValueOnce(
        detailWithTurns([
          userTurn("history-user", "history", "2026-09-01T08:34:00Z"),
          assistantTurn("history-a", "history", "2026-09-01T08:34:01Z"),
          userTurn("parser-user", "continue", "2026-09-01T08:34:10Z"),
          assistantTurn("parser-a", "done", "2026-09-01T08:34:11Z"),
        ])
      )

    useConversationRuntimeStore.getState().actions.refetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()

    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-user", "wire-a"])
  })

  it("does not promote delegate content fallback into retirement identity", async () => {
    vi.useFakeTimers()
    const delegateSummary = {
      ...detailWithTurns([]).summary,
      kind: "delegate" as const,
      parent_id: 1,
      delegation_task_status: "completed" as const,
    }
    const localTurns = [
      userTurn("wire-new-user", "continue", "2026-09-01T09:35:00Z"),
      assistantTurn("wire-new-a", "done", "2026-09-01T09:35:01Z"),
    ]
    seedRuntimeSession({
      detail: detailWithTurns(
        [userTurn("old-unanswered-user", "continue", "2026-09-01T08:35:00Z")],
        { summary: delegateSummary }
      ),
      localTurns,
      liveOwnsActiveTurn: true,
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns(
        [
          userTurn("old-unanswered-user", "continue", "2026-09-01T08:35:00Z"),
          assistantTurn("old-a", "done", "2026-09-01T08:35:01Z"),
        ],
        { summary: delegateSummary }
      )
    )

    useConversationRuntimeStore
      .getState()
      .actions.syncDelegateTerminalDetail(CID)
    await vi.advanceTimersByTimeAsync(0)

    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.localTurns.map((turn) => turn.id)
    ).toEqual(["wire-new-user", "wire-new-a"])
  })

  it("lets Manual Reload replace unrelated local overlays with disk", async () => {
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")]),
      localTurns: [
        userTurn("wire-user", "local prompt", "2026-09-01T09:30:00Z"),
        assistantTurn(
          "live-assistant",
          "local complete reply",
          "2026-09-01T09:30:10Z"
        ),
      ],
      backgroundTurns: [
        {
          turn: assistantTurn(
            "background-a",
            "background overlay",
            "2026-09-01T09:30:20Z"
          ),
          watermark: 100,
        },
      ],
      lastTurnOwned: true,
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("parser-user", "disk prompt", "2026-09-01T10:30:00Z"),
        assistantTurn("parser-assistant", "disk reply", "2026-09-01T10:30:10Z"),
      ])
    )

    useConversationRuntimeStore
      .getState()
      .actions.reloadDetail(CID, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()

    const state = useConversationRuntimeStore.getState()
    expect(state.byConversationId.get(CID)?.localTurns).toEqual([])
    expect(state.byConversationId.get(CID)?.backgroundTurns).toEqual([])
    expect(
      selectHistoricalTimelineTurns(state, CID).map((entry) => entry.turn.id)
    ).toEqual(["parser-user", "parser-assistant"])
  })

  it("resets active owner proof while preserving queued prompts on Manual Reload", async () => {
    const queued = userTurn(
      "queued-user",
      "next prompt",
      "2026-09-01T10:30:20Z"
    )
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")]),
      localTurns: [
        userTurn("wire-user", "active prompt", "2026-09-01T10:30:00Z"),
      ],
      optimisticTurns: [queued],
      queuedOptimisticTurnIds: [queued.id],
      liveMessage: liveMessage("active-live", "active reply"),
      syncState: "awaiting_persist",
      activeTurnToken: "active-token",
      lastTurnOwned: true,
      liveOwnsActiveTurn: true,
      historyAssistantBaseline: 20,
      batchBoundaryIndex: 40,
      batchBoundaryPrefixHash: "0000000000000040",
    })
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("disk-user", "disk prompt", "2026-09-01T10:29:00Z"),
        assistantTurn("disk-a", "disk reply", "2026-09-01T10:29:01Z"),
      ])
    )

    useConversationRuntimeStore
      .getState()
      .actions.reloadDetail(CID, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()

    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.optimisticTurns.map((turn) => turn.id)).toEqual([
      "queued-user",
    ])
    expect(session?.queuedOptimisticTurnIds).toEqual(["queued-user"])
    expect(session).toMatchObject({
      localTurns: [],
      liveMessage: null,
      syncState: "idle",
      activeTurnToken: null,
      lastTurnOwned: false,
      liveOwnsActiveTurn: false,
      historyAssistantBaseline: null,
      batchBoundaryIndex: null,
      batchBoundaryPrefixHash: null,
    })
  })

  it("does not auto-start delegate polling after Manual Reload", async () => {
    const delegateSummary = {
      ...detailWithTurns([]).summary,
      kind: "delegate" as const,
      parent_id: 1,
      delegation_task_status: "completed" as const,
    }
    const disk = detailWithTurns(
      [
        userTurn("disk-user", "inspect", "2026-09-01T10:30:30Z"),
        assistantTurn("disk-a", "complete", "2026-09-01T10:30:31Z"),
      ],
      { summary: delegateSummary }
    )
    seedRuntimeSession({ detail: disk })
    mockGetFolderConversation.mockResolvedValue(disk)

    useConversationRuntimeStore
      .getState()
      .actions.reloadDetail(CID, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()

    expect(mockGetFolderConversation).toHaveBeenCalledTimes(1)
  })

  it("lets Manual Reload replace a stale loaded prefix with a fresh window", async () => {
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")], {
        history_window: {
          has_more_before: true,
          total_turn_count: 2,
          total_user_turn_count: 1,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      }),
    })
    const fresh = detailWithTurns(
      [
        userTurn("parser-user", "fresh prompt", "2026-09-01T10:31:00Z"),
        assistantTurn(
          "parser-assistant",
          "fresh reply",
          "2026-09-01T10:31:10Z"
        ),
      ],
      {
        history_window: {
          has_more_before: true,
          total_turn_count: 4,
          total_user_turn_count: 2,
          user_turn_limit: 20,
          returned_user_turn_count: 1,
        },
      }
    )
    mockGetFolderConversation.mockResolvedValueOnce(fresh)

    useConversationRuntimeStore
      .getState()
      .actions.reloadDetail(CID, { reason: "manual_reload" })
    await Promise.resolve()
    await Promise.resolve()

    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.detail?.turns.map((turn) => turn.id)
    ).toEqual(["parser-user", "parser-assistant"])
  })

  it("does not let a passive refetch supersede Manual Reload", async () => {
    let resolveReload!: (detail: DbConversationDetail) => void
    let resolveRefetch!: (detail: DbConversationDetail) => void
    const reload = new Promise<DbConversationDetail>((resolve) => {
      resolveReload = resolve
    })
    const refetch = new Promise<DbConversationDetail>((resolve) => {
      resolveRefetch = resolve
    })
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")]),
      localTurns: [
        userTurn("local-user", "local prompt", "2026-09-01T09:32:00Z"),
        assistantTurn("local-a", "local reply", "2026-09-01T09:32:01Z"),
      ],
    })
    mockGetFolderConversation
      .mockReturnValueOnce(reload)
      .mockReturnValueOnce(refetch)

    const actions = useConversationRuntimeStore.getState().actions
    actions.reloadDetail(CID, { reason: "manual_reload" })
    actions.refetchDetail(CID)
    resolveReload(
      detailWithTurns([
        userTurn("reloaded-user", "disk prompt", "2026-09-01T10:32:00Z"),
        assistantTurn("reloaded-a", "disk reply", "2026-09-01T10:32:01Z"),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()
    resolveRefetch(
      detailWithTurns([
        userTurn("stale-user", "stale prompt", "2026-09-01T08:32:00Z"),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()

    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.detail?.turns.map((turn) => turn.id)).toEqual([
      "reloaded-user",
      "reloaded-a",
    ])
    expect(session?.localTurns).toEqual([])
  })

  it("lets a fork refetch supersede a pre-fork Manual Reload", async () => {
    let resolveReload!: (detail: DbConversationDetail) => void
    let resolveForkRefetch!: (detail: DbConversationDetail) => void
    const reload = new Promise<DbConversationDetail>((resolve) => {
      resolveReload = resolve
    })
    const forkRefetch = new Promise<DbConversationDetail>((resolve) => {
      resolveForkRefetch = resolve
    })
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("parent-user")]),
      localTurns: [assistantTurn("live-parent-reply")],
    })
    mockGetFolderConversation
      .mockReturnValueOnce(reload)
      .mockReturnValueOnce(forkRefetch)

    const actions = useConversationRuntimeStore.getState().actions
    actions.reloadDetail(CID, { reason: "manual_reload" })
    actions.refetchDetail(CID, {
      preserveLive: true,
      supersedeAuthoritative: true,
    })

    expect(mockGetFolderConversation).toHaveBeenCalledTimes(2)
    resolveForkRefetch(
      detailWithTurns([userTurn("fork-user"), assistantTurn("fork-reply")])
    )
    await Promise.resolve()
    await Promise.resolve()
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.detail?.turns.map((turn) => turn.id)
    ).toEqual(["fork-user", "fork-reply"])

    resolveReload(
      detailWithTurns([userTurn("parent-user"), assistantTurn("parent-reply")])
    )
    await Promise.resolve()
    await Promise.resolve()
    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.detail?.turns.map((turn) => turn.id)
    ).toEqual(["fork-user", "fork-reply"])
  })

  it("does not let viewer sync supersede Manual Reload", async () => {
    let resolveReload!: (detail: DbConversationDetail) => void
    let resolveViewer!: (detail: DbConversationDetail) => void
    const reload = new Promise<DbConversationDetail>((resolve) => {
      resolveReload = resolve
    })
    const viewer = new Promise<DbConversationDetail>((resolve) => {
      resolveViewer = resolve
    })
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")]),
    })
    mockGetFolderConversation
      .mockReturnValueOnce(reload)
      .mockReturnValueOnce(viewer)

    const actions = useConversationRuntimeStore.getState().actions
    actions.reloadDetail(CID, { reason: "manual_reload" })
    actions.syncViewerDetail(CID)
    resolveReload(
      detailWithTurns([
        userTurn("reloaded-user", "disk prompt", "2026-09-01T10:33:00Z"),
        assistantTurn("reloaded-a", "disk reply", "2026-09-01T10:33:01Z"),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()
    resolveViewer(
      detailWithTurns([
        userTurn("stale-user", "stale prompt", "2026-09-01T08:33:00Z"),
      ])
    )
    await Promise.resolve()
    await Promise.resolve()

    expect(
      useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)
        ?.detail?.turns.map((turn) => turn.id)
    ).toEqual(["reloaded-user", "reloaded-a"])
  })

  it("clears invalidated pagination loading state when Manual Reload starts", async () => {
    let resolveReload!: (detail: DbConversationDetail) => void
    const reload = new Promise<DbConversationDetail>((resolve) => {
      resolveReload = resolve
    })
    seedRuntimeSession({
      detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")]),
      detailHistoryLoadingOlder: true,
      loadingOlderTurns: true,
    })
    mockGetFolderConversation.mockReturnValueOnce(reload)

    useConversationRuntimeStore
      .getState()
      .actions.reloadDetail(CID, { reason: "manual_reload" })

    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
    ).toMatchObject({
      detailHistoryLoadingOlder: false,
      loadingOlderTurns: false,
    })

    resolveReload(
      detailWithTurns([userTurn("disk-user"), assistantTurn("disk-a")])
    )
    await Promise.resolve()
    await Promise.resolve()
  })

  it.each(["actions", "exported"] as const)(
    "does not retain the Manual Reload exclusion across an %s runtime reset",
    async (resetKind) => {
      let resolveReload!: (detail: DbConversationDetail) => void
      const pendingReload = new Promise<DbConversationDetail>((resolve) => {
        resolveReload = resolve
      })
      seedRuntimeSession({
        detail: detailWithTurns([userTurn("old-user"), assistantTurn("old-a")]),
      })
      mockGetFolderConversation
        .mockReturnValueOnce(pendingReload)
        .mockResolvedValueOnce(
          detailWithTurns([userTurn("fresh-user"), assistantTurn("fresh-a")])
        )

      useConversationRuntimeStore
        .getState()
        .actions.reloadDetail(CID, { reason: "manual_reload" })
      if (resetKind === "actions") {
        useConversationRuntimeStore.getState().actions.reset()
      } else {
        resetConversationRuntimeStore()
      }
      seedRuntimeSession({
        detail: detailWithTurns([
          userTurn("seed-user"),
          assistantTurn("seed-a"),
        ]),
      })
      useConversationRuntimeStore.getState().actions.refetchDetail(CID)

      expect(mockGetFolderConversation).toHaveBeenCalledTimes(2)
      await Promise.resolve()
      await Promise.resolve()

      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(CID)
          ?.detail?.turns.map((turn) => turn.id)
      ).toEqual(["fresh-user", "fresh-a"])

      resolveReload(
        detailWithTurns([userTurn("stale-user"), assistantTurn("stale-a")])
      )
      await Promise.resolve()
      await Promise.resolve()

      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(CID)
          ?.detail?.turns.map((turn) => turn.id)
      ).toEqual(["fresh-user", "fresh-a"])
    }
  )

  // Production parsers persist UUID / cursor-turn-N / grok-turn-N ids.
  // Promoted overlays always use live-${cid}-${msgId}. If retirement still
  // requires an id match, a settled refetch keeps both copies and the last
  // assistant renders twice for the rest of the tab.
  it.each([false, true] as const)(
    "settled refetch retires the live overlay when persist uses parser ids (preserveLive: %s)",
    async (preserveLive) => {
      const startedAt = Date.parse("2026-08-20T12:00:00.000Z")
      const persistTs = new Date(startedAt).toISOString()
      const { actions } = useConversationRuntimeStore.getState()

      seedRuntimeSession({
        detail: detailWithTurns([
          userTurn("u-old", "old prompt", "2026-08-20T11:00:00.000Z"),
          assistantTurn("a-old", "old reply", "2026-08-20T11:00:01.000Z"),
        ]),
        liveOwnsActiveTurn: false,
        syncState: "idle",
      })

      const covered = liveMessage("latest", "latest reply", startedAt)
      actions.setLiveMessage(CID, covered, true)
      actions.completeTurn(CID, covered)

      const pending = liveMessage(
        "pending",
        "not on disk yet",
        startedAt + 60_000
      )
      actions.setLiveMessage(CID, pending, true)
      actions.completeTurn(CID, pending)

      const overlayId = `live-${CID}-latest`
      expect(
        useConversationRuntimeStore
          .getState()
          .byConversationId.get(CID)
          ?.localTurns.some((t) => t.id === overlayId)
      ).toBe(true)

      mockGetFolderConversation.mockResolvedValueOnce(
        detailWithTurns([
          userTurn("u-old", "old prompt", "2026-08-20T11:00:00.000Z"),
          assistantTurn("a-old", "old reply", "2026-08-20T11:00:01.000Z"),
          userTurn("550e8400-e29b-41d4-a716-446655440000", "prompt", persistTs),
          assistantTurn("cursor-turn-0", "latest reply", persistTs),
        ])
      )
      actions.refetchDetail(CID, { preserveLive })
      await Promise.resolve()
      await Promise.resolve()

      const session = useConversationRuntimeStore
        .getState()
        .byConversationId.get(CID)!
      expect(session.localTurns.some((t) => t.id === overlayId)).toBe(false)
      expect(
        session.localTurns.some((t) => t.id === `live-${CID}-pending`)
      ).toBe(true)

      const latestCopies = selectHistoricalTimelineTurns(
        useConversationRuntimeStore.getState(),
        CID
      ).filter((entry) => {
        const block = entry.turn.blocks[0]
        return (
          entry.turn.role === "assistant" &&
          block?.type === "text" &&
          block.text === "latest reply"
        )
      })
      expect(latestCopies).toHaveLength(1)
      expect(latestCopies[0]?.turn.id).toBe("cursor-turn-0")
    }
  )

  it("settled refetch retires an aligned round when assistant clocks differ", async () => {
    const { actions } = useConversationRuntimeStore.getState()
    seedRuntimeSession({
      detail: detailWithTurns([
        userTurn("u-old", "old prompt", "2026-08-20T11:00:00.000Z"),
        assistantTurn("a-old", "old reply", "2026-08-20T11:00:01.000Z"),
      ]),
      localTurns: [
        userTurn("msg-latest", "latest prompt", "2026-08-20T12:00:00.000Z"),
        assistantTurn(
          "live-42-latest",
          "latest reply",
          "2026-08-20T12:00:01.000Z"
        ),
      ],
      liveOwnsActiveTurn: false,
      syncState: "idle",
    })

    mockGetFolderConversation.mockResolvedValueOnce(
      detailWithTurns([
        userTurn("u-old", "old prompt", "2026-08-20T11:00:00.000Z"),
        assistantTurn("a-old", "old reply", "2026-08-20T11:00:01.000Z"),
        userTurn(
          "550e8400-e29b-41d4-a716-446655440000",
          "latest prompt",
          "2026-08-20T12:00:00.000Z"
        ),
        assistantTurn(
          "cursor-turn-0",
          "latest reply",
          "2026-08-20T14:00:05.000Z"
        ),
      ])
    )
    actions.refetchDetail(CID, { preserveLive: false })
    await Promise.resolve()
    await Promise.resolve()

    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)!
    expect(session.localTurns.some((t) => t.id === "live-42-latest")).toBe(
      false
    )
    expect(session.localTurns.some((t) => t.id === "msg-latest")).toBe(false)

    const promptCopies = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    ).filter((entry) => {
      const block = entry.turn.blocks[0]
      return (
        entry.turn.role === "user" &&
        block?.type === "text" &&
        block.text === "latest prompt"
      )
    })
    expect(promptCopies).toHaveLength(1)

    const latestCopies = selectHistoricalTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    ).filter((entry) => {
      const block = entry.turn.blocks[0]
      return (
        entry.turn.role === "assistant" &&
        block?.type === "text" &&
        block.text === "latest reply"
      )
    })
    expect(latestCopies).toHaveLength(1)
    expect(latestCopies[0]?.turn.id).toBe("cursor-turn-0")
  })

  it("completeTurn against stale settled detail does not drop a new last assistant", () => {
    const { actions } = useConversationRuntimeStore.getState()
    const startedAt = Date.parse("2026-08-20T15:00:00.000Z")
    seedRuntimeSession({
      detail: detailWithTurns([
        userTurn("u-old", "old prompt", "2026-08-20T11:00:00.000Z"),
        assistantTurn("a-old", "old reply", "2026-08-20T11:00:01.000Z"),
      ]),
      localTurns: [
        userTurn("optimistic-new", "new prompt", "2026-08-20T15:00:00.000Z"),
      ],
      liveOwnsActiveTurn: false,
      syncState: "awaiting_persist",
    })

    const fresh = liveMessage("fresh", "fresh reply", startedAt)
    actions.completeTurn(CID, fresh)

    const local =
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns ?? []
    expect(local.some((t) => t.id === `live-${CID}-fresh`)).toBe(true)
    expect(local.some((t) => t.id === "optimistic-new")).toBe(true)
  })
})
