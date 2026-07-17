import { describe, expect, it } from "vitest"

import { parseInput, resolveDelegationStatus } from "@/lib/delegation-card"
import type { DelegationBinding } from "@/contexts/delegation-context"
import { AGENT_LABELS, ALL_AGENT_TYPES } from "@/lib/types"

function binding(
  overrides: Partial<DelegationBinding> = {}
): DelegationBinding {
  return {
    parentConnectionId: "p1",
    parentToolUseId: "pt-1",
    childConnectionId: "c1",
    childConversationId: 99,
    agentType: "codex",
    status: "running",
    observation: "active",
    ...overrides,
  }
}

describe("resolveDelegationStatus — live binding observation", () => {
  it.each([
    ["active", "active"],
    ["waiting_input", "waiting_input"],
    ["stalled", "stalled"],
  ] as const)(
    "maps running/%s binding observation to card status %s",
    (observation, expected) => {
      expect(
        resolveDelegationStatus({
          binding: binding({ status: "running", observation }),
          parsedMeta: null,
          toolOutput: null,
          state: "input-available",
          errorText: null,
          childAwaitingPermission: false,
        })
      ).toBe(expected)
    }
  )

  it("keeps ok/err terminal statuses and ignores observation", () => {
    expect(
      resolveDelegationStatus({
        binding: binding({ status: "ok", observation: "stalled" }),
        parsedMeta: null,
        toolOutput: null,
        state: "output-available",
        errorText: null,
        childAwaitingPermission: false,
      })
    ).toBe("ok")
    expect(
      resolveDelegationStatus({
        binding: binding({
          status: "err",
          observation: null,
          errorCode: "timeout",
        }),
        parsedMeta: null,
        toolOutput: null,
        state: "output-error",
        errorText: "failed",
        childAwaitingPermission: false,
      })
    ).toBe("err")
  })

  it("prefers permission waiting over observation", () => {
    expect(
      resolveDelegationStatus({
        binding: binding({
          status: "running",
          observation: "stalled",
        }),
        parsedMeta: null,
        toolOutput: null,
        state: "input-available",
        errorText: null,
        childAwaitingPermission: true,
      })
    ).toBe("waiting")
  })

  it("falls back to plain running when observation is absent", () => {
    expect(
      resolveDelegationStatus({
        binding: binding({ observation: undefined }),
        parsedMeta: null,
        toolOutput: null,
        state: "input-available",
        errorText: null,
        childAwaitingPermission: false,
      })
    ).toBe("running")
  })
})

describe("parseInput — historical agent types", () => {
  it("resolves agent_type grok for history reload without a live binding", () => {
    const parsed = parseInput(
      JSON.stringify({
        agent_type: "grok",
        task: "fix review findings",
        working_dir: "D:\\MyCodeBuddy",
      })
    )
    expect(parsed.agentType).toBe("grok")
    expect(ALL_AGENT_TYPES).toContain("grok")
    expect(AGENT_LABELS.grok).toBe("Grok")
    expect(parsed.task).toBe("fix review findings")
  })

  it("accepts every agent type in ALL_AGENT_TYPES", () => {
    for (const agentType of ALL_AGENT_TYPES) {
      const parsed = parseInput(
        JSON.stringify({ agent_type: agentType, task: "t" })
      )
      expect(parsed.agentType).toBe(agentType)
    }
  })
})
