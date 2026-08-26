/**
 * A failed `sendPrompt` must be user-visible.
 *
 * The composer clears itself synchronously on send (fire-and-forget), so a
 * rejection with no toast — e.g. the plain-text 413 axum's `DefaultBodyLimit`
 * returns for an oversized `/acp_prompt` body, or a `prompt_hydration`
 * failure for a deleted upload — used to look like the message simply
 * vanished. These tests pin the two contractual behaviors of the catch
 * branch in `useConnectionLifecycle.handleSend`:
 *
 * 1. `TurnBusyError` is NOT an error: it routes to `onTurnInProgress` (the
 *    caller re-queues the draft) with no toast.
 * 2. Everything else surfaces as a `sendPromptFailed` error toast, preferring
 *    the structured backend message when one is present.
 */

import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { TurnBusyError } from "@/lib/turn-busy"

const toastError = vi.fn()
const recordFrontendTurnTrace = vi.fn()

vi.mock("sonner", () => ({
  toast: {
    error: (...args: unknown[]) => toastError(...args),
    success: vi.fn(),
    warning: vi.fn(),
    message: vi.fn(),
  },
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string, params?: Record<string, unknown>) =>
    params ? `${key}:${JSON.stringify(params)}` : key,
}))

vi.mock("@/lib/acp/frontend-turn-trace", () => ({
  recordFrontendTurnTrace: (...args: unknown[]) =>
    recordFrontendTurnTrace(...args),
}))

const sendPrompt = vi.fn()
let sharedSession: { generation: number } | null = null

vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({ setActiveKey: vi.fn(), touchActivity: vi.fn() }),
}))

vi.mock("@/contexts/task-context", () => ({
  useTaskContext: () => ({
    addTask: vi.fn(),
    updateTask: vi.fn(),
    removeTask: vi.fn(),
  }),
}))

vi.mock("@/hooks/use-connection", () => ({
  useConnection: () => ({
    status: "connected",
    selectorsReady: true,
    connect: vi.fn().mockResolvedValue(undefined),
    disconnect: vi.fn().mockResolvedValue(undefined),
    sendPrompt,
    setMode: vi.fn().mockResolvedValue(undefined),
    setConfigOption: vi.fn().mockResolvedValue(undefined),
    cancel: vi.fn().mockResolvedValue(undefined),
    respondPermission: vi.fn().mockResolvedValue(undefined),
    modes: null,
    configOptions: null,
    hasCachedSelectors: true,
    isViewer: false,
    backgroundOutstanding: 0,
    sharedSession,
  }),
}))

import { useConnectionLifecycle } from "@/hooks/use-connection-lifecycle"

function draft() {
  return {
    blocks: [{ type: "text" as const, text: "hello" }],
    displayText: "hello",
  }
}

function renderLifecycle() {
  return renderHook(() =>
    useConnectionLifecycle({
      contextKey: "ctx-1",
      agentType: "claude_code",
      isActive: true,
    })
  )
}

async function flush() {
  await Promise.resolve()
  await Promise.resolve()
}

