import { describe, expect, it } from "vitest"

import type {
  AcpAgentInfo,
  DelegationProfile,
  ReferenceCandidate,
} from "@/lib/types"

import {
  agentToSuggestion,
  candidateToSuggestion,
  catalogSearchFields,
  profileToSuggestion,
} from "./adapters"

describe("agentToSuggestion", () => {
  it("maps to an agent reference with a codeg://agent routing uri", () => {
    const agent = {
      agent_type: "claude_code",
      name: "Claude Code",
      description: "Anthropic CLI",
      available: true,
    } as AcpAgentInfo
    const item = agentToSuggestion(agent)
    expect(item.reference).toMatchObject({
      refType: "agent",
      id: "claude_code",
      label: "Claude Code",
      uri: "codeg://agent/claude_code",
      meta: { agentType: "claude_code", available: true },
    })
  })
})

describe("profileToSuggestion", () => {
  it("maps a profile to a stable delegation route", () => {
    const profile: DelegationProfile = {
      id: "11111111-1111-4111-8111-111111111111",
      agent_type: "code_buddy",
      name: "GLM5.2",
      config_values: { model: "glm-5.2" },
      enabled: true,
      created_at: 1,
      updated_at: 1,
    }
    expect(profileToSuggestion(profile).reference).toEqual({
      refType: "delegation_profile",
      id: profile.id,
      label: "CodeBuddy:GLM5.2",
      uri: `codeg://delegation-profile/code_buddy/${profile.id}`,
      meta: { agentType: "code_buddy", profileId: profile.id },
    })
  })
})

describe("candidateToSuggestion", () => {
  it("maps backend candidates without rebuilding identity", () => {
    const file: ReferenceCandidate = {
      source: "file",
      uri: "file:///repo/src/app.ts",
      id: "src/app.ts",
      label: "app.ts",
      detail: "src/app.ts",
      keywords: "app.ts",
      metadata: {
        kind: "file",
        canonicalWorkspaceRoot: "/repo",
        relativePath: "src/app.ts",
        entryKind: "directory",
      },
      sourceOrdinal: 3,
      regexRank: { fieldTier: 1, start: 0, length: 3 },
    }
    const item = candidateToSuggestion(file, "cache", true)
    expect(item.reference).toMatchObject({
      refType: "file",
      id: "src/app.ts",
      uri: "file:///repo/src/app.ts",
      meta: { fileKind: "dir" },
    })
    expect(item.sourceOrdinal).toBe(3)
    expect(item.freshness).toBe("cache")
    expect(item.regexRank).toEqual({ fieldTier: 1, start: 0, length: 3 })
  })
})

describe("catalogSearchFields", () => {
  it("uses display name primary and type/description/model secondary", () => {
    const agent = {
      agent_type: "codex",
      name: "Codex CLI",
      description: "desc",
      available: true,
    } as AcpAgentInfo
    expect(catalogSearchFields({ kind: "agent", agent })).toEqual({
      primary: ["Codex CLI"],
      secondary: ["codex", "desc", ""],
    })
    const profile: DelegationProfile = {
      id: "11111111-1111-4111-8111-111111111111",
      agent_type: "code_buddy",
      name: "GLM",
      config_values: { model: "glm" },
      enabled: true,
      created_at: 1,
      updated_at: 1,
    }
    expect(
      catalogSearchFields({
        kind: "profile",
        profile,
        backingAgent: {
          agent_type: "code_buddy",
          name: "CodeBuddy",
          description: "CB",
          available: true,
        } as AcpAgentInfo,
      })
    ).toEqual({
      primary: ["CodeBuddy:GLM"],
      secondary: ["code_buddy", "CB", "glm"],
    })
  })
})
