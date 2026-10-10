import { describe, expect, it } from "vitest"
import {
  agentStatusReasonKey,
  defaultRoundtableAgents,
  isAgentSelectable,
  namedSeat,
  nextRoundtableAgent,
  ROUNDTABLE_CANDIDATES,
  sortRoundtableAgents,
  statusByAgent,
  type RoundtableAgentStatus,
} from "./agents"

function agentRow(
  agent: string,
  status: RoundtableAgentStatus["status"],
  extra: Partial<RoundtableAgentStatus> = {}
): RoundtableAgentStatus {
  return {
    agent,
    label: agent,
    status,
    enabled: status !== "disabled",
    installed: status !== "not_installed",
    installed_version: "1",
    profile_version: "1",
    version_matches_profile: true,
    qualified: status === "ready",
    credential: status === "credential_missing" ? "missing" : "settings",
    credential_keys: [],
    last_qualification: { verdict: null, failed_checks: [], observed_at: null },
    ...extra,
  }
}

const rows = statusByAgent([
  agentRow("grok", "unqualified"),
  agentRow("antigravity", "ready"),
  agentRow("cursor", "ready"),
  agentRow("codex", "disabled"),
  agentRow("code_buddy", "credential_missing"),
])

describe("roundtable agent availability", () => {
  it("keeps the five fixed candidates", () => {
    expect(ROUNDTABLE_CANDIDATES).toEqual([
      "grok",
      "antigravity",
      "cursor",
      "codex",
      "code_buddy",
    ])
  })

  it("sorts ready first and disabled last, keeping the fixed order", () => {
    expect(sortRoundtableAgents(ROUNDTABLE_CANDIDATES, rows)).toEqual([
      "antigravity",
      "cursor",
      "grok",
      "code_buddy",
      "codex",
    ])
    expect(sortRoundtableAgents(ROUNDTABLE_CANDIDATES, new Map())).toEqual([
      ...ROUNDTABLE_CANDIDATES,
    ])
  })

  it("defaults to the first two qualified agents", () => {
    expect(defaultRoundtableAgents(rows)).toEqual(["antigravity", "cursor"])
    const one = statusByAgent([agentRow("code_buddy", "ready")])
    expect(defaultRoundtableAgents(one)).toEqual(["code_buddy", "grok"])
    expect(defaultRoundtableAgents(new Map())).toEqual(["grok", "antigravity"])
  })

  it("adds an unused ready agent and never a blocked one", () => {
    expect(nextRoundtableAgent(["antigravity"], rows)).toBe("cursor")
    expect(nextRoundtableAgent(["antigravity", "cursor"], rows)).toBe(
      "antigravity"
    )
    expect(isAgentSelectable("codex", rows)).toBe(false)
    expect(isAgentSelectable("grok", new Map())).toBe(true)
  })

  it("names a version drift separately from a failed qualification", () => {
    expect(
      agentStatusReasonKey(
        agentRow("code_buddy", "unqualified", {
          version_matches_profile: false,
        })
      )
    ).toBe("version_mismatch")
    expect(agentStatusReasonKey(agentRow("grok", "unqualified"))).toBe(
      "unqualified"
    )
  })

  it("reads the seat a readiness error names", () => {
    expect(
      namedSeat({
        code: "capability_unqualified",
        details: {
          reason: "adapter_unqualified",
          field_errors: [{ path: "participants[1].agent", reason: "cursor" }],
        },
      })
    ).toEqual({ ordinal: 1, agent: "cursor" })
    expect(namedSeat({ details: { reason: "x", field_errors: [] } })).toBe(null)
    expect(namedSeat(new Error("x"))).toBe(null)
  })
})
