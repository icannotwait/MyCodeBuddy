import { afterEach, describe, expect, it, vi } from "vitest"

import type {
  LiveMessage,
  ToolCallInfo,
} from "@/contexts/acp-connections-context"
import { getFolderConversation } from "@/lib/api"
import type {
  ContentBlock,
  DbConversationDetail,
  MessageTurn,
} from "@/lib/types"
import {
  resetConversationRuntimeStore,
  selectTimelineTurns,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>()
  return {
    ...actual,
    getFolderConversation: vi.fn(actual.getFolderConversation),
  }
})

const mockGetFolderConversation = vi.mocked(getFolderConversation)
const CID = 66

function userTurn(id: string, text: string): MessageTurn {
  return {
    id,
    role: "user",
    blocks: [{ type: "text", text }],
    timestamp: "2026-09-26T09:41:06.000Z",
  }
}

function assistantTurn(id: string, blocks: ContentBlock[]): MessageTurn {
  return {
    id,
    role: "assistant",
    blocks,
    timestamp: "2026-09-26T09:41:11.000Z",
  }
}

function detailWithTurns(
  turns: MessageTurn[],
  inFlightUserTurnId: string | null
): DbConversationDetail {
  return {
    summary: {
      id: CID,
      folder_id: 1,
      agent_type: "grok",
      title: "t",
      title_locked: false,
      auto_title_finalized: false,
      status: "in_progress",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "sid-66",
      message_count: turns.length,
      child_count: 0,
      created_at: "2026-09-26T09:41:06.000Z",
      updated_at: "2026-09-26T09:41:11.000Z",
      pinned_at: null,
    },
    turns,
    session_stats: null,
    in_flight_user_turn_id: inFlightUserTurnId,
  }
}

function toolInfo(
  id: string,
  title: string,
  meta: Record<string, unknown> | null
): ToolCallInfo {
  return {
    tool_call_id: id,
    title,
    kind: "other",
    status: "completed",
    content: null,
    raw_input: null,
    raw_output_chunks: [],
    raw_output_total_bytes: 0,
    locations: null,
    meta,
    images: [],
  }
}

function liveWithTools(
  tools: ToolCallInfo[],
  text: string,
  gap?: { omittedToolCalls?: number; omittedLiveToolRefs?: number }
): LiveMessage {
  return {
    id: "live-66",
    role: "assistant",
    content: [
      { type: "text", text },
      ...tools.map((info) => ({ type: "tool_call" as const, info })),
    ],
    startedAt: Date.parse("2026-09-26T09:41:09.851Z"),
    snapshotOmittedToolCalls: gap?.omittedToolCalls,
    snapshotOmittedLiveToolRefs: gap?.omittedLiveToolRefs,
  }
}

function seed(input: {
  detail?: DbConversationDetail | null
  liveMessage?: LiveMessage | null
}) {
  useConversationRuntimeStore.setState({
    byConversationId: new Map([
      [
        CID,
        {
          conversationId: CID,
          externalId: "sid-66",
          dbConversationId: null,
          detail: input.detail ?? null,
          detailLoading: false,
          detailError: null,
          detailHistoryLoadingOlder: false,
          acpLoadError: null,
          localTurns: [],
          backgroundTurns: [],
          pendingBackgroundSettlements: [],
          optimisticTurns: [],
          queuedOptimisticTurnIds: [],
          liveMessage: input.liveMessage ?? null,
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
          pendingOutOfTurnContent: false,
          delegateSyncError: null,
          pendingCancel: null,
          softFence: false,
          ownerPreserve: false,
        },
      ],
    ]),
    conversationIdByExternalId: new Map(),
  })
}

function toolUseIds(): string[] {
  const turns = selectTimelineTurns(useConversationRuntimeStore.getState(), CID)
  const ids: string[] = []
  for (const entry of turns) {
    for (const block of entry.turn.blocks) {
      if (block.type === "tool_use" && block.tool_use_id)
        ids.push(block.tool_use_id)
    }
  }
  return ids
}

const delegationMeta = {
  "codeg.delegation": { child_conversation_id: 84, status: "running" },
}

afterEach(() => {
  resetConversationRuntimeStore()
  mockGetFolderConversation.mockReset()
})

