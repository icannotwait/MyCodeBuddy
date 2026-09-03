import { act, cleanup, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type {
  SharedActiveTurn,
  SharedQueuedPrompt,
  SnapshotPatch,
} from "@/lib/snapshot-denormalize"
import {
  __connectionsReducerForTests,
  type ConnectionState,
} from "@/contexts/acp-connections-context"

import { SharedMessageQueueDisplay } from "./shared-message-queue-display"

type ConnectionsAction = Parameters<typeof __connectionsReducerForTests>[1]
type SharedSessionEvent = Extract<
  ConnectionsAction,
  { type: "SHARED_SESSION_EVENT" }
>["event"]

function queued(
  queueItemId: string,
  enqueueSeq: number,
  visibleText: string | null,
  state: SharedQueuedPrompt["state"] = "queued",
  attachmentCount = 0
): SharedQueuedPrompt {
  return {
    queueItemId,
    enqueueSeq,
    clientMessageId: `m-${queueItemId}`,
    visibleText,
    visibleTextTruncated: false,
    attachmentCount,
    submittedAt: "2026-08-16T00:00:00.000Z",
    state,
  }
}

function deferred() {
  let resolve!: () => void
  let reject!: (error: Error) => void
  const promise = new Promise<void>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function renderQueue(
  queue: SharedQueuedPrompt[],
  onCancel: (queueItemId: string) => Promise<void> = vi.fn(async () => {}),
  onDismissFailed: (queueItemId: string) => void = vi.fn()
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <SharedMessageQueueDisplay
        queue={queue}
        onCancel={onCancel}
        onDismissFailed={onDismissFailed}
      />
    </NextIntlClientProvider>
  )
}

function initialSharedConnection(
  queue: SharedQueuedPrompt[] = [],
  lastAppliedSeq = 0
): Map<string, ConnectionState> {
  let state = __connectionsReducerForTests(new Map(), {
    type: "CONNECTION_CREATED",
    contextKey: "tab-1",
    connectionId: "conn-1",
    agentType: "codex",
    workingDir: null,
    sharedSession: {
      generation: 1,
      leaseId: "lease-1",
      leaseExpiresAt: "2026-08-16T00:05:00.000Z",
      connectRequestId: "request-1",
      phase: { phase: "ready" },
      queue,
      activeTurn: null,
    },
  })
  if (lastAppliedSeq > 0) {
    state = __connectionsReducerForTests(state, {
      type: "EVENT_APPLIED",
      contextKey: "tab-1",
      seq: lastAppliedSeq,
    })
  }
  return state
}

function snapshotPatch(
  eventSeq: number,
  activeTurn: SharedActiveTurn | null = null
): SnapshotPatch {
  return {
    connectionId: "conn-1",
    conversationId: null,
    status: "connected",
    sessionId: null,
    modes: null,
    configOptions: null,
    availableCommands: null,
    usage: null,
    liveMessage: null,
    pendingPermission: null,
    pendingAskQuestion: null,
    pendingPlanApproval: null,
    pendingUserMessage: null,
    promptCapabilities: {
      image: false,
      audio: false,
      embedded_context: false,
    },
    selectorsReady: true,
    supportsFork: false,
    configStale: false,
    configStaleKind: null,
    backgroundOutstanding: 0,
    backgroundDetailRevision: 0,
    backgroundTranscriptGeneration: 0,
    sessionFailures: [],
    lastError: null,
    lastErrorDetails: null,
    eventSeq,
    activeDelegations: [],
    delegationRoute: null,
    waitingForSubagents: null,
    toolWatchdogProjections: {},
    toolWatchdogMaxVersions: {},
    lastToolWatchdogDiagnostic: null,
    sharedSession: {
      generation: 1,
      phase: { phase: "ready" },
      queue: [],
      activeTurn,
      leaseExpiresAt: null,
    },
  }
}

function reduceSharedEvent(
  state: Map<string, ConnectionState>,
  event: SharedSessionEvent
): Map<string, ConnectionState> {
  const reduced = __connectionsReducerForTests(state, {
    type: "SHARED_SESSION_EVENT",
    contextKey: "tab-1",
    event,
  })
  return __connectionsReducerForTests(reduced, {
    type: "EVENT_APPLIED",
    contextKey: "tab-1",
    seq: event.seq,
  })
}

function failedLifecycleState(): Map<string, ConnectionState> {
  let state = initialSharedConnection()
  state = reduceSharedEvent(state, queuedEvent())
  state = reduceSharedEvent(state, dispatchStartedEvent())
  state = reduceSharedEvent(state, failedEvent())
  return reduceSharedEvent(state, settledEvent("failed"))
}

function settledOnlyFailedLifecycleState(): Map<string, ConnectionState> {
  let state = initialSharedConnection()
  state = reduceSharedEvent(state, queuedEvent())
  state = reduceSharedEvent(state, dispatchStartedEvent())
  return reduceSharedEvent(state, settledEvent("failed", 3))
}

function hydrateEmptyQueue(
  state: Map<string, ConnectionState>,
  eventSeq: number
): Map<string, ConnectionState> {
  return __connectionsReducerForTests(state, {
    type: "HYDRATE_FROM_SNAPSHOT",
    contextKey: "tab-1",
    patch: snapshotPatch(eventSeq),
  })
}

function queuedEvent(seq = 1): SharedSessionEvent {
  return {
    seq,
    type: "prompt_queued",
    generation: 1,
    item: {
      queue_item_id: "q2",
      enqueue_seq: 2,
      client_message_id: "m-q2",
      visible_text: "do not lose me",
      visible_text_truncated: false,
      attachment_count: 0,
      submitted_at: "2026-08-16T00:00:00.000Z",
      state: "queued",
    },
  }
}

function dispatchStartedEvent(seq = 2): SharedSessionEvent {
  return {
    seq,
    type: "prompt_dispatch_started",
    generation: 1,
    turn: {
      turn_id: "turn-q2",
      queue_item_id: "q2",
      enqueue_seq: 2,
      client_message_id: "m-q2",
      stop_requested: false,
    },
  }
}

function failedEvent(seq = 3): SharedSessionEvent {
  return {
    seq,
    type: "prompt_queue_item_failed",
    generation: 1,
    queue_item_id: "q2",
    error_code: "prompt_hydration_failed",
  }
}

function settledEvent(
  outcome: "completed" | "cancelled" | "failed",
  seq = 4
): SharedSessionEvent {
  return {
    seq,
    type: "shared_turn_settled",
    generation: 1,
    turn_id: "turn-q2",
    outcome,
  }
}

describe("SharedMessageQueueDisplay", () => {
  afterEach(cleanup)

  it("renders authoritative rows by enqueue sequence without edit or reorder controls", () => {
    const { container } = renderQueue([
      queued("q3", 3, "third", "dispatching"),
      queued("q1", 1, "first"),
      queued("q2", 2, null, "queued", 2),
    ])

    const list = screen.getByTestId("shared-message-queue")
    expect(list.textContent).toMatch(/#1.*first.*#2.*2.*#3.*third/)
    expect(container.querySelector(".lucide-paperclip")).not.toBeNull()
    expect(container.querySelector(".lucide-grip-vertical")).toBeNull()
    expect(container.querySelector(".lucide-pencil")).toBeNull()
    expect(screen.getAllByRole("button")).toHaveLength(2)
  })

  it("cancels by queue item id and suppresses duplicate clicks while pending", async () => {
    const pending = deferred()
    const onCancel = vi.fn(() => pending.promise)
    renderQueue([queued("q2", 2, "later")], onCancel)

    const cancel = screen.getByRole("button", { name: "Remove" })
    fireEvent.click(cancel)
    fireEvent.click(cancel)

    expect(onCancel).toHaveBeenCalledTimes(1)
    expect(onCancel).toHaveBeenCalledWith("q2")
    expect(cancel).toBeDisabled()

    await act(async () => pending.resolve())
  })

  it("re-enables a queued row when cancellation fails without removing it", async () => {
    const pending = deferred()
    const onCancel = vi.fn(() => pending.promise)
    renderQueue([queued("q2", 2, "keep me")], onCancel)

    const cancel = screen.getByRole("button", { name: "Remove" })
    fireEvent.click(cancel)
    await act(async () => pending.reject(new Error("cancel failed")))

    expect(cancel).not.toBeDisabled()
    expect(screen.getByText("keep me")).toBeInTheDocument()
  })

  it("keeps a reducer-failed prompt visible across hydrate with a local dismiss action", () => {
    const initialQueue = [queued("q2", 2, "do not lose me")]
    const initial = initialSharedConnection(initialQueue, 7)
    const reduced = __connectionsReducerForTests(initial, {
      type: "SHARED_SESSION_EVENT",
      contextKey: "tab-1",
      event: {
        seq: 7,
        type: "prompt_queue_item_failed",
        generation: 1,
        queue_item_id: "q2",
        error_code: "prompt_hydration_failed",
      },
    })
    const hydrated = __connectionsReducerForTests(reduced, {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "tab-1",
      patch: snapshotPatch(7),
    })
    const queue = hydrated.get("tab-1")?.sharedSession?.queue ?? []
    const onCancel = vi.fn(async () => {})
    const onDismissFailed = vi.fn()

    renderQueue(queue, onCancel, onDismissFailed)

    expect(screen.getByText("do not lose me")).toBeInTheDocument()
    expect(screen.getByText("prompt_hydration_failed")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "Remove" }))
    expect(onDismissFailed).toHaveBeenCalledWith("q2")
    expect(onCancel).not.toHaveBeenCalled()
  })

  it("keeps prompt evidence through queued, dispatch, failed, and failed-settled events", () => {
    const state = failedLifecycleState()

    const shared = state.get("tab-1")?.sharedSession
    expect(shared?.activeTurn).toBeNull()
    expect(shared?.queue).toEqual([
      expect.objectContaining({
        queueItemId: "q2",
        visibleText: "do not lose me",
        state: "failed",
        errorCode: "prompt_hydration_failed",
        failureEventSeq: 4,
      }),
    ])

    renderQueue(shared?.queue ?? [])
    expect(screen.getByText("do not lose me")).toBeInTheDocument()
    expect(screen.getByText("prompt_hydration_failed")).toBeInTheDocument()
  })

  it("synthesizes a failed row when dispatch settles failed without a queue failure event", () => {
    const shared = settledOnlyFailedLifecycleState().get("tab-1")?.sharedSession

    expect(shared?.activeTurn).toBeNull()
    expect(shared?.queue).toEqual([
      expect.objectContaining({
        queueItemId: "q2",
        visibleText: "do not lose me",
        state: "failed",
        errorCode: "shared_turn_failed",
        failureEventSeq: 3,
      }),
    ])
  })

  it("bounds and locally dismisses a failed row synthesized by settlement", () => {
    const failed = settledOnlyFailedLifecycleState()

    expect(
      hydrateEmptyQueue(failed, 2).get("tab-1")?.sharedSession?.queue
    ).toHaveLength(1)
    expect(
      hydrateEmptyQueue(failed, 3).get("tab-1")?.sharedSession?.queue
    ).toHaveLength(1)
    expect(
      hydrateEmptyQueue(failed, 4).get("tab-1")?.sharedSession?.queue
    ).toEqual([])

    const dismissed = __connectionsReducerForTests(failed, {
      type: "DISMISS_FAILED_SHARED_PROMPT",
      contextKey: "tab-1",
      queueItemId: "q2",
    })
    expect(dismissed.get("tab-1")?.sharedSession?.queue).toEqual([])
  })

  it.each(["completed", "cancelled"] as const)(
    "clears dispatch evidence on a %s settlement",
    (outcome) => {
      let state = initialSharedConnection()
      state = reduceSharedEvent(state, queuedEvent())
      state = reduceSharedEvent(state, dispatchStartedEvent())
      state = reduceSharedEvent(state, settledEvent(outcome, 3))
      state = reduceSharedEvent(state, failedEvent(4))

      expect(state.get("tab-1")?.sharedSession).toMatchObject({
        queue: [],
        activeTurn: null,
      })
    }
  )

  it("bounds failed-row retention to older or equal snapshots", () => {
    const failed = failedLifecycleState()

    const older = hydrateEmptyQueue(failed, 3)
    expect(older.get("tab-1")?.sharedSession?.queue).toHaveLength(1)

    const equal = hydrateEmptyQueue(failed, 4)
    expect(equal.get("tab-1")?.sharedSession?.queue).toHaveLength(1)

    const newer = hydrateEmptyQueue(failed, 5)
    expect(newer.get("tab-1")?.sharedSession?.queue).toEqual([])
  })

  it("keeps dispatch evidence through an equal active-turn snapshot", () => {
    let state = initialSharedConnection()
    state = reduceSharedEvent(state, queuedEvent())
    state = reduceSharedEvent(state, dispatchStartedEvent())
    state = __connectionsReducerForTests(state, {
      type: "HYDRATE_FROM_SNAPSHOT",
      contextKey: "tab-1",
      patch: snapshotPatch(2, {
        turnId: "turn-q2",
        queueItemId: "q2",
        enqueueSeq: 2,
        clientMessageId: "m-q2",
        stopRequested: false,
      }),
    })
    state = reduceSharedEvent(state, failedEvent())
    state = reduceSharedEvent(state, settledEvent("failed"))

    expect(state.get("tab-1")?.sharedSession?.queue).toEqual([
      expect.objectContaining({
        queueItemId: "q2",
        visibleText: "do not lose me",
        state: "failed",
      }),
    ])
  })

  it("locally dismisses failed rows without calling backend cancel", () => {
    const failedQueue =
      failedLifecycleState().get("tab-1")?.sharedSession?.queue
    const onCancel = vi.fn(async () => {})
    const onDismissFailed = vi.fn()
    renderQueue(failedQueue ?? [], onCancel, onDismissFailed)

    fireEvent.click(screen.getByRole("button", { name: "Remove" }))

    expect(onDismissFailed).toHaveBeenCalledWith("q2")
    expect(onCancel).not.toHaveBeenCalled()
  })

  it("removes a failed row through the local reducer action", () => {
    const dismissed = __connectionsReducerForTests(failedLifecycleState(), {
      type: "DISMISS_FAILED_SHARED_PROMPT",
      contextKey: "tab-1",
      queueItemId: "q2",
    })

    expect(dismissed.get("tab-1")?.sharedSession?.queue).toEqual([])
  })
})
