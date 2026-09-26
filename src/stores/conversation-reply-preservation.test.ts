import { afterEach, expect, it, vi } from "vitest"
import { getFolderConversation } from "@/lib/api"
import type { LiveMessage } from "@/contexts/acp-connections-context"
import type { DbConversationDetail, MessageTurn } from "@/lib/types"
import {
  getTimelineTurns,
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"

vi.mock("@/lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/api")>()),
  getFolderConversation: vi.fn(),
  saveTurnGenerationStat: vi.fn(async () => undefined),
}))

const CID = 909
const mockGet = vi.mocked(getFolderConversation)
const actions = () => useConversationRuntimeStore.getState().actions
const texts = () =>
  getTimelineTurns(CID).flatMap(({ turn }) =>
    turn.blocks.flatMap((block) => (block.type === "text" ? [block.text] : []))
  )
const turn = (
  id: string,
  role: "user" | "assistant",
  text: string,
  timestamp = "2026-09-24T10:00:00.000Z"
): MessageTurn => ({
  id,
  role,
  blocks: [{ type: "text", text }],
  timestamp,
})
function detail(
  turns: MessageTurn[],
  marker: string | null = null
): DbConversationDetail {
  return {
    summary: {
      id: CID,
      folder_id: 1,
      agent_type: "codex",
      title: "audit",
      title_locked: false,
      auto_title_finalized: false,
      status: "in_progress",
      awaiting_reply_token: null,
      kind: "regular",
      model: null,
      git_branch: null,
      external_id: "audit-session",
      message_count: turns.length,
      child_count: 0,
      created_at: "2026-09-24T09:00:00.000Z",
      updated_at: "2026-09-24T10:00:00.000Z",
      pinned_at: null,
    },
    turns,
    in_flight_user_turn_id: marker,
  }
}
const live = (id: string, text: string): LiveMessage => ({
  id,
  role: "assistant" as const,
  startedAt: Date.parse("2026-09-24T10:00:00.000Z"),
  content: [{ type: "text" as const, text }],
})
async function flush() {
  await Promise.resolve()
  await Promise.resolve()
}
async function seed(value: DbConversationDetail) {
  mockGet.mockResolvedValueOnce(value)
  actions().fetchDetail(CID)
  await flush()
}
afterEach(() => {
  resetConversationRuntimeStore()
  vi.useRealTimers()
  vi.resetAllMocks()
})

it("keeps a final already in detail before the live completion", async () => {
  vi.useFakeTimers()
  await seed(detail([]))
  const prompt = turn("p1", "user", "check")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  actions().setLiveMessage(CID, live("reply", "Checking."), true)
  const persisted = [
    prompt,
    turn("a1", "assistant", "Checking. Final: all passed."),
  ]
  mockGet.mockResolvedValueOnce(detail(persisted, prompt.id))
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  mockGet.mockResolvedValue(detail(persisted))
  actions().completeTurn(CID)
  // The full reply must not disappear even for the hydration delay window.
  expect(texts().join("\n")).toContain("Final: all passed.")
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts().join("\n")).toContain("Final: all passed.")
})

it("an older manual reload cannot erase a newly completed reply", async () => {
  const old = detail([
    turn("p0", "user", "old"),
    turn("a0", "assistant", "old reply"),
  ])
  await seed(old)
  let resolve!: (value: DbConversationDetail) => void
  mockGet.mockReturnValueOnce(
    new Promise((done) => {
      resolve = done
    })
  )
  actions().reloadDetail(CID, { reason: "manual_reload" })
  const prompt = turn("p1", "user", "new")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  actions().setLiveMessage(CID, live("new", "NEW COMPLETE REPLY"), true)
  actions().completeTurn(CID)
  expect(texts()).toContain("NEW COMPLETE REPLY")
  resolve(old)
  await flush()
  expect(texts()).toContain("NEW COMPLETE REPLY")
})

it("a later unrelated reply with the same prefix cannot retire a continuation", async () => {
  vi.useFakeTimers()
  const history = [
    turn("p0", "user", "old", "2026-09-24T09:00:00.000Z"),
    turn("a0", "assistant", "old reply", "2026-09-24T09:01:00.000Z"),
  ]
  await seed(detail(history))
  actions().setLiveMessage(CID, live("continuation", "OK"), true)
  actions().completeTurn(CID)
  expect(texts()).toContain("OK")
  mockGet.mockResolvedValueOnce(
    detail([
      ...history,
      turn("p1", "user", "different task", "2026-09-24T11:00:00.000Z"),
      turn(
        "a1",
        "assistant",
        "OK, different task completed.",
        "2026-09-24T11:01:00.000Z"
      ),
    ])
  )
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  expect(texts()).toContain("OK")
})

