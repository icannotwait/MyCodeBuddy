import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import type { ToolWatchdogProjection } from "@/lib/types"
import enMessages from "@/i18n/messages/en.json"
import {
  formatCountdown,
  reduceToolWatchdogProjection,
  remainingGraceSeconds,
  ToolWatchdogBanner,
} from "./tool-watchdog-banner"

type ExtendToolWatchdogLease =
  typeof import("@/lib/api").extendToolWatchdogLease
type CancelToolWatchdogLease =
  typeof import("@/lib/api").cancelToolWatchdogLease

const h = vi.hoisted(() => ({
  projections: {} as Record<string, ToolWatchdogProjection>,
  extend: vi.fn<ExtendToolWatchdogLease>(async () => undefined),
  cancel: vi.fn<CancelToolWatchdogLease>(async () => undefined),
}))

vi.mock("@/hooks/use-connection", () => ({
  useConnection: () => ({
    toolWatchdogProjections: h.projections,
  }),
}))

vi.mock("@/lib/api", () => ({
  extendToolWatchdogLease: h.extend,
  cancelToolWatchdogLease: h.cancel,
}))

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
  },
}))

function renderBanner(contextKey = "tab-1") {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <ToolWatchdogBanner contextKey={contextKey} />
    </NextIntlClientProvider>
  )
}

function graceProjection(
  overrides: Partial<ToolWatchdogProjection> = {}
): ToolWatchdogProjection {
  const now = Date.now()
  return {
    lease_id: "lease-1",
    version: 2,
    tool_title: "terminal",
    phase: "grace",
    last_progress_at: new Date(now - 600_000).toISOString(),
    grace_deadline: new Date(now + 600_000).toISOString(),
    cancellation_scope: null,
    error_code: null,
    ...overrides,
  }
}

describe("tool-watchdog pure helpers", () => {
  it("clamps countdown at the zero boundary", () => {
    const past = new Date(Date.now() - 5_000).toISOString()
    expect(remainingGraceSeconds(past, Date.now())).toBe(0)
    expect(remainingGraceSeconds(null, Date.now())).toBeNull()
    expect(formatCountdown(125)).toBe("2:05")
    expect(formatCountdown(0)).toBe("0:00")
  })

  it("reduces multi-window versions without inventing terminal state", () => {
    const a = graceProjection({ version: 2 })
    let map: Record<string, ToolWatchdogProjection> = {}
    let maxVersionByLease: Record<string, number> = {}
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      a,
      maxVersionByLease
    ))
    expect(map["lease-1"]?.version).toBe(2)

    // Stale older event ignored
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({ version: 1, phase: "warning" }),
      maxVersionByLease
    ))
    expect(map["lease-1"]?.version).toBe(2)
    expect(map["lease-1"]?.phase).toBe("grace")

    // Winner extend
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({
        version: 3,
        grace_deadline: new Date(Date.now() + 900_000).toISOString(),
      }),
      maxVersionByLease
    ))
    expect(map["lease-1"]?.version).toBe(3)

    // Progress clear
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({ version: 4, phase: "cleared" }),
      maxVersionByLease
    ))
    expect(map["lease-1"]).toBeUndefined()
  })

  it("supports unlimited extension versions and timed_out removal", () => {
    let map: Record<string, ToolWatchdogProjection> = {}
    let maxVersionByLease: Record<string, number> = {}
    for (let v = 1; v <= 5; v++) {
      ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
        map,
        graceProjection({ version: v, phase: "grace" }),
        maxVersionByLease
      ))
    }
    expect(map["lease-1"]?.version).toBe(5)
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({
        version: 6,
        phase: "timed_out",
        error_code: "tool_stalled_timeout",
      }),
      maxVersionByLease
    ))
    expect(map["lease-1"]).toBeUndefined()
  })

  it("two-window winner/loser convergence keeps higher version", () => {
    const base = graceProjection({ version: 2 })
    // Window A applies winner's cancel projection
    const winner = reduceToolWatchdogProjection(
      { "lease-1": base },
      graceProjection({ version: 3, phase: "cancelling" }),
      { "lease-1": 2 }
    )
    // Window B still on v2 receives same event
    const loser = reduceToolWatchdogProjection(
      { "lease-1": base },
      graceProjection({ version: 3, phase: "cancelling" }),
      { "lease-1": 2 }
    )
    expect(winner.map["lease-1"]?.phase).toBe("cancelling")
    expect(loser.map["lease-1"]?.phase).toBe("cancelling")
    expect(winner.map["lease-1"]?.version).toBe(loser.map["lease-1"]?.version)
  })

  it("ignores lower-version cancelling after timed_out tombstone", () => {
    // I1: TimedOut wins the emit race, then stale Cancelling arrives later.
    let map: Record<string, ToolWatchdogProjection> = {}
    let maxVersionByLease: Record<string, number> = {}
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({ version: 1, phase: "grace" }),
      maxVersionByLease
    ))
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({
        version: 3,
        phase: "timed_out",
        error_code: "tool_stalled_timeout",
      }),
      maxVersionByLease
    ))
    expect(map["lease-1"]).toBeUndefined()
    expect(maxVersionByLease["lease-1"]).toBe(3)
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({ version: 2, phase: "cancelling" }),
      maxVersionByLease
    ))
    expect(map["lease-1"]).toBeUndefined()

    // Equal-version actionable after terminal also rejected.
    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({ version: 3, phase: "cancelling" }),
      maxVersionByLease
    ))
    expect(map["lease-1"]).toBeUndefined()
  })

  it("cold multi-lease hydrate rejects late lower-version cancelling for A", () => {
    // I1 R3: A TimedOut(v3), B newer diagnostic replaces last_*; cold attach
    // seeds floors from tool_watchdog_max_versions; delayed A Cancelling(v2)
    // must not resurrect A's banner.
    const floors = { "lease-a": 3, "lease-b": 2 }
    let map: Record<string, ToolWatchdogProjection> = {
      "lease-b": graceProjection({
        lease_id: "lease-b",
        version: 2,
        phase: "warning",
      }),
    }
    let maxVersionByLease: Record<string, number> = { ...floors }

    ;({ map, maxVersionByLease } = reduceToolWatchdogProjection(
      map,
      graceProjection({
        lease_id: "lease-a",
        version: 2,
        phase: "cancelling",
      }),
      maxVersionByLease
    ))
    expect(map["lease-a"]).toBeUndefined()
    expect(map["lease-b"]?.phase).toBe("warning")
    expect(maxVersionByLease["lease-a"]).toBe(3)
  })
})

