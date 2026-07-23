import { describe, expect, it } from "vitest"
import type { ToolWatchdogProjection } from "@/lib/types"
import {
  diagnosticFieldKeys,
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
      timestamp: "2026-07-22T12:00:00Z",
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
})

describe("pickLatestToolWatchdogDiagnostic", () => {
  it("prefers higher-version live lease over older last", () => {
    const d = pickLatestToolWatchdogDiagnostic(
      {
        a: proj({ lease_id: "a", version: 5, phase: "grace" }),
      },
      proj({ version: 2, phase: "timed_out" })
    )
    expect(d?.phase).toBe("grace")
  })

  it("uses last terminal when live map is empty", () => {
    const d = pickLatestToolWatchdogDiagnostic(
      {},
      proj({ phase: "timed_out", error_code: "user_cancelled" })
    )
    expect(d?.error_code).toBe("user_cancelled")
    expect(d?.phase).toBe("timed_out")
  })

  it("returns null when nothing is known", () => {
    expect(pickLatestToolWatchdogDiagnostic(undefined, null)).toBeNull()
  })
})
