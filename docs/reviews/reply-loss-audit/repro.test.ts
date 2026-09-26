// Audit reproductions: failed at the audit baseline; pass after remediation.
// Kept outside the normal test suite; see the adjacent audit config.
import { afterEach, expect, it, vi } from "vitest"
import { getFolderConversation } from "@/lib/api"
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
const live = (id: string, text: string) => ({
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

it("audit: keeps a final already in detail before the live completion", async () => {
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
  await vi.advanceTimersByTimeAsync(30_000)
  expect(texts().join("\n")).toContain("Final: all passed.")
})

it("audit: an older manual reload cannot erase a newly completed reply", async () => {
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

it("audit: a later unrelated reply with the same prefix cannot retire a continuation", async () => {
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

it("audit: a stale ordinary refetch cannot clear a newly observed viewer reply", async () => {
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
