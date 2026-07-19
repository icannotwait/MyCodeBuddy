import { act, render, screen, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"

import {
  DelegationProvider,
  useDelegation,
} from "@/contexts/delegation-context"
import {
  emptyRuntimeStats,
  type EventEnvelope,
} from "@/lib/types"

const STARTED_AT = "2026-07-19T00:00:00.000Z"

/** Honest wire-shaped `delegation_started` for provider tests. */
function startedEvent(
  overrides: Partial<{
    parent_connection_id: string
    parent_tool_use_id: string
    child_connection_id: string
    child_conversation_id: number
    agent_type: "codex"
    task_id: string
    started_at: string
    observation: "active" | "waiting_input" | "stalled"
    last_agent_activity_at: string | null
    stalled_since: string | null
  }> = {}
): EventEnvelope {
  return {
    seq: 1,
    connection_id: "p1",
    type: "delegation_started",
    parent_connection_id: "p1",
    parent_tool_use_id: "pt-1",
    child_connection_id: "c1",
    child_conversation_id: 99,
    agent_type: "codex",
    task_id: "task-1",
    started_at: STARTED_AT,
    runtime_stats: emptyRuntimeStats(STARTED_AT),
    ...overrides,
  }
}

/** Honest wire-shaped `delegation_completed` for provider tests. */
function completedEvent(
  result:
    | { kind: "ok"; duration_ms: number; text_preview?: string | null }
    | { kind: "err"; error_code: string },
  overrides: Partial<{
    parent_connection_id: string
    parent_tool_use_id: string
    child_connection_id: string
    child_conversation_id: number
    agent_type: "codex"
    task_id: string
  }> = {}
): EventEnvelope {
  return {
    seq: 2,
    connection_id: "p1",
    type: "delegation_completed",
    parent_connection_id: "p1",
    parent_tool_use_id: "pt-1",
    child_connection_id: "c1",
    child_conversation_id: 99,
    agent_type: "codex",
    task_id: "task-1",
    runtime_stats: emptyRuntimeStats(STARTED_AT),
    result,
    ...overrides,
  }
}

// Capture the envelope handler the provider registers via `useAcpEvent` so
// each test can drive the provider with synthetic acp://event envelopes.
// `useAcpEvent` runs during render, so the handler is captured synchronously
// on mount.
let capturedHandler: ((envelope: EventEnvelope) => void) | null = null

const mockAttach = vi.fn()
const mockDetach = vi.fn()

vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({
    attachDelegationChild: mockAttach,
    detachDelegationChild: mockDetach,
  }),
  useAcpEvent: (handler: (e: EventEnvelope) => void) => {
    capturedHandler = handler
  },
}))

/** Render-side probe that exposes the binding lookup as text so tests can
 *  read the binding state by `data-testid="status"` without depending on
 *  any UI component. */
function BindingProbe({ parentToolUseId }: { parentToolUseId: string }) {
  const { findByParentToolUseId } = useDelegation()
  const binding = findByParentToolUseId(parentToolUseId)
  if (!binding) return <div data-testid="status">none</div>
  return (
    <div>
      <div data-testid="status">{binding.status}</div>
      <div data-testid="error-code">{binding.errorCode ?? "-"}</div>
      <div data-testid="agent">{binding.agentType}</div>
      <div data-testid="task-id">{binding.taskId}</div>
      <div data-testid="started-at">{binding.startedAt}</div>
      <div data-testid="tool-count">
        {binding.runtimeStats.tool_call_count}
      </div>
      <div data-testid="duration-ms">
        {binding.completedDurationMs ?? "-"}
      </div>
      <div data-testid="attention">
        {binding.attentionRequest?.request_id ?? "-"}
      </div>
      <div data-testid="observation">{binding.observation ?? "-"}</div>
      <div data-testid="last-activity">
        {binding.lastAgentActivityAt ?? "-"}
      </div>
      <div data-testid="stalled-since">{binding.stalledSince ?? "-"}</div>
    </div>
  )
}

function renderProvider(children: ReactNode = null) {
  return render(
    <DelegationProvider>
      <BindingProbe parentToolUseId="pt-1" />
      {children}
    </DelegationProvider>
  )
}

/** Wait until the provider has registered its `useAcpEvent` handler. The
 *  capture is synchronous on mount, so this resolves on the first check; it
 *  stays a waitFor for resilience and must run with REAL timers. */
async function awaitHandlerCaptured() {
  await waitFor(() => expect(capturedHandler).not.toBeNull())
}

/** Drive a synthetic envelope through the provider's captured handler.
 *  Assumes `awaitHandlerCaptured` has already run. Works with fake
 *  timers because it's a synchronous dispatch. */
function dispatch(envelope: EventEnvelope) {
  if (!capturedHandler) {
    throw new Error(
      "capturedHandler not set — call awaitHandlerCaptured() with real timers first"
    )
  }
  act(() => {
    capturedHandler!(envelope)
  })
}

