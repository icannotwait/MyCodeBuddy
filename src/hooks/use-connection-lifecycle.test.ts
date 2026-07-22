import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { UseConnectionReturn } from "@/hooks/use-connection"

type ConnectFn = UseConnectionReturn["connect"]

const h = vi.hoisted(() => ({
  sendPrompt: vi.fn(async () => undefined),
  setMode: vi.fn(async () => undefined),
  // Zero-arg implementation is assignable to ConnectFn; Vitest still
  // records the real call arguments without unused-parameter warnings.
  connect: vi.fn<ConnectFn>(async () => undefined),
  touchActivity: vi.fn(),
  setActiveKey: vi.fn(),
  status: "prompting" as string | null,
  locale: "zh_cn" as string,
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({
    setActiveKey: h.setActiveKey,
    touchActivity: h.touchActivity,
  }),
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
    // Keep owner busy on unmount so cleanup skips disconnect (avoids ref churn).
    // Focus-retry tests override `h.status` to disconnected/error/null.
    status: h.status,
    isViewer: false,
    backgroundOutstanding: 0,
    selectorsReady: true,
    connect: h.connect,
    disconnect: () => Promise.resolve(),
    sendPrompt: h.sendPrompt,
    setMode: h.setMode,
    setConfigOption: () => Promise.resolve(),
    cancel: () => Promise.resolve(),
    respondPermission: () => Promise.resolve(),
    modes: null,
    configOptions: null,
    hasCachedSelectors: true,
  }),
}))

vi.mock("@/lib/i18n", () => ({
  getCurrentEffectiveAppLocale: () => h.locale,
}))

import {
  shouldDisconnectOnUnmount,
  useConnectionLifecycle,
} from "@/hooks/use-connection-lifecycle"

// Unmount cleanup (tab closed) must not kill an owner whose agent still has
// work in flight: disconnecting kills the agent CLI, and any launched
// background tasks with it. Busy owners are reclaimed by the idle sweeps —
// which exempt them only while outstanding > 0 — so once the work settles the
// connection becomes sweep-eligible again through the SAME predicate flipping
// to true on the next idle pass.
describe("shouldDisconnectOnUnmount", () => {
  it("keeps an owner alive while background work is outstanding", () => {
    expect(
      shouldDisconnectOnUnmount({
        status: "connected",
        isViewer: false,
        backgroundOutstanding: 2,
      })
    ).toBe(false)
  })

  it("keeps a prompting owner alive (existing behavior)", () => {
    expect(
      shouldDisconnectOnUnmount({
        status: "prompting",
        isViewer: false,
        backgroundOutstanding: 0,
      })
    ).toBe(false)
  })

  it("disconnects an idle owner once outstanding has settled to zero", () => {
    expect(
      shouldDisconnectOnUnmount({
        status: "connected",
        isViewer: false,
        backgroundOutstanding: 0,
      })
    ).toBe(true)
  })

  it("always tears down viewers — their disconnect only detaches", () => {
    expect(
      shouldDisconnectOnUnmount({
        status: "prompting",
        isViewer: true,
        backgroundOutstanding: 5,
      })
    ).toBe(true)
  })
})

describe("handleFocus_forwards_ownerOperationId", () => {
  beforeEach(() => {
    h.connect.mockClear()
    h.touchActivity.mockClear()
    h.setActiveKey.mockClear()
    h.status = "disconnected"
    h.locale = "zh_cn"
  })

  it("focus-retry connect includes ownerOperationId from the cold incarnation ref", async () => {
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "detached-tab",
        agentType: "claude_code",
        isActive: true,
        autoConnectAllowed: true,
        workingDir: "/tmp/project",
        sessionId: "sess-ext",
        conversationId: 42,
        ownerOperationId: "op-focus-retry",
      })
    )

    // Auto-connect also fires when active+workingDir; clear before focus.
    await waitFor(() => expect(h.connect).toHaveBeenCalled())
    h.connect.mockClear()

    act(() => {
      result.current.handleFocus()
    })

    await waitFor(() => expect(h.connect).toHaveBeenCalledTimes(1))
    expect(h.connect).toHaveBeenCalledWith(
      "claude_code",
      "/tmp/project",
      "sess-ext",
      42,
      undefined,
      "op-focus-retry"
    )
  })

  it("focus-retry still works when status is error and operation is set", async () => {
    h.status = "error"
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "detached-tab-err",
        agentType: "codex",
        isActive: true,
        autoConnectAllowed: true,
        workingDir: "/tmp/p",
        ownerOperationId: "op-err",
      })
    )
    await waitFor(() => expect(h.connect).toHaveBeenCalled())
    h.connect.mockClear()

    act(() => {
      result.current.handleFocus()
    })

    await waitFor(() => expect(h.connect).toHaveBeenCalledTimes(1))
    const args = h.connect.mock.calls[0]
    expect(args[0]).toBe("codex")
    expect(args[5]).toBe("op-err")
  })
})