describe("useConnectionLifecycle send-failure surfacing", () => {
  beforeEach(() => {
    vi.clearAllMocks()
    sharedSession = null
  })

  it("records send start before the wire call and completion with elapsed time", async () => {
    const milestones: string[] = []
    recordFrontendTurnTrace.mockImplementation((trace: { phase: string }) =>
      milestones.push(trace.phase)
    )
    sendPrompt.mockImplementation(async () => {
      expect(milestones).toEqual(["send_started"])
      return undefined
    })
    const { result } = renderLifecycle()
    await act(async () => {
      await result.current.handleSend(draft(), null, {
        conversationId: 42,
        clientMessageId: "message-1",
      })
    })

    expect(milestones).toEqual(["send_started", "send_completed"])
    expect(recordFrontendTurnTrace).toHaveBeenLastCalledWith({
      phase: "send_completed",
      contextKey: "ctx-1",
      conversationId: 42,
      clientMessageId: "message-1",
      elapsedMs: expect.any(Number),
      outcome: "success",
    })
    expect(
      (recordFrontendTurnTrace.mock.calls.at(-1)?.[0] as { elapsedMs: number })
        .elapsedMs
    ).toBeGreaterThanOrEqual(0)
  })

  it("toasts a structured backend error with its message", async () => {
    // Shape produced by the web transport for a JSON `AppCommandError`
    // response — e.g. a prompt_hydration failure for a deleted upload.
    sendPrompt.mockRejectedValue({
      code: "task_execution_failed",
      message: "image attachment unavailable: /uploads/x.png",
    })
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})

    const { result } = renderLifecycle()
    await act(async () => {
      result.current.handleSend(draft())
      await flush()
    })

    expect(toastError).toHaveBeenCalledTimes(1)
    expect(String(toastError.mock.calls[0][0])).toContain(
      "image attachment unavailable: /uploads/x.png"
    )
    consoleError.mockRestore()
  })

  it("toasts the fallback shape the transport builds for a plain-text 413", async () => {
    // `web-transport` falls back to `{ message: "HTTP 413" }` when the
    // response body isn't JSON (axum's DefaultBodyLimit rejection).
    sendPrompt.mockRejectedValue({ code: "http_error", message: "HTTP 413" })
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})

    const { result } = renderLifecycle()
    await act(async () => {
      result.current.handleSend(draft())
      await flush()
    })

    expect(toastError).toHaveBeenCalledTimes(1)
    expect(String(toastError.mock.calls[0][0])).toContain("HTTP 413")
    consoleError.mockRestore()
  })

  it("toasts and invokes onSendFailed when the connection disappears before lookup", async () => {
    const error = Object.assign(new Error("connection not found: ctx-1"), {
      code: "connection_not_found",
    })
    sendPrompt.mockRejectedValue(error)
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})
    const calls: string[] = []
    toastError.mockImplementation(() => calls.push("toast"))
    const onSendFailed = vi.fn(() => calls.push("onSendFailed"))

    const { result } = renderLifecycle()
    await act(async () => {
      result.current.handleSend(draft(), null, { onSendFailed })
      await flush()
    })

    expect(onSendFailed).toHaveBeenCalledTimes(1)
    expect(onSendFailed).toHaveBeenCalledWith(error)
    // Toast first, then the state rollback — the rollback must not hide the
    // failure from the user.
    expect(calls).toEqual(["toast", "onSendFailed"])
    consoleError.mockRestore()
  })

  it("routes TurnBusyError to onTurnInProgress with no toast and no onSendFailed", async () => {
    sendPrompt.mockRejectedValue(new TurnBusyError())
    const onTurnInProgress = vi.fn()
    const onSendFailed = vi.fn()

    const { result } = renderLifecycle()
    await act(async () => {
      result.current.handleSend(draft(), null, {
        onTurnInProgress,
        onSendFailed,
      })
      await flush()
    })

    expect(onTurnInProgress).toHaveBeenCalledTimes(1)
    expect(onSendFailed).not.toHaveBeenCalled()
    expect(toastError).not.toHaveBeenCalled()
  })

  it("does not toast on a successful send", async () => {
    sendPrompt.mockResolvedValue(undefined)

    const { result } = renderLifecycle()
    await act(async () => {
      result.current.handleSend(draft())
      await flush()
    })

    expect(sendPrompt).toHaveBeenCalledTimes(1)
    expect(toastError).not.toHaveBeenCalled()
  })

  it("returns broker admission only after it resolves and rethrows shared failures", async () => {
    sharedSession = { generation: 1 }
    const admitted = {
      queueItemId: "queue-1",
      enqueueSeq: 3,
      state: "queued" as const,
    }
    sendPrompt.mockResolvedValue(admitted)
    const onPromptAdmitted = vi.fn()
    const { result } = renderLifecycle()

    await expect(
      result.current.handleSend(draft(), null, { onPromptAdmitted })
    ).resolves.toEqual(admitted)
    expect(onPromptAdmitted).toHaveBeenCalledWith(admitted)

    const failure = { code: "lease_expired", message: "lease expired" }
    sendPrompt.mockRejectedValue(failure)
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})
    await expect(result.current.handleSend(draft())).rejects.toBe(failure)
    consoleError.mockRestore()
  })
})
