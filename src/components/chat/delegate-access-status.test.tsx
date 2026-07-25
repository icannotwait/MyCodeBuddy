import type { ComponentProps } from "react"
import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import {
  DelegateAccessStatus,
  resolveDelegateAccessStatus,
} from "./delegate-access-status"

const taskRunning = {
  mode: "viewer_only" as const,
  reason: "task_running" as const,
  parent_id: 1,
}

describe("resolveDelegateAccessStatus", () => {
  it.each([
    [
      {
        access: taskRunning,
        loading: false,
        connectionId: null,
        syncError: null,
      },
      "waiting",
    ],
    [
      {
        access: taskRunning,
        loading: false,
        connectionId: "broker-child",
        syncError: null,
      },
      "observing",
    ],
    [
      {
        access: {
          mode: "viewer_only" as const,
          reason: "parent_turn_active" as const,
          parent_id: 1,
        },
        loading: false,
        connectionId: "owner-child",
        syncError: null,
      },
      "parent_turn_active",
    ],
    [
      {
        access: {
          mode: "viewer_only" as const,
          reason: "state_unknown" as const,
          parent_id: 1,
        },
        loading: false,
        connectionId: null,
        syncError: null,
      },
      "state_unknown",
    ],
    [
      {
        access: {
          mode: "interactive" as const,
          reason: null,
          parent_id: 1,
        },
        loading: false,
        connectionId: null,
        syncError: null,
      },
      "interactive",
    ],
    [
      {
        access: {
          mode: "interactive" as const,
          reason: null,
          parent_id: 1,
        },
        loading: false,
        connectionId: null,
        syncError: "flush failed",
      },
      "sync_failed",
    ],
    [
      {
        access: {
          mode: "interactive" as const,
          reason: null,
          parent_id: 1,
        },
        loading: false,
        connectionId: null,
        syncError: "",
      },
      "sync_failed",
    ],
  ])("resolves %j to %s", (args, expected) => {
    expect(resolveDelegateAccessStatus(args)).toBe(expected)
  })
})

function renderStatus(props: ComponentProps<typeof DelegateAccessStatus>) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <DelegateAccessStatus {...props} />
    </NextIntlClientProvider>
  )
}

describe("DelegateAccessStatus", () => {
  it("announces waiting and observing without calling the child disconnected", () => {
    const view = renderStatus({
      access: taskRunning,
      loading: false,
      connectionId: null,
      syncError: null,
    })
    expect(screen.getByRole("status")).toHaveTextContent(
      "Waiting for the delegated agent"
    )
    expect(screen.queryByText(/disconnected/i)).not.toBeInTheDocument()

    view.rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <DelegateAccessStatus
          access={taskRunning}
          loading={false}
          connectionId="broker-child"
          syncError={null}
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByRole("status")).toHaveTextContent(
      "Observing delegated task"
    )
  })

  it("gives sync failure alert precedence and retains the diagnostic as a title", () => {
    renderStatus({
      access: taskRunning,
      loading: false,
      connectionId: "broker-child",
      syncError: "transcript flush timed out",
    })
    const alert = screen.getByRole("alert")
    expect(alert).toHaveTextContent("Could not synchronize the final response")
    expect(alert).toHaveAttribute("title", "transcript flush timed out")
  })
})