describe("autoConnectAllowed_policy", () => {
  beforeEach(() => {
    h.connect.mockClear()
    h.touchActivity.mockClear()
    h.setActiveKey.mockClear()
    h.status = "disconnected"
    h.locale = "zh_cn"
  })

  it("omitted autoConnectAllowed retains legacy automatic connection", async () => {
    renderHook(() =>
      useConnectionLifecycle({
        contextKey: "legacy-tab",
        agentType: "codex",
        isActive: true,
        workingDir: "/tmp/project",
        sessionId: "s-legacy",
        conversationId: 7,
      })
    )
    await waitFor(() => expect(h.connect).toHaveBeenCalledTimes(1))
    expect(h.connect).toHaveBeenCalledWith(
      "codex",
      "/tmp/project",
      "s-legacy",
      7,
      undefined,
      undefined
    )
  })

  it("does not automatically connect or focus-retry when autoConnectAllowed is false", async () => {
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "terminal-tab",
        agentType: "codex",
        isActive: true,
        autoConnectAllowed: false,
        workingDir: "/tmp/project",
        sessionId: "s1",
        conversationId: 42,
      })
    )
    await act(async () => {})
    expect(h.connect).not.toHaveBeenCalled()
    // Mount/isActive effect may touch activity; clear before focus so we
    // assert exactly one post-focus activity update.
    h.touchActivity.mockClear()
    act(() => result.current.handleFocus())
    expect(h.connect).not.toHaveBeenCalled()
    expect(h.touchActivity).toHaveBeenCalledTimes(1)
    expect(h.touchActivity).toHaveBeenCalledWith("terminal-tab")
  })

  it("explicit reconnect preserves the stored session identity", async () => {
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "terminal-tab",
        agentType: "codex",
        isActive: true,
        autoConnectAllowed: false,
        workingDir: "/tmp/project",
        sessionId: "s1",
        conversationId: 42,
        ownerOperationId: "op-1",
      })
    )
    await result.current.handleReconnect()
    expect(h.connect).toHaveBeenCalledTimes(1)
    expect(h.connect).toHaveBeenCalledWith(
      "codex",
      "/tmp/project",
      "s1",
      42,
      undefined,
      "op-1"
    )
  })
})

describe("handle_send_forwards_display_text_and_effective_locale", () => {
  beforeEach(() => {
    h.sendPrompt.mockClear()
    h.setMode.mockClear()
    h.connect.mockClear()
    h.touchActivity.mockClear()
    h.setActiveKey.mockClear()
    h.status = "prompting"
    h.locale = "zh_cn"
  })

  it("forwards displayText and effective locale as promptContext", async () => {
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "tab-1",
        agentType: "claude_code",
        isActive: true,
        autoConnectAllowed: true,
      })
    )

    act(() => {
      result.current.handleSend(
        {
          blocks: [{ type: "text", text: "wire" }],
          displayText: "README.md task",
        },
        null,
        {
          folderId: 1,
          conversationId: 2,
          clientMessageId: "m1",
        }
      )
    })

    await waitFor(() => {
      expect(h.sendPrompt).toHaveBeenCalledWith(
        [{ type: "text", text: "wire" }],
        {
          folderId: 1,
          conversationId: 2,
          clientMessageId: "m1",
          promptContext: {
            visibleText: "README.md task",
            locale: "zh_cn",
          },
        }
      )
    })
  })

  it("does not reach prompt dispatch when mode change fails", async () => {
    h.setMode.mockRejectedValueOnce(new Error("mode failed"))
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "tab-1",
        agentType: "claude_code",
        isActive: true,
        autoConnectAllowed: true,
      })
    )
    act(() => {
      result.current.handleSend(
        { blocks: [{ type: "text", text: "wire" }], displayText: "wire" },
        "plan"
      )
    })
    await waitFor(() => expect(h.setMode).toHaveBeenCalledWith("plan"))
    expect(h.sendPrompt).not.toHaveBeenCalled()
  })

  it("invokes onTurnInProgress for TurnBusyError so callers can requeue", async () => {
    const { TurnBusyError } = await import("@/lib/turn-busy")
    h.sendPrompt.mockRejectedValueOnce(new TurnBusyError())
    const onTurnInProgress = vi.fn()
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "tab-1",
        agentType: "claude_code",
        isActive: true,
        autoConnectAllowed: true,
      })
    )

    act(() => {
      result.current.handleSend(
        { blocks: [{ type: "text", text: "wire" }], displayText: "wire" },
        null,
        { onTurnInProgress }
      )
    })

    await waitFor(() => expect(onTurnInProgress).toHaveBeenCalledTimes(1))
    expect(h.sendPrompt).toHaveBeenCalledTimes(1)
  })

  it("invokes only onContinuationWaiting for a continuation waiting rejection", async () => {
    const { ContinuationWaitingError } =
      await import("@/lib/continuation-waiting")
    h.sendPrompt.mockRejectedValueOnce(
      new ContinuationWaitingError(42, "arming")
    )
    const onTurnInProgress = vi.fn()
    const onContinuationWaiting = vi.fn()
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "tab-1",
        agentType: "claude_code",
        isActive: true,
        autoConnectAllowed: true,
      })
    )

    act(() => {
      result.current.handleSend(
        { blocks: [{ type: "text", text: "wire" }], displayText: "wire" },
        null,
        { onTurnInProgress, onContinuationWaiting }
      )
    })

    await waitFor(() => expect(onContinuationWaiting).toHaveBeenCalledTimes(1))
    expect(onTurnInProgress).not.toHaveBeenCalled()
  })
})
