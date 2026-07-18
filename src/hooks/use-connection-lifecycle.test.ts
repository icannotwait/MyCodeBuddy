import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const h = vi.hoisted(() => ({
  sendPrompt: vi.fn(async () => undefined),
  setMode: vi.fn(async () => undefined),
  locale: "zh_cn" as string,
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

vi.mock("@/contexts/acp-connections-context", () => ({
  useAcpActions: () => ({
    setActiveKey: vi.fn(),
    touchActivity: vi.fn(),
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
    status: "prompting",
    isViewer: false,
    backgroundOutstanding: 0,
    selectorsReady: true,
    connect: () => Promise.resolve(),
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

describe("handle_send_forwards_display_text_and_effective_locale", () => {
  beforeEach(() => {
    h.sendPrompt.mockClear()
    h.setMode.mockClear()
    h.locale = "zh_cn"
  })

  it("forwards displayText and effective locale as promptContext", async () => {
    const { result } = renderHook(() =>
      useConnectionLifecycle({
        contextKey: "tab-1",
        agentType: "claude_code",
        isActive: true,
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
})
