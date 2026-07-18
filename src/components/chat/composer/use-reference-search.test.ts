import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import type { AcpAgentInfo } from "@/lib/types"

import {
  DEFAULT_GROUP_LABELS,
  useReferenceSearchController,
} from "./use-reference-search"

function makeAgent(
  agentType: string,
  over: { name?: string; description?: string; enabled?: boolean } = {}
): AcpAgentInfo {
  return {
    agent_type: agentType,
    name: over.name ?? agentType,
    description: over.description ?? "",
    available: true,
    enabled: over.enabled ?? true,
    sort_order: 0,
  } as unknown as AcpAgentInfo
}

const mocks = vi.hoisted(() => ({
  agents: [] as AcpAgentInfo[],
  agentsFresh: true,
  getGitHead: vi.fn(),
  profileReady: true,
  profileCatalog: null as null | {
    profiles: unknown[]
    delegation_enabled: boolean
    revision: number
  },
  profileError: null as string | null,
  referenceLimit: 50,
}))

vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: () => ({
    agents: mocks.agents,
    fresh: mocks.agentsFresh,
    refresh: vi.fn(),
  }),
}))
vi.mock("@/lib/api", () => ({
  getGitHead: (...args: unknown[]) => mocks.getGitHead(...args),
  startReferenceSearch: vi.fn(),
  nextReferenceSearchPage: vi.fn(),
  cancelReferenceSearch: vi.fn(),
  validateReferenceCandidate: vi.fn(),
  matchReferenceRegex: vi.fn(),
}))
vi.mock("@/stores/delegation-profile-store", () => ({
  useDelegationProfileStore: (
    selector: (s: {
      ready: boolean
      catalog: typeof mocks.profileCatalog
      error: string | null
    }) => unknown
  ) =>
    selector({
      ready: mocks.profileReady,
      catalog: mocks.profileCatalog,
      error: mocks.profileError,
    }),
}))
vi.mock("@/stores/conversation-experience-store", () => ({
  useConversationExperienceStore: (
    selector: (s: {
      settings: { reference_search_limit: number } | null
    }) => unknown
  ) =>
    selector({
      settings: { reference_search_limit: mocks.referenceLimit },
    }),
}))
vi.mock("@/lib/transport", () => ({
  getActiveBackendCacheKey: () => "test-backend",
}))

describe("useReferenceSearchController", () => {
  beforeEach(() => {
    mocks.agents = [makeAgent("codex", { name: "Codex" })]
    mocks.agentsFresh = true
    mocks.profileReady = true
    mocks.profileCatalog = {
      profiles: [],
      delegation_enabled: true,
      revision: 1,
    }
    mocks.profileError = null
    mocks.referenceLimit = 50
    mocks.getGitHead.mockReset().mockResolvedValue({
      is_repo: true,
      branch: "main",
      detached: false,
      short_sha: null,
      canonical_repo: "/repo",
      head_sha: "a".repeat(40),
      reference_source_epoch: "v1:epoch-a",
    })
  })

  it("returns null until the shared catalog is ready", () => {
    mocks.agentsFresh = false
    const { result } = renderHook(() =>
      useReferenceSearchController({
        folderId: 1,
        defaultPath: "/repo",
        enabled: true,
        labels: DEFAULT_GROUP_LABELS,
      })
    )
    expect(result.current).toBeNull()
  })

  it("creates a controller after readiness and closes on disable", async () => {
    const { result, rerender } = renderHook(
      (props: { enabled: boolean }) =>
        useReferenceSearchController({
          folderId: 1,
          defaultPath: "/repo",
          enabled: props.enabled,
          labels: DEFAULT_GROUP_LABELS,
        }),
      { initialProps: { enabled: true } }
    )

    await act(async () => {
      await Promise.resolve()
    })
    expect(result.current).not.toBeNull()
    result.current!.setQuery("")
    expect(
      result
        .current!.getSnapshot()
        .groups.agent.items.map((i) => i.reference.uri)
    ).toEqual(["codeg://agent/codex"])

    await act(async () => {
      rerender({ enabled: false })
    })
    expect(result.current).toBeNull()
  })
})
