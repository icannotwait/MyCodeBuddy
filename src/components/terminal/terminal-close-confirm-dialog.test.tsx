import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import type { PendingTerminalClose } from "@/contexts/terminal-close-guard"
import { TerminalCloseConfirmDialog } from "./terminal-close-confirm-dialog"

function renderDialog(
  pending: PendingTerminalClose | null,
  onConfirm = vi.fn(),
  onCancel = vi.fn()
) {
  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TerminalCloseConfirmDialog
        pending={pending}
        onConfirm={onConfirm}
        onCancel={onCancel}
      />
    </NextIntlClientProvider>
  )
  return { onConfirm, onCancel }
}

describe("TerminalCloseConfirmDialog", () => {
  it("renders nothing when pending is null", () => {
    renderDialog(null)
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument()
  })

  it("shows the tab title in the single-close copy", () => {
    renderDialog({ kind: "one", tabId: "a", title: "Terminal 1" })
    expect(screen.getByRole("alertdialog")).toBeInTheDocument()
    expect(screen.getByText(/Terminal 1/)).toBeInTheDocument()
  })

  it("shows the live count in the multi-close copy", () => {
    renderDialog({
      kind: "all",
      liveCount: 3,
      targetIds: ["a", "b", "c"],
    })
    expect(screen.getByText(/3 terminals/)).toBeInTheDocument()
  })

  it("confirm and cancel fire their callbacks", () => {
    const { onConfirm, onCancel } = renderDialog({
      kind: "one",
      tabId: "a",
      title: "Terminal 1",
    })
    fireEvent.click(screen.getByRole("button", { name: "Kill & close" }))
    expect(onConfirm).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }))
    expect(onCancel).toHaveBeenCalledTimes(1)
  })
})
