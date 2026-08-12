import { describe, expect, it } from "vitest"

import type { WorkflowGraphSnapshot } from "./types"

describe("workflow graph transport contract", () => {
  it("accepts the Simple locator and pending lifecycle shape", () => {
    const snapshot = {
      schema_version: 1,
      workflow_kind: "brainstorm_to_delivery",
      compatibility: "simple",
      overall_state: "pending",
      simple: {
        plan_rel_path: "docs/superpowers/plans/plan.md",
        progress_rel_path: ".superpowers/sdd/42/progress.md",
      },
      projection_warning_codes: ["simple_progress_block_missing"],
      current_phase_id: "tasks",
      current_node_ids: ["simple-task-1"],
      phases: [],
      nodes: [],
      edges: [],
      gates: [],
    } satisfies WorkflowGraphSnapshot

    expect(snapshot.compatibility).toBe("simple")
    expect(snapshot.overall_state).toBe("pending")
    expect(snapshot.simple).toEqual({
      plan_rel_path: "docs/superpowers/plans/plan.md",
      progress_rel_path: ".superpowers/sdd/42/progress.md",
    })
  })

  it("accepts archived successor navigation without Simple-only fields", () => {
    const snapshot = {
      schema_version: 1,
      workflow_id: "pub_workflow",
      workflow_kind: "brainstorm_to_delivery",
      manifest_revision: 3,
      graph_revision: 5,
      manifest_state: "approved",
      compatibility: "manifest",
      overall_state: "approved",
      archived: {
        source_conversation_id: 7,
        plan_rel_path: "docs/superpowers/plans/plan.md",
        successor_conversation_id: null,
        can_create_simple_successor: false,
      },
      projection_warning_codes: [],
      current_node_ids: [],
      phases: [],
      nodes: [],
      edges: [],
      gates: [],
    } satisfies WorkflowGraphSnapshot

    expect(snapshot.archived?.successor_conversation_id).toBe(null)
    expect(snapshot.archived?.can_create_simple_successor).toBe(false)
    expect("simple" in snapshot).toBe(false)
  })
})