it("a stale ordinary refetch cannot clear a newly observed viewer reply", async () => {
  const old = detail([
    turn("p0", "user", "old"),
    turn("a0", "assistant", "old reply"),
  ])
  await seed(old)
  let resolve!: (value: DbConversationDetail) => void
  mockGet.mockReturnValueOnce(
    new Promise((done) => {
      resolve = done
    })
  )
  actions().refetchDetail(CID)
  actions().appendViewerUserTurn(CID, turn("p1", "user", "new"))
  actions().setLiveMessage(CID, live("new", "NEW STREAMING REPLY"), true)
  expect(texts()).toContain("NEW STREAMING REPLY")
  resolve(old)
  await flush()
  expect(texts()).toContain("NEW STREAMING REPLY")
})

it.each(["fetch", "refetch", "reload"] as const)(
  "%s cannot erase a continuation that completed during the read",
  async (kind) => {
    vi.useFakeTimers()
    const old = detail([
      turn("p0", "user", "old", "2026-09-24T09:00:00.000Z"),
      turn("a0", "assistant", "old reply", "2026-09-24T09:01:00.000Z"),
    ])
    if (kind !== "fetch") await seed(old)
    let resolve!: (value: DbConversationDetail) => void
    mockGet.mockReturnValueOnce(
      new Promise((done) => {
        resolve = done
      })
    )
    if (kind === "fetch") actions().fetchDetail(CID)
    if (kind === "refetch") actions().refetchDetail(CID)
    if (kind === "reload")
      actions().reloadDetail(CID, { reason: "manual_reload" })
    // No optimistic prompt and no awaiting_persist: internal continuation.
    actions().setLiveMessage(CID, live("continuation", "CONTINUED REPLY"), true)
    actions().completeTurn(CID)
    resolve(old)
    await flush()
    expect(texts()).toContain("old reply")
    expect(texts()).toContain("CONTINUED REPLY")
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.detailLoading
    ).toBe(false)
  }
)

it("does not treat growth of an older repeated prompt as persistence of the new reply", async () => {
  vi.useFakeTimers()
  const oldPrompt = turn("p0", "user", "repeat", "2026-09-24T09:00:00.000Z")
  await seed(
    detail([
      oldPrompt,
      turn("a0", "assistant", "OK", "2026-09-24T09:01:00.000Z"),
    ])
  )
  const prompt = turn("p1", "user", "repeat")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  actions().setLiveMessage(CID, live("new", "OK"), true)
  actions().completeTurn(CID)
  mockGet.mockResolvedValueOnce(
    detail([
      oldPrompt,
      turn(
        "a0",
        "assistant",
        "OK, old reply expanded.",
        "2026-09-24T09:01:00.000Z"
      ),
    ])
  )
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  expect(texts()).toContain("OK")
  expect(texts()).toContain("OK, old reply expanded.")
})

it("manual reload cannot discard a queued prompt activated during the read", async () => {
  const old = detail([])
  await seed(old)
  const prompt = turn("queued", "user", "QUEUED PROMPT NOW SENDING")
  actions().appendOptimisticTurn(CID, prompt, prompt.id, { queuePending: true })
  let resolve!: (value: DbConversationDetail) => void
  mockGet.mockReturnValueOnce(
    new Promise((done) => {
      resolve = done
    })
  )
  actions().reloadDetail(CID, { reason: "manual_reload" })
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  resolve(old)
  await flush()
  expect(texts()).toContain("QUEUED PROMPT NOW SENDING")
  expect(
    useConversationRuntimeStore.getState().byConversationId.get(CID)?.syncState
  ).toBe("awaiting_persist")
})

