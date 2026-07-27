import { beforeEach, describe, expect, it, vi } from "vitest"

const { getDelegationRunSnapshotMock } = vi.hoisted(() => ({
  getDelegationRunSnapshotMock: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  getDelegationRunSnapshot: getDelegationRunSnapshotMock,
}))

import {
  DelegationRunSnapshotCache,
  normalizeCardSummary,
  normalizeDelegationRunSnapshot,
} from "@/lib/delegation-run-snapshot"
import { getActiveBackendCacheKey } from "@/lib/transport"
import type { DelegationRunSnapshot } from "@/lib/types"

function snapshot(
  overrides: Partial<DelegationRunSnapshot> = {}
): DelegationRunSnapshot {
  return {
    task_id: "run-1",
    root_task_id: "run-1",
    previous_task_id: null,
    generation: 1,
    parent_tool_use_id: "tool-1",
    child_conversation_id: 42,
    agent_type: "codex",
    profile_id: null,
    task_preview: "Review the patch",
    status: "completed",
    error_code: null,
    started_at: "2026-07-21T10:00:00.000Z",
    finished_at: "2026-07-21T10:01:00.000Z",
    runtime_stats: null,
    card_summary: {
      kind: "review",
      verdict: "approve",
      critical: 0,
      important: 0,
      minor: 0,
      summary: "First run is frozen.",
    },
    child_turn_anchor: null,
    replaced_task_id: null,
    replacement_reason: null,
    ...overrides,
  }
}

describe("delegation run snapshots", () => {
  beforeEach(() => {
    getDelegationRunSnapshotMock.mockReset()
  })

  it("drops an invalid card summary without dropping the run snapshot", () => {
    const normalized = normalizeDelegationRunSnapshot(
      snapshot({
        card_summary: {
          kind: "review",
          verdict: "invalid",
          critical: 0,
          important: 0,
          minor: 0,
          summary: "bad",
        } as unknown as DelegationRunSnapshot["card_summary"],
      })
    )

    expect(normalized.task_id).toBe("run-1")
    expect(normalized.card_summary).toBeNull()
  })

  it("drops implementation summaries with out-of-bounds report_file paths", () => {
    const normalized = normalizeDelegationRunSnapshot(
      snapshot({
        card_summary: {
          kind: "implementation",
          phase: "fix",
          status: "done",
          summary: "done",
          report_file: "/etc/passwd",
        },
      })
    )

    expect(normalized.task_id).toBe("run-1")
    expect(normalized.card_summary).toBeNull()
  })

  it("drops implementation summaries with oversized test status text", () => {
    const normalized = normalizeDelegationRunSnapshot(
      snapshot({
        card_summary: {
          kind: "implementation",
          phase: "fix",
          status: "done",
          summary: "done",
          tests: { status: "x".repeat(65) },
        },
      })
    )

    expect(normalized.card_summary).toBeNull()
  })

  it("preserves valid Author and Review role evidence", () => {
    expect(
      normalizeCardSummary({
        kind: "author",
        status: "done",
        summary: "Plan is ready.",
        plan_digest: "sha256:plan-v2",
        report_file: "docs/superpowers/plans/adaptive-routing.md",
      })
    ).toEqual({
      kind: "author",
      status: "done",
      summary: "Plan is ready.",
      plan_digest: "sha256:plan-v2",
      report_file: "docs/superpowers/plans/adaptive-routing.md",
    })

    expect(
      normalizeCardSummary({
        kind: "review",
        verdict: "approve",
        critical: 0,
        important: 0,
        minor: 0,
        summary: "Plan approved.",
        report_file: ".superpowers/sdd/plan-review.md",
      })
    ).toEqual({
      kind: "review",
      verdict: "approve",
      critical: 0,
      important: 0,
      minor: 0,
      summary: "Plan approved.",
      report_file: ".superpowers/sdd/plan-review.md",
    })
  })

  it("rejects Author summaries without a non-empty digest", () => {
    const validBase = {
      kind: "author",
      status: "done",
      summary: "Plan is ready.",
      report_file: "docs/superpowers/plans/adaptive-routing.md",
    }
    expect(normalizeCardSummary(validBase)).toBeNull()
    expect(normalizeCardSummary({ ...validBase, plan_digest: "" })).toBeNull()
  })

  it.each(["C:/repo/report.md", "/repo/report.md", "../report.md"])(
    "rejects unsafe role-specific report path %s",
    (report_file) => {
      expect(
        normalizeCardSummary({
          kind: "author",
          status: "done",
          summary: "Plan is ready.",
          plan_digest: "sha256:plan-v2",
          report_file,
        })
      ).toBeNull()
      expect(
        normalizeCardSummary({
          kind: "review",
          verdict: "approve",
          critical: 0,
          important: 0,
          minor: 0,
          summary: "Plan approved.",
          report_file,
        })
      ).toBeNull()
    }
  )

  it("does not overwrite a terminal card when a later fetch shares its key", () => {
    const cache = new DelegationRunSnapshotCache()
    const key = `${getActiveBackendCacheKey()}\0${10}\0run-1`
    const first = snapshot()
    const staleLaterFetch = snapshot({
      generation: 2,
      task_preview: "This must not replace the first card",
      card_summary: {
        kind: "implementation",
        phase: "fix",
        status: "done",
        summary: "Later run",
      },
    })

    cache.install(key, first)
    cache.install(key, staleLaterFetch)

    expect(cache.get(10, "run-1")).toMatchObject({
      generation: 1,
      task_preview: "Review the patch",
      card_summary: first.card_summary,
    })
  })

  it("revalidates a cached running snapshot until the run becomes terminal", async () => {
    const cache = new DelegationRunSnapshotCache()
    const key = `${getActiveBackendCacheKey()}\0${10}\0run-1`
    cache.install(
      key,
      snapshot({
        status: "running",
        finished_at: null,
        card_summary: null,
      })
    )
    getDelegationRunSnapshotMock.mockResolvedValueOnce(
      snapshot({
        status: "completed",
        finished_at: "2026-07-21T10:01:00.000Z",
      })
    )

    cache.ensure(10, "run-1")

    await vi.waitFor(() => {
      expect(cache.get(10, "run-1")?.status).toBe("completed")
    })
    expect(getDelegationRunSnapshotMock).toHaveBeenCalledWith(10, "run-1")
  })
})