describe("DelegationProvider", () => {
  beforeEach(() => {
    // Fake timers are activated PER-TEST after the provider's async
    // subscribe-handler capture has resolved. Doing it in beforeEach
    // breaks waitFor (which polls via setTimeout) and stalls every test.
    capturedHandler = null
    mockAttach.mockReset()
    mockDetach.mockReset()
  })

  afterEach(() => {
    // Defensive: clear any test-local fake-timer install. Real timers
    // are the harness default; useRealTimers is a no-op if no fakes
    // are active.
    vi.useRealTimers()
  })

  it("flips binding to err when delegation_completed arrives with kind=err and schedules a detach", async () => {
    // Regression for the termination-cascade gap
    // (.docs/issues/2026-05-24-delegation-termination-cascade.md): every
    // broker terminal path now emits DelegationCompleted, so the context's
    // existing `delegation_completed` branch has to flip the binding to
    // err — not stay at "running" — and the detach grace timer has to
    // fire on err as well as ok.
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent())
    expect(screen.getByTestId("status")).toHaveTextContent("running")
    expect(mockAttach).toHaveBeenCalledTimes(1)

    // Install fake timers BEFORE the completed event so the setTimeout
    // scheduled by `cancelDetachTimer + setTimeout` registers as a fake
    // timer we can advance below.
    vi.useFakeTimers()
    dispatch(completedEvent({ kind: "err", error_code: "canceled" }))
    expect(screen.getByTestId("status")).toHaveTextContent("err")
    expect(screen.getByTestId("error-code")).toHaveTextContent("canceled")

    // Detach is delayed by CHILD_DETACH_GRACE_MS (2_000ms). Before the
    // timer fires the detach has been *scheduled* but not yet *called*.
    expect(mockDetach).not.toHaveBeenCalled()
    act(() => {
      vi.advanceTimersByTime(2_000)
    })
    expect(mockDetach).toHaveBeenCalledWith("c1")
  })

  it("flips binding to ok and detaches when delegation_completed arrives with kind=ok", async () => {
    // Cover the happy-path detach so the err and ok paths share coverage.
    // Previously only the ok branch was exercised end-to-end (via the
    // broker happy-path → lifecycle.forward_turn_complete_to_broker emit).
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent())

    vi.useFakeTimers()
    dispatch(completedEvent({ kind: "ok", duration_ms: 1234 }))
    expect(screen.getByTestId("status")).toHaveTextContent("ok")

    act(() => {
      vi.advanceTimersByTime(2_000)
    })
    expect(mockDetach).toHaveBeenCalledWith("c1")
  })

  it("synthesizes a minimal binding when delegation_completed arrives without a prior delegation_started", async () => {
    // Context-mount-after-start path (e.g. user switched tabs mid-delegation).
    // The completed event has to still update the binding so the parent UI
    // shows the terminal state instead of dropping the event silently.
    renderProvider()
    await awaitHandlerCaptured()

    dispatch(completedEvent({ kind: "err", error_code: "timeout" }))
    expect(screen.getByTestId("status")).toHaveTextContent("err")
    expect(screen.getByTestId("error-code")).toHaveTextContent("timeout")
    // Regression lock (Medium): with no prior delegation_started, the binding
    // must take the agent_type the completion now carries — not a hardcoded
    // default — so the card shows the correct agent icon/label.
    expect(screen.getByTestId("agent")).toHaveTextContent("codex")
  })

  it("cancels a pending detach when delegation_started replays for the same parent_tool_use_id", async () => {
    // Defensive: a reconnect / replay can re-emit delegation_started for
    // an entry currently mid-grace-period. The detach timer must be
    // canceled so the synthetic child state isn't torn down right as it
    // returns.
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent())
    vi.useFakeTimers()
    dispatch(completedEvent({ kind: "ok", duration_ms: 100 }))
    // Re-emit started before grace period expires
    dispatch(startedEvent())
    act(() => {
      vi.advanceTimersByTime(2_000)
    })
    // Detach was canceled by the re-arriving start event.
    expect(mockDetach).not.toHaveBeenCalled()
  })

  it.each([
    ["active", "2026-07-17T10:00:00Z", null],
    ["waiting_input", "2026-07-17T10:01:00Z", null],
    ["stalled", "2026-07-17T09:00:00Z", "2026-07-17T10:05:00Z"],
  ] as const)(
    "applies delegation_observation_changed to an existing running binding (%s) without terminal flip",
    async (observation, lastAt, stalledSince) => {
      renderProvider()
      await awaitHandlerCaptured()
      dispatch(startedEvent())
      expect(screen.getByTestId("status")).toHaveTextContent("running")
      expect(screen.getByTestId("observation")).toHaveTextContent("active")

      dispatch({
        seq: 3,
        connection_id: "p1",
        type: "delegation_observation_changed",
        parent_tool_use_id: "pt-1",
        task_id: "task-1",
        observation,
        last_agent_activity_at: lastAt,
        stalled_since: stalledSince,
      })

      // Lifecycle status stays running — observation is non-terminal health only.
      expect(screen.getByTestId("status")).toHaveTextContent("running")
      expect(screen.getByTestId("observation")).toHaveTextContent(observation)
      expect(screen.getByTestId("last-activity")).toHaveTextContent(lastAt)
      expect(screen.getByTestId("stalled-since")).toHaveTextContent(
        stalledSince ?? "-"
      )
      // Never attaches a second child or synthesizes completion.
      expect(mockAttach).toHaveBeenCalledTimes(1)
      expect(mockDetach).not.toHaveBeenCalled()
    }
  )

  it("does not synthesize a binding for observation on an unknown tool use", async () => {
    renderProvider()
    await awaitHandlerCaptured()
    dispatch({
      seq: 3,
      connection_id: "p1",
      type: "delegation_observation_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-missing",
      observation: "stalled",
      last_agent_activity_at: "2026-07-17T10:00:00Z",
      stalled_since: "2026-07-17T10:05:00Z",
    })
    expect(screen.getByTestId("status")).toHaveTextContent("none")
    expect(mockAttach).not.toHaveBeenCalled()
  })

  it("does not apply observation to a terminal binding", async () => {
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent())
    dispatch(completedEvent({ kind: "ok", duration_ms: 10 }))
    expect(screen.getByTestId("status")).toHaveTextContent("ok")

    dispatch({
      seq: 3,
      connection_id: "p1",
      type: "delegation_observation_changed",
      parent_tool_use_id: "pt-1",
      task_id: "task-1",
      observation: "stalled",
      last_agent_activity_at: "2026-07-17T10:00:00Z",
    })
    expect(screen.getByTestId("status")).toHaveTextContent("ok")
    expect(screen.getByTestId("observation")).toHaveTextContent("-")
  })

  it.each([
    ["waiting_input", "2026-07-17T11:00:00Z", null],
    ["stalled", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"],
  ] as const)(
    "seeds snapshot recovery observation=%s on started without hardcoding active",
    async (observation, lastAt, stalledSince) => {
      renderProvider()
      await awaitHandlerCaptured()
      dispatch(
        startedEvent({
          observation,
          last_agent_activity_at: lastAt,
          stalled_since: stalledSince,
        })
      )
      expect(screen.getByTestId("status")).toHaveTextContent("running")
      expect(screen.getByTestId("observation")).toHaveTextContent(observation)
      expect(screen.getByTestId("last-activity")).toHaveTextContent(lastAt)
      expect(screen.getByTestId("stalled-since")).toHaveTextContent(
        stalledSince ?? "-"
      )
    }
  )

  it("installs taskId and runtimeStats from delegation_started", async () => {
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(
      startedEvent({
        task_id: "task-xyz",
      })
    )
    expect(screen.getByTestId("status")).toHaveTextContent("running")
    expect(screen.getByTestId("task-id")).toHaveTextContent("task-xyz")
    expect(screen.getByTestId("started-at")).toHaveTextContent(STARTED_AT)
    expect(screen.getByTestId("tool-count")).toHaveTextContent("0")
  })

  it("does not schedule detach when completion task_id mismatches binding", async () => {
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent({ task_id: "task-1" }))
    expect(screen.getByTestId("status")).toHaveTextContent("running")

    vi.useFakeTimers()
    dispatch(
      completedEvent(
        { kind: "ok", duration_ms: 500 },
        { task_id: "stale-other-task" }
      )
    )
    // Binding stays running; detach must never fire.
    expect(screen.getByTestId("status")).toHaveTextContent("running")
    act(() => {
      vi.advanceTimersByTime(2_000)
    })
    expect(mockDetach).not.toHaveBeenCalled()
  })

  it("schedules detach only for matching completion task_id", async () => {
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent({ task_id: "task-1" }))

    vi.useFakeTimers()
    dispatch(
      completedEvent(
        { kind: "ok", duration_ms: 900 },
        {
          task_id: "task-1",
          // completedEvent already defaults task_id task-1
        }
      )
    )
    expect(screen.getByTestId("status")).toHaveTextContent("ok")
    expect(screen.getByTestId("duration-ms")).toHaveTextContent("900")
    expect(mockDetach).not.toHaveBeenCalled()
    act(() => {
      vi.advanceTimersByTime(2_000)
    })
    expect(mockDetach).toHaveBeenCalledWith("c1")
  })

  it("ignores mismatched observation without map or detach side effects", async () => {
    renderProvider()
    await awaitHandlerCaptured()
    dispatch(startedEvent({ task_id: "task-1" }))
    expect(screen.getByTestId("observation")).toHaveTextContent("active")

    dispatch({
      seq: 3,
      connection_id: "p1",
      type: "delegation_observation_changed",
      parent_tool_use_id: "pt-1",
      task_id: "not-task-1",
      observation: "stalled",
      last_agent_activity_at: "2026-07-17T10:00:00Z",
      stalled_since: "2026-07-17T10:05:00Z",
    })
    expect(screen.getByTestId("observation")).toHaveTextContent("active")
    expect(screen.getByTestId("status")).toHaveTextContent("running")
    expect(mockDetach).not.toHaveBeenCalled()
  })
})