it.each(["before", "after"] as const)(
  "shows a boundary-aligned final with parser clock skew arriving %s completion",
  async (arrival) => {
    vi.useFakeTimers()
    await seed(detail([]))
    const prompt = turn("client", "user", "check")
    actions().appendOptimisticTurn(CID, prompt, prompt.id)
    actions().setLiveMessage(CID, live("short", "Checking."), true)
    const persisted = [
      turn("parser", "user", "check", "2026-09-24T10:00:00.010Z"),
      turn(
        "final",
        "assistant",
        "Checking. FINAL COMPLETE",
        "2026-09-24T10:00:01.000Z"
      ),
    ]
    if (arrival === "before") {
      mockGet.mockResolvedValueOnce(detail(persisted, "parser"))
      actions().refetchDetail(CID, { preserveLive: true })
      await flush()
    }
    mockGet.mockResolvedValue(detail(persisted))
    actions().completeTurn(CID)
    if (arrival === "before")
      expect(texts().join("\n")).toContain("FINAL COMPLETE")
    await vi.advanceTimersByTimeAsync(30_000)
    expect(texts().join("\n")).toContain("FINAL COMPLETE")
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns
    ).toEqual([])
  }
)

it("keeps two distinct repeated rounds with identical timestamps against unchanged history", async () => {
  vi.useFakeTimers()
  const old = detail([
    turn("old-p", "user", "repeat"),
    turn("old-a", "assistant", "OK, old full reply"),
  ])
  await seed(old)
  const prompt = turn("new-p", "user", "repeat")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  actions().completeTurn(CID, live("new", "OK"))
  mockGet.mockResolvedValueOnce(old)
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  expect(
    useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
      ?.localTurns.some((entry) => entry.id === "live-909-new")
  ).toBe(true)
  expect(texts()).toEqual(["repeat", "OK, old full reply", "repeat", "OK"])
})

it.each([
  "thinking",
  "tool absent",
  "tool missing output",
  "tool different output",
] as const)("shows the final while retaining accepted %s", async (kind) => {
  vi.useFakeTimers()
  await seed(detail([]))
  const prompt = turn("p", "user", "check")
  actions().appendOptimisticTurn(CID, prompt, prompt.id)
  const extra: LiveMessage["content"][number] =
    kind === "thinking"
      ? { type: "thinking", text: "UNIQUE CONTENT" }
      : {
          type: "tool_call",
          info: {
            tool_call_id: "check",
            title: "bash",
            kind: "execute",
            status: "completed",
            content: null,
            raw_input: "{}",
            raw_output_chunks: ["UNIQUE CONTENT"],
            raw_output_total_bytes: 14,
            locations: null,
            meta: null,
            images: [],
          },
        }
  actions().completeTurn(CID, {
    ...live("short", "Checking."),
    content: [{ type: "text", text: "Checking." }, extra],
  })
  const accepted = getTimelineTurns(CID)
    .flatMap(({ turn }) => turn.blocks)
    .filter((block) => block.type !== "text")
  expect(accepted.length).toBeGreaterThan(0)
  const persisted = turn("final", "assistant", "Checking. FINAL COMPLETE")
  if (kind === "tool missing output" || kind === "tool different output") {
    persisted.blocks.unshift({
      type: "tool_use",
      tool_use_id: "check",
      tool_name: "bash",
      input_preview: "{}",
      status: "completed",
      meta: null,
    })
  }
  if (kind === "tool different output") {
    persisted.blocks.unshift({
      type: "tool_result",
      tool_use_id: "check",
      output_preview: "DIFFERENT OUTPUT",
      is_error: false,
    })
  }
  mockGet.mockResolvedValue(detail([prompt, persisted]))
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts().join("\n")).toContain("FINAL COMPLETE")
  expect(getTimelineTurns(CID).flatMap(({ turn }) => turn.blocks)).toEqual(
    expect.arrayContaining(accepted)
  )
  expect(
    useConversationRuntimeStore.getState().byConversationId.get(CID)?.localTurns
      .length
  ).toBeGreaterThan(0)
  const covered = {
    ...persisted,
    blocks: [
      ...persisted.blocks,
      ...accepted.filter(
        (block) =>
          !persisted.blocks.some(
            (other) => JSON.stringify(other) === JSON.stringify(block)
          )
      ),
    ],
  }
  mockGet.mockResolvedValueOnce(detail([prompt, covered]))
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  expect(texts().join("\n")).toContain("FINAL COMPLETE")
  expect(getTimelineTurns(CID).flatMap(({ turn }) => turn.blocks)).toEqual(
    expect.arrayContaining(accepted)
  )
  expect(
    useConversationRuntimeStore.getState().byConversationId.get(CID)?.localTurns
  ).toEqual([])
})

