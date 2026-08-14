/**
 * Stopping a task records WHY, and the reason is optional — the confirm must
 * never wait on the textarea, and a box holding only whitespace must not write
 * an empty note onto the timeline.
 */
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import type { WorkTask } from "@/lib/types"

const cancelMock = vi.fn().mockResolvedValue(undefined)

vi.mock("@/lib/api", () => ({
  workTaskCancel: (...args: unknown[]) => cancelMock(...args),
}))

import { TaskCancelDialog } from "./task-cancel-dialog"

function task(): WorkTask {
  return {
    id: 7,
    folder_id: 1,
    title: "Fix login",
    config: null,
    status: "running",
  } as WorkTask
}

function renderDialog() {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <TaskCancelDialog open onOpenChange={() => {}} task={task()} />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  cancelMock.mockClear()
})

describe("TaskCancelDialog", () => {
  it("sends the trimmed reason", async () => {
    renderDialog()
    await userEvent.type(screen.getByLabelText(/Reason/), "  wrong approach  ")
    await userEvent.click(screen.getByRole("button", { name: "Cancel task" }))
    await waitFor(() =>
      expect(cancelMock).toHaveBeenCalledWith(7, "wrong approach")
    )
  })

  it("cancels with a null reason when nothing was typed", async () => {
    renderDialog()
    // No reason is still a legitimate stop — the confirm is never blocked.
    await userEvent.click(screen.getByRole("button", { name: "Cancel task" }))
    await waitFor(() => expect(cancelMock).toHaveBeenCalledWith(7, null))
  })

  it("treats a whitespace-only reason as none", async () => {
    renderDialog()
    await userEvent.type(screen.getByLabelText(/Reason/), "   ")
    await userEvent.click(screen.getByRole("button", { name: "Cancel task" }))
    await waitFor(() => expect(cancelMock).toHaveBeenCalledWith(7, null))
  })
})
