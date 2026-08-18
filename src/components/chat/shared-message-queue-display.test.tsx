import { act, cleanup, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { SharedQueuedPrompt } from "@/lib/snapshot-denormalize"

import { SharedMessageQueueDisplay } from "./shared-message-queue-display"

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
  onCancel: (queueItemId: string) => Promise<void> = vi.fn(async () => {})
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <SharedMessageQueueDisplay queue={queue} onCancel={onCancel} />
    </NextIntlClientProvider>
  )
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
})