it.each([
  "matching",
  "unrelated timestamp",
  "unrelated prompt",
  "different reasoning",
  "old reasoning with matching final timestamp",
] as const)(
  "hydrates thinking-only continuation only with its own reasoning and final: %s",
  async (variant) => {
    vi.useFakeTimers()
    const history = [
      turn("old-p", "user", "old", "2026-09-24T09:00:00.000Z"),
      turn("old-a", "assistant", "old answer", "2026-09-24T09:01:00.000Z"),
    ]
    await seed(detail(history))
    const thought = {
      type: "thinking",
      text: "Checking the final result",
    } as const
    actions().setLiveMessage(
      CID,
      { ...live("continuation", ""), content: [thought] },
      true
    )
    actions().completeTurn(CID)
    const reasoning: MessageTurn = {
      ...turn(
        "reason",
        "assistant",
        "",
        variant === "old reasoning with matching final timestamp"
          ? "2026-09-24T09:30:00.000Z"
          : variant === "unrelated timestamp"
            ? "2026-09-24T11:00:00.000Z"
            : undefined
      ),
      blocks: [
        {
          ...thought,
          text:
            variant === "different reasoning"
              ? "Unrelated reasoning"
              : thought.text,
        },
      ],
    }
    mockGet.mockResolvedValue(
      detail([
        ...history,
        ...(variant === "unrelated prompt"
          ? [turn("other", "user", "other task")]
          : []),
        reasoning,
        turn(
          "final",
          "assistant",
          "FINAL COMPLETE",
          variant === "old reasoning with matching final timestamp"
            ? "2026-09-24T10:00:00.000Z"
            : "2026-09-24T11:01:00.000Z"
        ),
      ])
    )
    await vi.advanceTimersByTimeAsync(30_000)
    if (variant === "matching") {
      expect(texts()).toContain("FINAL COMPLETE")
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(CID)
          ?.localTurns
      ).toEqual([])
    } else {
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(CID)?.detail
          ?.turns
      ).toEqual(history)
      expect(
        useConversationRuntimeStore.getState().byConversationId.get(CID)
          ?.localTurns.length
      ).toBeGreaterThan(0)
    }
    expect(
      getTimelineTurns(CID).flatMap(({ turn }) => turn.blocks)
    ).toContainEqual(thought)
  }
)

it("metadata and identity no-ops still allow manual reload cleanup", async () => {
  const old = detail([
    turn("p", "user", "old"),
    turn("a", "assistant", "persisted final"),
  ])
  await seed(old)
  const stale = live("stale", "old live")
  actions().setLiveMessage(CID, stale, true)
  let resolve!: (value: DbConversationDetail) => void
  mockGet.mockReturnValueOnce(
    new Promise((done) => {
      resolve = done
    })
  )
  actions().reloadDetail(CID, { reason: "manual_reload" })
  actions().setPendingCleanup(CID, true)
  actions().setLiveMessage(CID, stale, true)
  actions().setExternalId(CID, "audit-session")
  resolve(old)
  await flush()
  expect(
    useConversationRuntimeStore.getState().byConversationId.get(CID)
      ?.liveMessage
  ).toBeNull()
  expect(texts()).toContain("persisted final")
})

it("keeps polling a thinking-only continuation until its own trailing final is persisted", async () => {
  vi.useFakeTimers()
  const history = [
    turn("old-p", "user", "old", "2026-09-24T09:00:00.000Z"),
    turn("old-a", "assistant", "old answer", "2026-09-24T09:01:00.000Z"),
  ]
  await seed(detail(history))
  const thought = { type: "thinking", text: "Waiting for my final" } as const
  actions().setLiveMessage(
    CID,
    { ...live("continuation", ""), content: [thought] },
    true
  )
  actions().completeTurn(CID)
  const reasoning: MessageTurn = {
    ...turn("reason", "assistant", ""),
    blocks: [thought],
  }
  mockGet.mockResolvedValueOnce(detail([...history, reasoning]))
  mockGet.mockResolvedValue(
    detail([
      ...history,
      reasoning,
      turn(
        "final",
        "assistant",
        "CONTINUATION FINAL",
        "2026-09-24T10:01:00.000Z"
      ),
    ])
  )
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts()).toContain("CONTINUATION FINAL")
})