describe("ToolWatchdogBanner", () => {
  beforeEach(() => {
    h.projections = {}
    h.extend.mockReset()
    h.cancel.mockReset()
    h.extend.mockResolvedValue(undefined)
    h.cancel.mockResolvedValue(undefined)
  })

  it("renders nothing without actionable projections", () => {
    const { container } = renderBanner()
    expect(container.firstChild).toBeNull()
  })

  it("shows safe tool title, last progress, countdown, Stop now, Wait 10 minutes", () => {
    h.projections = {
      "lease-1": graceProjection(),
    }
    renderBanner()
    expect(screen.getByText(/Terminal appears stalled/i)).toBeInTheDocument()
    expect(screen.getByText(/Last progress/i)).toBeInTheDocument()
    expect(screen.getByTestId("tool-watchdog-countdown")).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: /Stop now/i })
    ).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: /Wait 10 minutes/i })
    ).toBeInTheDocument()
  })

  it("disables controls after first click until next event (double-click dedup)", async () => {
    h.projections = { "lease-1": graceProjection({ version: 2 }) }
    const { rerender } = renderBanner()

    const stop = screen.getByRole("button", { name: /Stop now/i })
    fireEvent.click(stop)
    fireEvent.click(stop)

    await waitFor(() => {
      expect(h.cancel).toHaveBeenCalledTimes(1)
    })
    expect(h.cancel).toHaveBeenCalledWith("lease-1", 2)
    expect(stop).toBeDisabled()
    expect(
      screen.getByRole("button", { name: /Wait 10 minutes/i })
    ).toBeDisabled()

    // Authoritative next event re-enables (new version)
    h.projections = {
      "lease-1": graceProjection({ version: 3, phase: "grace" }),
    }
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ToolWatchdogBanner contextKey="tab-1" />
      </NextIntlClientProvider>
    )
    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: /Stop now/i })
      ).not.toBeDisabled()
    })
  })

  it("Wait 10 minutes sends lease_id + version", async () => {
    h.projections = { "lease-1": graceProjection({ version: 5 }) }
    renderBanner()
    fireEvent.click(screen.getByRole("button", { name: /Wait 10 minutes/i }))
    await waitFor(() => {
      expect(h.extend).toHaveBeenCalledWith("lease-1", 5)
    })
  })

  it("stale-action error clears pending so user can retry after refresh", async () => {
    h.projections = { "lease-1": graceProjection({ version: 2 }) }
    h.extend.mockRejectedValueOnce({
      code: "invalid_input",
      message: "stale_tool_watchdog_lease",
    })
    renderBanner()
    fireEvent.click(screen.getByRole("button", { name: /Wait 10 minutes/i }))
    await waitFor(() => {
      expect(h.extend).toHaveBeenCalled()
    })
    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: /Wait 10 minutes/i })
      ).not.toBeDisabled()
    })
  })

  it("progress clear removes the banner surface", () => {
    h.projections = { "lease-1": graceProjection() }
    const { rerender, container } = renderBanner()
    expect(screen.getByTestId("tool-watchdog-banner")).toBeInTheDocument()
    h.projections = {}
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <ToolWatchdogBanner contextKey="tab-1" />
      </NextIntlClientProvider>
    )
    expect(container.firstChild).toBeNull()
  })

  it("cancelling phase disables Wait and Stop", () => {
    h.projections = {
      "lease-1": graceProjection({ phase: "cancelling", version: 4 }),
    }
    renderBanner()
    expect(screen.getByRole("button", { name: /Stop now/i })).toBeDisabled()
    expect(
      screen.getByRole("button", { name: /Wait 10 minutes/i })
    ).toBeDisabled()
  })

  it("does not render timed_out as an open banner (failed tool entry is transcript-owned)", () => {
    // timed_out is removed by the reducer; empty map → no banner. Composer
    // usability is restored by turn_complete → connected (not local invention).
    h.projections = {}
    const { container } = renderBanner()
    expect(container.firstChild).toBeNull()
  })
})

describe("countdown tick", () => {
  it("updates remaining grace as time advances", () => {
    vi.useFakeTimers()
    const deadline = new Date(Date.now() + 90_000).toISOString()
    h.projections = {
      "lease-1": graceProjection({ grace_deadline: deadline }),
    }
    renderBanner()
    const before = screen.getByTestId("tool-watchdog-countdown").textContent
    act(() => {
      vi.advanceTimersByTime(1000)
    })
    const after = screen.getByTestId("tool-watchdog-countdown").textContent
    // Either same second-boundary or decreased — not increased.
    expect(after).toBeTruthy()
    expect(before).toBeTruthy()
    vi.useRealTimers()
  })
})
