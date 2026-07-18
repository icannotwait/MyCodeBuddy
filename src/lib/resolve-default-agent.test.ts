import { describe, expect, it } from "vitest"
import {
  resolveDefaultAgent,
  type ResolveDefaultAgentInput,
} from "./resolve-default-agent"

const base: ResolveDefaultAgentInput = {
  folderDefault: null,
  inherit: null,
  folderRecent: null,
  sortedTypes: ["codex", "gemini"],
  fresh: true,
}

describe("resolveDefaultAgent project recency", () => {
  it("keeps the explicit folder default highest", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        folderDefault: "claude_code",
        inherit: "open_code",
        folderRecent: "gemini",
      })
    ).toEqual({ agentType: "claude_code", provisional: false })
  })

  it("keeps explicitly requested conversation inheritance above recency", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        inherit: "open_code",
        folderRecent: "gemini",
      })
    ).toEqual({ agentType: "open_code", provisional: false })
  })

  it("uses an available recent agent after fresh hydration", () => {
    expect(resolveDefaultAgent({ ...base, folderRecent: "gemini" })).toEqual({
      agentType: "gemini",
      provisional: false,
    })
  })

  it("returns recent recency provisionally before hydration", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        folderRecent: "gemini",
        sortedTypes: [],
        fresh: false,
      })
    ).toEqual({ agentType: "gemini", provisional: true })
  })

  it("corrects unavailable recency to the first fresh sorted agent", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        folderRecent: "gemini",
        sortedTypes: ["codex"],
      })
    ).toEqual({ agentType: "codex", provisional: false })
  })

  it("uses saved Agent order when the folder has no recency", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        sortedTypes: ["open_code", "codex"],
      })
    ).toEqual({ agentType: "open_code", provisional: false })
  })

  it("keeps the existing hard fallback provisional on a cold empty list", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        sortedTypes: [],
        fresh: false,
      })
    ).toEqual({ agentType: "codex", provisional: true })
  })
})