it("retained reasoning in an earlier round cannot retire a later identical round", async () => {
  vi.useFakeTimers()
  await seed(detail([]))
  const oldPrompt = turn("old-p", "user", "repeat")
  const thought = {
    type: "thinking",
    text: "unpersisted prior reasoning",
  } as const
  actions().appendOptimisticTurn(CID, oldPrompt, oldPrompt.id)
  actions().completeTurn(CID, {
    ...live("old", "OK"),
    content: [{ type: "text", text: "OK" }, thought],
  })
  const persisted = detail([
    oldPrompt,
    turn("old-a", "assistant", "OK, old full reply"),
  ])
  mockGet.mockResolvedValueOnce(persisted)
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  const newPrompt = turn("new-p", "user", "repeat")
  actions().appendOptimisticTurn(CID, newPrompt, newPrompt.id)
  actions().completeTurn(CID, live("new", "OK"))
  expect(
    useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
      ?.localTurns.map(({ id }) => id)
  ).toEqual(["old-p", "live-909-old", "new-p", "live-909-new"])
  mockGet.mockResolvedValue(persisted)
  actions().refetchDetail(CID, { preserveLive: true })
  await flush()
  await vi.advanceTimersByTimeAsync(30_000)
  expect(
    useConversationRuntimeStore
      .getState()
      .byConversationId.get(CID)
      ?.localTurns.map(({ id }) => id)
  ).toEqual(["old-p", "live-909-old", "new-p", "live-909-new"])
  expect(texts()).toEqual(["repeat", "OK, old full reply", "repeat", "OK"])
  expect(
    getTimelineTurns(CID).flatMap(({ turn }) => turn.blocks)
  ).toContainEqual(thought)
})

it.each([
  {
    name: "segmented final",
    fragments: ["Checking.", "FINAL COMPLETE"],
    persistedText: "Checking.\n\nFINAL COMPLETE",
    uncovered: [],
  },
  {
    name: "missing middle fragment",
    fragments: ["Checking.", "LOCAL ONLY", "FINAL COMPLETE"],
    persistedText: "Checking.\n\nFINAL COMPLETE\nFull persisted details.",
    uncovered: ["LOCAL ONLY"],
  },
  {
    name: "out-of-order fragments",
    fragments: ["FINAL COMPLETE", "Checking."],
    persistedText: "Checking.\n\nFINAL COMPLETE",
    uncovered: ["Checking."],
  },
  {
    name: "significant whitespace",
    fragments: ["const value = 1", "FINAL COMPLETE"],
    persistedText: "const  value = 1\n\nFINAL COMPLETE",
    uncovered: ["const value = 1"],
  },
  {
    name: "repeated fragment",
    fragments: ["FINAL COMPLETE", "FINAL COMPLETE"],
    persistedText: "Checking.\n\nFINAL COMPLETE\nFull persisted details.",
    uncovered: ["FINAL COMPLETE"],
  },
])(
  "merges ordered text coverage for $name while preserving thinking",
  async ({ fragments, persistedText, uncovered }) => {
    vi.useFakeTimers()
    await seed(detail([]))
    const prompt = turn("p", "user", "check")
    const thought = { type: "thinking", text: "unpersisted reasoning" } as const
    actions().appendOptimisticTurn(CID, prompt, prompt.id)
    actions().completeTurn(CID, {
      ...live("reply", ""),
      content: fragments.flatMap((text, index): LiveMessage["content"] => [
        { type: "text", text },
        ...(index === 0 ? [thought] : []),
      ]),
    })
    const persisted = turn("final", "assistant", persistedText)
    mockGet.mockResolvedValue(detail([prompt, persisted]))
    for (const read of [1, 2]) {
      actions().refetchDetail(CID, { preserveLive: true })
      await flush()
      expect(texts(), `read ${read}`).toEqual([
        "check",
        ...uncovered,
        persistedText,
      ])
      expect(
        getTimelineTurns(CID).flatMap(({ turn }) => turn.blocks)
      ).toContainEqual(thought)
    }
    mockGet.mockResolvedValueOnce(
      detail([prompt, { ...persisted, blocks: [thought, ...persisted.blocks] }])
    )
    actions().refetchDetail(CID, { preserveLive: true })
    await flush()
    expect(texts()).toEqual(["check", ...uncovered, persistedText])
    expect(
      useConversationRuntimeStore.getState().byConversationId.get(CID)
        ?.localTurns.length === 0
    ).toBe(uncovered.length === 0)
  }
)
