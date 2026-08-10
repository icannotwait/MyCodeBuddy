import { beforeEach, expect, it, vi } from "vitest"

const toastMock = vi.hoisted(() => ({ error: vi.fn() }))
const relaunchAppMock = vi.hoisted(() => vi.fn<() => Promise<void>>())

vi.mock("sonner", () => ({ toast: toastMock }))
vi.mock("@/lib/updater", () => ({ relaunchApp: relaunchAppMock }))

import { PopOutRuntimeRestartRequiredError } from "@/lib/conversation-popout"
import { notifyConversationPopoutFailure } from "@/lib/conversation-popout-notifications"

const messages = {
  popupBlocked: "Popup blocked",
  handoffFailed: "Handoff failed",
  runtimeRestartRequired: "Restart required",
  restartAction: "Restart DrawCode",
  restartFailed: "Restart failed",
}

function recordedAction(): () => Promise<void> {
  const call = toastMock.error.mock.calls[toastMock.error.mock.calls.length - 1]
  const options = call?.[1] as
    | { action: { onClick: () => Promise<void> } }
    | undefined
  if (!options) throw new Error("restart toast action was not recorded")
  return options.action.onClick
}

beforeEach(() => {
  toastMock.error.mockReset()
  relaunchAppMock.mockReset()
})

it("uses one persistent fixed-id toast and does not relaunch automatically", () => {
  const error = new PopOutRuntimeRestartRequiredError(new Error("drift"))
  notifyConversationPopoutFailure(error, messages)
  notifyConversationPopoutFailure(error, messages)

  expect(relaunchAppMock).not.toHaveBeenCalled()
  for (const call of toastMock.error.mock.calls) {
    expect(call).toEqual([
      "Restart required",
      expect.objectContaining({
        id: "conversation-popout-runtime-restart-required",
        duration: Infinity,
        action: expect.objectContaining({ label: "Restart DrawCode" }),
      }),
    ])
  }
})

it("calls plain relaunch only from the action", async () => {
  relaunchAppMock.mockResolvedValueOnce(undefined)
  notifyConversationPopoutFailure(
    new PopOutRuntimeRestartRequiredError(new Error("drift")),
    messages
  )

  await recordedAction()()

  expect(relaunchAppMock).toHaveBeenCalledOnce()
})

it("catches relaunch rejection inside the action and reports failure", async () => {
  const consoleError = vi.spyOn(console, "error").mockImplementation(() => {})
  relaunchAppMock.mockRejectedValueOnce(new Error("relaunch denied"))
  notifyConversationPopoutFailure(
    new PopOutRuntimeRestartRequiredError(new Error("drift")),
    messages
  )

  await expect(recordedAction()()).resolves.toBeUndefined()
  expect(consoleError).toHaveBeenCalledWith(
    "[ConversationPopout] relaunch failed",
    expect.any(Error)
  )
  expect(toastMock.error).toHaveBeenLastCalledWith("Restart failed")
  consoleError.mockRestore()
})

it("keeps popup-blocked and generic failures on their existing copy", () => {
  notifyConversationPopoutFailure({ code: "popup_blocked" }, messages)
  notifyConversationPopoutFailure(new Error("desktop handoff failed"), messages)

  expect(toastMock.error.mock.calls).toEqual([
    ["Popup blocked"],
    ["Handoff failed"],
  ])
  expect(relaunchAppMock).not.toHaveBeenCalled()
})
