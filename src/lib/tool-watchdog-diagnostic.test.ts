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

  it("orders same-second transitions by sub-second transition_at not grace", () => {
    const olderGrace = proj({
      lease_id: "old",
      version: 5,
      phase: "grace",
      transition_at: "2026-07-22T12:00:00.100Z",
      grace_deadline: "2026-07-22T12:10:00.000Z",
    })
    const newerWarning = proj({
      lease_id: "new",
      version: 1,
      phase: "warning",
      transition_at: "2026-07-22T12:00:00.900Z",
      grace_deadline: "2026-07-22T12:05:00.000Z",
    })
    expect(isNewerDiagnostic(newerWarning, olderGrace)).toBe(true)
    expect(isNewerDiagnostic(olderGrace, newerWarning)).toBe(false)
  })

  it("does not prefer later grace_deadline when transition_at second-truncated equal", () => {
    // Simulates two concurrent leases whose sub-second transitions collapsed
    // to the same wall second on the wire.
    const olderGrace = proj({
      lease_id: "old",
      version: 9,
      phase: "grace",
      transition_at: "2026-07-22T12:00:00Z",
      grace_deadline: "2026-07-22T12:10:00Z",
    })
    const newerWarning = proj({
      lease_id: "new",
      version: 1,
      phase: "warning",
      transition_at: "2026-07-22T12:00:00Z",
      grace_deadline: "2026-07-22T12:05:00Z",
    })
    expect(isNewerDiagnostic(newerWarning, olderGrace)).toBe(true)
  })
})

describe("pickLatestToolWatchdogDiagnostic same-second concurrent", () => {
  it("picks the later millis transition across concurrent live leases", () => {
    const d = pickLatestToolWatchdogDiagnostic(
      {
        old: proj({
          lease_id: "old",
          version: 5,
          phase: "grace",
          transition_at: "2026-07-22T12:00:00.100Z",
          grace_deadline: "2026-07-22T12:10:00.000Z",
        }),
        neu: proj({
          lease_id: "new",
          version: 1,
          phase: "warning",
          transition_at: "2026-07-22T12:00:00.900Z",
          grace_deadline: "2026-07-22T12:05:00.000Z",
        }),
      },
      null
    )
    expect(d?.phase).toBe("warning")
    expect(d?.timestamp).toBe("2026-07-22T12:00:00.900Z")
  })

  it("picks equal-millis transitions by transition_seq not map key order", () => {
    // Same scan stamp .000Z; lease-a applied later (higher seq). Map key order
    // would favor lease-z last under Object.values insertion if seq is ignored.
    const d = pickLatestToolWatchdogDiagnostic(
      {
        "lease-a": proj({
          lease_id: "lease-a",
          version: 1,
          phase: "warning",
          tool_title: "terminal",
          transition_at: "2026-07-22T12:00:00.000Z",
          transition_seq: 5,
        }),
        "lease-z": proj({
          lease_id: "lease-z",
          version: 1,
          phase: "grace",
          tool_title: "mcp",
          transition_at: "2026-07-22T12:00:00.000Z",
          transition_seq: 4,
        }),
      },
      null
    )
    expect(d?.phase).toBe("warning")
    expect(d?.tool_title).toBe("terminal")
    expect(d?.timestamp).toBe("2026-07-22T12:00:00.000Z")
  })

  it("retains higher-seq server diagnostic over equal-millis live map entry", () => {
    const retained = proj({
      lease_id: "lease-a",
      version: 2,
      phase: "timed_out",
      transition_at: "2026-07-22T12:00:00.000Z",
      transition_seq: 20,
      error_code: "tool_stalled_timeout",
    })
    const d = pickLatestToolWatchdogDiagnostic(
      {
        "lease-z": proj({
          lease_id: "lease-z",
          version: 1,
          phase: "warning",
          transition_at: "2026-07-22T12:00:00.000Z",
          transition_seq: 19,
        }),
      },
      retained
    )
    expect(d?.phase).toBe("timed_out")
    expect(d?.error_code).toBe("tool_stalled_timeout")
  })
})

describe("isNewerDiagnostic equal millis transition_seq", () => {
  it("orders equal transition_at by transition_seq", () => {
    const lower = proj({
      lease_id: "a",
      transition_at: "2026-07-22T12:00:00.000Z",
      transition_seq: 3,
    })
    const higher = proj({
      lease_id: "z",
      transition_at: "2026-07-22T12:00:00.000Z",
      transition_seq: 4,
    })
    expect(isNewerDiagnostic(higher, lower)).toBe(true)
    expect(isNewerDiagnostic(lower, higher)).toBe(false)
  })
})
