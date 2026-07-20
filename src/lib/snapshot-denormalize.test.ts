import { describe, expect, it } from "vitest"

import { denormalizeSnapshot } from "@/lib/snapshot-denormalize"
import type { LiveSessionSnapshot } from "@/lib/types"

function baseSnapshot(
  overrides: Partial<LiveSessionSnapshot> = {}
): LiveSessionSnapshot {
  return {
    connection_id: "conn-1",
    conversation_id: null,
    folder_id: null,
    status: "connected",
    external_id: null,
    live_message: null,
    active_tool_calls: [],
    pending_permission: null,
    modes: null,
    current_mode: null,
    config_options: null,
    prompt_capabilities: null,
    usage: null,
    fork_supported: false,
    available_commands: [],
    selectors_ready: false,
    event_seq: 0,
    ...overrides,
  }
}

describe("denormalizeSnapshot — active_delegations", () => {
  it("carries active_delegations through to the patch", () => {
    const patch = denormalizeSnapshot(
      baseSnapshot({
        active_delegations: [
          {
            parent_tool_use_id: "pt-1",
            child_connection_id: "c1",
            child_conversation_id: 9,
            agent_type: "codex",
            task_id: "task-1",
            started_at: "2026-07-19T00:00:00.000Z",
            runtime_stats: {
              started_at: "2026-07-19T00:00:00.000Z",
              tool_call_count: 0,
              edit_tool_call_count: 0,
              touched_files: [],
              touched_files_truncated: false,
              line_counts_complete: false,
            },
          },
        ],
      })
    )
    expect(patch.activeDelegations).toHaveLength(1)
    expect(patch.activeDelegations[0].parent_tool_use_id).toBe("pt-1")
    expect(patch.activeDelegations[0].child_conversation_id).toBe(9)
  })

  it("defaults activeDelegations to [] when the field is absent (older server payload)", () => {
    const snap = baseSnapshot()
    // Older server payloads omit the field entirely.
    delete (snap as { active_delegations?: unknown }).active_delegations
    const patch = denormalizeSnapshot(snap)
    expect(patch.activeDelegations).toEqual([])
  })
})

describe("denormalizeSnapshot — config staleness", () => {
  it("carries config_stale / config_stale_kind into the patch", () => {
    const patch = denormalizeSnapshot(
      baseSnapshot({ config_stale: true, config_stale_kind: "model_provider" })
    )
    expect(patch.configStale).toBe(true)
    expect(patch.configStaleKind).toBe("model_provider")
  })

  it("defaults to not-stale when the fields are absent (older server payload)", () => {
    const snap = baseSnapshot()
    delete (snap as { config_stale?: unknown }).config_stale
    delete (snap as { config_stale_kind?: unknown }).config_stale_kind
    const patch = denormalizeSnapshot(snap)
    expect(patch.configStale).toBe(false)
    expect(patch.configStaleKind).toBeNull()
  })
})

describe("denormalizeSnapshot — delegation route", () => {
  it("denormalizes route snapshot without deriving it from settings", () => {
    const patch = denormalizeSnapshot(
      baseSnapshot({
        delegation_route: {
          requested: "codeg",
          effective: "native",
          source: "safe_fallback",
          managed: true,
          degraded_reason: "companion_binary_unavailable",
          delegation_available: false,
        },
      })
    )
    expect(patch.delegationRoute).toEqual({
      requested: "codeg",
      effective: "native",
      source: "safe_fallback",
      managed: true,
      degraded_reason: "companion_binary_unavailable",
      delegation_available: false,
    })
  })

  it("defaults delegationRoute to null when the field is absent", () => {
    const snap = baseSnapshot()
    delete (snap as { delegation_route?: unknown }).delegation_route
    const patch = denormalizeSnapshot(snap)
    expect(patch.delegationRoute).toBeNull()
  })
})

describe("denormalizeSnapshot — waiting_for_subagents", () => {
  const waiting = {
    conversation_id: 42,
    state: "waiting" as const,
    generation: 3,
    armed_at: "2026-01-01T00:00:00.000Z",
    wake_at: "2026-01-01T00:04:00.000Z",
  }

  it("hydrates waiting_for_subagents into the patch so send can be gated", () => {
    const patch = denormalizeSnapshot(
      baseSnapshot({
        status: "connected",
        waiting_for_subagents: waiting,
      })
    )
    expect(patch.waitingForSubagents).toEqual(waiting)
    // Waiting is independent of connection status / turn_in_flight.
    expect(patch.status).toBe("connected")
  })

  it("defaults waitingForSubagents to null when the field is absent", () => {
    const snap = baseSnapshot()
    delete (snap as { waiting_for_subagents?: unknown }).waiting_for_subagents
    const patch = denormalizeSnapshot(snap)
    expect(patch.waitingForSubagents).toBeNull()
  })

  it("clears waiting when snapshot projects null", () => {
    const patch = denormalizeSnapshot(
      baseSnapshot({ waiting_for_subagents: null })
    )
    expect(patch.waitingForSubagents).toBeNull()
  })
})
