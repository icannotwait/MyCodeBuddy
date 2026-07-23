import { describe, expect, it } from "vitest"
import type { ToolWatchdogProjection } from "@/lib/types"
import {
  diagnosticFieldKeys,
  isNewerDiagnostic,
  pickLatestToolWatchdogDiagnostic,
  toSecretSafeDiagnostic,
} from "./tool-watchdog-diagnostic"

function proj(
  overrides: Partial<ToolWatchdogProjection> = {}
): ToolWatchdogProjection {
  return {
    lease_id: "lease-secret-should-not-leak-to-keys",
    version: 3,
    tool_title: "terminal",
    phase: "timed_out",
    last_progress_at: "2026-07-22T12:00:00Z",
    transition_at: "2026-07-22T12:20:00Z",
    grace_deadline: "2026-07-22T12:20:00Z",
    cancellation_scope: "terminal",
    error_code: "tool_stalled_timeout",
    ...overrides,
  }
}

describe("toSecretSafeDiagnostic", () => {
  it("exposes only transition, timestamp, safe title, scope, reason code", () => {
    const d = toSecretSafeDiagnostic(proj())
    expect(d).toEqual({
      phase: "timed_out",
      tool_title: "terminal",
      timestamp: "2026-07-22T12:20:00Z",
      cancellation_scope: "terminal",
      error_code: "tool_stalled_timeout",
    })
    const keys = diagnosticFieldKeys(d)
    expect(keys.sort()).toEqual(
      [
        "cancellation_scope",
        "error_code",
        "phase",
        "timestamp",
        "tool_title",
      ].sort()
    )
    // No lease id / version / grace_deadline / raw input fields.
    expect(JSON.stringify(d)).not.toContain("lease")
    expect(JSON.stringify(d)).not.toContain("command")
    expect(JSON.stringify(d)).not.toContain("password")
  })

  it("uses transition_at rather than last_progress_at for the UI timestamp", () => {
    const d = toSecretSafeDiagnostic(
      proj({
        last_progress_at: "2026-07-22T12:00:00Z",
        transition_at: "2026-07-22T12:30:00Z",
        phase: "grace",
      })
    )
    expect(d.timestamp).toBe("2026-07-22T12:30:00Z")
  })
})

describe("pickLatestToolWatchdogDiagnostic", () => {
  it("prefers later transition_at over higher per-lease version", () => {
    // Older lease extended many times → high version, earlier wall time.
    const olderHighVersion = proj({
      lease_id: "a",
      version: 9,
      phase: "grace",
      transition_at: "2026-07-22T12:05:00Z",
      last_progress_at: "2026-07-22T12:00:00Z",
    })
    // Newer lease first warning → version 1, later wall time.
    const newerLowVersion = proj({
      lease_id: "b",
      version: 1,
      phase: "warning",
      transition_at: "2026-07-22T12:15:00Z",
      last_progress_at: "2026-07-22T12:15:00Z",
    })
    const d = pickLatestToolWatchdogDiagnostic(
      {
        a: olderHighVersion,
        b: newerLowVersion,
      },
      null
    )
    expect(d?.phase).toBe("warning")
    expect(d?.timestamp).toBe("2026-07-22T12:15:00Z")
  })

  it("prefers last terminal timed_out when it is the latest transition", () => {
    const d = pickLatestToolWatchdogDiagnostic(
      {
        a: proj({
          lease_id: "a",
          version: 5,
          phase: "grace",
          transition_at: "2026-07-22T12:10:00Z",
        }),
      },
      proj({
        version: 2,
        phase: "timed_out",
        transition_at: "2026-07-22T12:25:00Z",
        error_code: "tool_stalled_timeout",
      })
    )
    expect(d?.phase).toBe("timed_out")
    expect(d?.error_code).toBe("tool_stalled_timeout")
    expect(d?.timestamp).toBe("2026-07-22T12:25:00Z")
  })

  it("uses last terminal when live map is empty (post-timeout reattach)", () => {
    const d = pickLatestToolWatchdogDiagnostic(
      {},
      proj({
        phase: "timed_out",
        error_code: "user_cancelled",
        transition_at: "2026-07-22T12:40:00Z",
      })
    )
    expect(d?.error_code).toBe("user_cancelled")
    expect(d?.phase).toBe("timed_out")
    expect(d?.timestamp).toBe("2026-07-22T12:40:00Z")
  })

  it("returns null when nothing is known", () => {
    expect(pickLatestToolWatchdogDiagnostic(undefined, null)).toBeNull()
  })
})

describe("isNewerDiagnostic", () => {
  it("orders by transition wall time across leases", () => {
    const earlier = proj({ version: 99, transition_at: "2026-07-22T12:00:00Z" })
    const later = proj({ version: 1, transition_at: "2026-07-22T12:01:00Z" })
    expect(isNewerDiagnostic(later, earlier)).toBe(true)
    expect(isNewerDiagnostic(earlier, later)).toBe(false)
  })
})
