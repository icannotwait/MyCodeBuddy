import { cleanup, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import type { QueuedMessage } from "@/hooks/use-message-queue"

import { MessageQueueDisplay } from "./message-queue-display"

const QUEUE: QueuedMessage[] = [
  {
    id: "q1",
    draft: {
      displayText: "first queued",
      blocks: [{ type: "text", text: "first queued" }],
    },
    modeId: null,
  },
]

function renderQueue(
  props: Partial<React.ComponentProps<typeof MessageQueueDisplay>> = {}
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageQueueDisplay
        queue={QUEUE}
        onReorder={vi.fn()}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        editingItemId={null}
        {...props}
      />
    </NextIntlClientProvider>
  )
}

describe("MessageQueueDisplay terminal pause controls", () => {
  afterEach(() => {
    cleanup()
  })

  it("hides pause text and Resume queue when not paused", () => {
    renderQueue({ paused: false })

    // Exact English key values from i18n (`messageQueue.paused` / `resumeQueue`).
    const pausedLabel = "Queue paused"
    const resumeLabel = "Resume queue"

    expect(screen.queryByText(pausedLabel)).toBeNull()
    expect(screen.queryByRole("button", { name: resumeLabel })).toBeNull()
  })

  it("hides pause controls when queue is empty even if paused", () => {
    renderQueue({ queue: [], paused: true, onResumeQueue: vi.fn() })

    const pausedLabel = "Queue paused"
    const resumeLabel = "Resume queue"

    expect(screen.queryByText(pausedLabel)).toBeNull()
    expect(screen.queryByRole("button", { name: resumeLabel })).toBeNull()
  })

  it("shows pause text and accessible Resume queue button that fires once", () => {
    const onResumeQueue = vi.fn()
    const pausedLabel = "Queue paused"
    const resumeLabel = "Resume queue"

    renderQueue({ paused: true, onResumeQueue })

    expect(screen.getByText(pausedLabel)).toBeTruthy()

    const button = screen.getByRole("button", { name: resumeLabel })
    expect(button).toBeTruthy()
    expect(button.getAttribute("type")).toBe("button")

    fireEvent.click(button)
    expect(onResumeQueue).toHaveBeenCalledTimes(1)
  })
})
