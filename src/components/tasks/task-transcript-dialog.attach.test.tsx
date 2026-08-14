/**
 * The transcript dialog's attach must track the task's live connection FORWARD,
 * never latch it at mount and never fall back to null.
 *
 * The latch made the dialog a persisted-transcript reader for its whole
 * lifetime whenever it came up empty, and a persisted render paints the running
 * turn's unfinished tool calls as settled — a `get_delegation_status` blocking
 * on its sub-agent shows a green ✓ for the entire wait. But plain re-derivation
 * is not the fix either: detaching the instant the task settles races the final
 * turn-complete the bridge needs to promote the live reply.
 */

import { act, render } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { WorkTask, WorkTaskStatus } from "@/lib/types"

import { TaskTranscriptDialog } from "./task-transcript-dialog"

const attachDelegationChild = vi.fn()
const detachDelegationChild = vi.fn()

vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({ attachDelegationChild, detachDelegationChild }),
}))
vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn().mockResolvedValue({
    summary: { agent_type: "claude_code" },
  }),
  workTaskEvents: vi.fn().mockResolvedValue([]),
}))
vi.mock("@/stores/app-workspace-store", () => {
  const state = {
    folders: [{ id: 1, path: "/repo", default_agent_type: null }],
  }
  return {
    useAppWorkspaceStore: (selector: (s: typeof state) => unknown) =>
      selector(state),
  }
})
/** The streaming surface itself is out of scope — record the connection it was
 *  handed so the test can assert what the viewer is pointed at. */
let renderedConnectionId: string | null | undefined
vi.mock("@/components/message/live-transcript-view", () => ({
  LiveTranscriptView: (props: { connectionId: string | null }) => {
    renderedConnectionId = props.connectionId
    return <div data-testid="live-transcript" />
  },
}))
vi.mock("./task-card", () => ({ StatusChip: () => <span /> }))

function makeTask(
  status: WorkTaskStatus,
  connectionId: string | null
): WorkTask {
  return {
    id: 9,
    folder_id: 1,
    title: "t",
    config: { agent_type: "claude_code" },
    status,
    failure_reason: null,
    last_error: null,
    run_seq: 1,
    sort_order: 1,
    worktree_folder_id: null,
    conversation_id: 100,
    connection_id: connectionId,
    base_branch: null,
    base_sha: null,
    work_branch: null,
    cleanup_state: null,
    verdict: null,
    result_summary: null,
    files_changed: null,
    additions: null,
    deletions: null,
    merge_commit: null,
    preflight: null,
    archived_at: null,
    scheduled_at: null,
  } as unknown as WorkTask
}

function tree(task: WorkTask) {
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TaskTranscriptDialog open onOpenChange={() => {}} task={task} />
    </NextIntlClientProvider>
  )
}

/** Mount and let the body's own round-marker fetch settle, so nothing lands
 *  outside `act` mid-assertion. */
async function mount(task: WorkTask) {
  const utils = render(tree(task))
  await act(async () => {})
  return utils
}

async function update(
  rerender: (ui: React.ReactElement) => void,
  task: WorkTask
) {
  await act(async () => {
    rerender(tree(task))
  })
}

function attachedIds(): string[] {
  return attachDelegationChild.mock.calls.map(
    (c) => (c[0] as { connectionId: string }).connectionId
  )
}

beforeEach(() => {
  attachDelegationChild.mockClear()
  detachDelegationChild.mockClear()
  renderedConnectionId = undefined
})

describe("work-task transcript dialog attach", () => {
  it("attaches to the live connection with hydration", async () => {
    await mount(makeTask("running", "conn-a"))
    expect(attachDelegationChild).toHaveBeenCalledTimes(1)
    expect(attachDelegationChild.mock.calls[0][0]).toMatchObject({
      connectionId: "conn-a",
      parentConnectionId: "conn-a",
      parentToolUseId: "work-task-9",
      hydrate: true,
    })
    expect(renderedConnectionId).toBe("conn-a")
  })

  it("never attaches for a task that settled long ago", async () => {
    // Its `connection_id` column is stale — the backend disconnected at
    // TurnComplete — so attaching would open a stream for a connection that
    // no longer exists.
    await mount(makeTask("review", "conn-dead"))
    expect(attachDelegationChild).not.toHaveBeenCalled()
    expect(renderedConnectionId).toBeNull()
  })

  it("picks up the connection that appears AFTER open (preparing → running)", async () => {
    // The 进行中 column covers `preparing`, which `isLive` excludes: a re-run
    // generation is shown there while its connection is still being made. The
    // old mount-latch left this dialog attached to nothing forever.
    const { rerender } = await mount(
      makeTask("preparing", "conn-previous-dead")
    )
    expect(attachDelegationChild).not.toHaveBeenCalled()

    await update(rerender, makeTask("running", "conn-new"))
    expect(attachedIds()).toEqual(["conn-new"])
    expect(renderedConnectionId).toBe("conn-new")
  })

  it("moves to the next generation's connection, detaching the previous", async () => {
    const { rerender } = await mount(makeTask("running", "conn-a"))
    await update(rerender, makeTask("running", "conn-b"))
    expect(attachedIds()).toEqual(["conn-a", "conn-b"])
    expect(detachDelegationChild).toHaveBeenCalledWith("conn-a")
    expect(renderedConnectionId).toBe("conn-b")
  })

  it("holds the attach when the task settles, so the final turn still lands", async () => {
    const { rerender } = await mount(makeTask("running", "conn-a"))
    await update(rerender, makeTask("review", "conn-a"))
    expect(detachDelegationChild).not.toHaveBeenCalled()
    expect(renderedConnectionId).toBe("conn-a")
  })

  it("holds the attach across the merging interval that clears connection_id", async () => {
    // `begin_merge` nulls `connection_id` in the same update that sets
    // `merging`; falling back to null there would drop the stream.
    const { rerender } = await mount(makeTask("running", "conn-a"))
    await update(rerender, makeTask("merging", null))
    expect(detachDelegationChild).not.toHaveBeenCalled()
    expect(renderedConnectionId).toBe("conn-a")

    // …and adopts the merge generation's connection once it exists.
    await update(rerender, makeTask("merging", "conn-merge"))
    expect(attachedIds()).toEqual(["conn-a", "conn-merge"])
    expect(detachDelegationChild).toHaveBeenCalledWith("conn-a")
  })

  it("detaches on close", async () => {
    const { unmount } = await mount(makeTask("running", "conn-a"))
    unmount()
    expect(detachDelegationChild).toHaveBeenCalledWith("conn-a")
  })
})