describe("fetchDetail when the attach snapshot omitted tool calls", () => {
  it("loads disk even though a live message is already hydrated", async () => {
    const disk = detailWithTurns(
      [
        userTurn("grok-turn-7", "继续"),
        assistantTurn("grok-turn-8", [
          { type: "text", text: "Working" },
          {
            type: "tool_use",
            tool_use_id: "call-late-delegate",
            tool_name: "delegate_to_agent",
            input_preview: "{}",
            meta: delegationMeta,
          },
        ]),
      ],
      null
    )
    mockGetFolderConversation.mockResolvedValue(disk)
    seed({
      liveMessage: liveWithTools(
        [
          toolInfo("call-late-delegate", "call-late-delegate", {
            "codeg.snapshot_omitted": true,
          }),
        ],
        "Working",
        { omittedToolCalls: 45, omittedLiveToolRefs: 1 }
      ),
    })

    useConversationRuntimeStore.getState().actions.fetchDetail(CID)
    await Promise.resolve()
    await Promise.resolve()

    expect(mockGetFolderConversation).toHaveBeenCalled()
    const session = useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
    expect(session?.detail?.turns).toHaveLength(2)
    expect(session?.liveMessage?.id).toBe("live-66")
    expect(toolUseIds().filter((id) => id === "call-late-delegate")).toEqual([
      "call-late-delegate",
    ])
  })

  it("still skips disk when the live message's tool refs all resolved", () => {
    seed({
      liveMessage: liveWithTools(
        [toolInfo("call-early", "read_file", null)],
        "Working"
      ),
    })
    useConversationRuntimeStore.getState().actions.fetchDetail(CID)
    expect(mockGetFolderConversation).not.toHaveBeenCalled()
  })
})

describe("timeline after a truncated snapshot hydrate", () => {
  const prompt = userTurn("grok-turn-7", "继续")
  const diskReply = assistantTurn("grok-turn-8", [
    { type: "text", text: "Working" },
    {
      type: "tool_use",
      tool_use_id: "call-kept",
      tool_name: "read_file",
      input_preview: "{}",
    },
    {
      type: "tool_use",
      tool_use_id: "call-late-delegate",
      tool_name: "delegate_to_agent",
      input_preview: "{}",
      meta: delegationMeta,
    },
    {
      type: "tool_use",
      tool_use_id: "call-continue",
      tool_name: "continue_delegation",
      input_preview: "{}",
      meta: {
        "codeg.delegation": { child_conversation_id: 73, status: "completed" },
      },
    },
  ])

  it("hides the persisted reply when the live snapshot resolved every ref", () => {
    seed({
      detail: detailWithTurns([prompt, diskReply], prompt.id),
      liveMessage: liveWithTools(
        [
          toolInfo("call-kept", "read_file", null),
          toolInfo("call-late-delegate", "delegate_to_agent", delegationMeta),
          toolInfo("call-continue", "continue_delegation", {
            "codeg.delegation": {
              child_conversation_id: 73,
              status: "completed",
            },
          }),
        ],
        "Working"
      ),
    })
    const turns = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(turns.some((entry) => entry.turn.id === "grok-turn-8")).toBe(false)
    expect(
      toolUseIds().filter((id) => id === "call-late-delegate")
    ).toHaveLength(1)
  })

  it("keeps disk delegation cards once and does not double-render live copies", () => {
    seed({
      detail: detailWithTurns([prompt, diskReply], prompt.id),
      liveMessage: liveWithTools(
        [
          toolInfo("call-kept", "read_file", null),
          toolInfo("call-late-delegate", "call-late-delegate", {
            "codeg.snapshot_omitted": true,
          }),
          toolInfo("call-continue", "continue_delegation", {
            "codeg.delegation": {
              child_conversation_id: 73,
              status: "completed",
            },
          }),
        ],
        "Working through the tail",
        { omittedToolCalls: 45, omittedLiveToolRefs: 1 }
      ),
    })
    const turns = selectTimelineTurns(
      useConversationRuntimeStore.getState(),
      CID
    )
    expect(turns.some((entry) => entry.turn.id === "grok-turn-8")).toBe(true)
    expect(toolUseIds()).toEqual([
      "call-kept",
      "call-late-delegate",
      "call-continue",
    ])
    const delegate = turns
      .flatMap((entry) => entry.turn.blocks)
      .find(
        (block) =>
          block.type === "tool_use" &&
          block.tool_use_id === "call-late-delegate"
      )
    expect(delegate?.type).toBe("tool_use")
    if (delegate?.type === "tool_use") {
      expect(delegate.meta).toEqual(delegationMeta)
      expect(delegate.tool_name).toBe("delegate_to_agent")
    }
    const text = turns
      .flatMap((entry) => entry.turn.blocks)
      .flatMap((block) => (block.type === "text" ? [block.text] : []))
      .join("")
    expect(text).toContain("Working")
    expect(text).toContain("through the tail")
    expect(text.match(/Working/g)).toHaveLength(1)
  })
})
