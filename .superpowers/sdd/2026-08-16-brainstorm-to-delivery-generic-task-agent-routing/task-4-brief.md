### Task 4: Project routed producers and reviewers as independent nodes

**Dependencies:** Task 3 provides a validated `SimpleExpectedRoute` and warning-only fallback for every Plan Task.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` (route nodes/edges, state derivation, projection tests)
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-4-report.md` (do not commit)

**Interfaces:**

- Consumes: Task 3's `derive_simple_expected_route`, `reconcile_simple_progress_route`, and `run_matches_work_unit_key`; existing `WorkflowGraphSnapshot`/`WorkflowNodeSnapshot` DTOs.
- Produces: separate implementer/primary/auxiliary Simple nodes when a valid routing block exists; legacy Plans without routing retain the existing one-node-per-Task projection.

- [ ] **Step 1: Write failing graph fan-out tests**

Extend the existing `simple_projection_*` test setup with one normal routed Plan and one high routed Plan. Assert:

- normal creates `simple-task-1-implementer` and `simple-task-1-reviewer-primary` with an implementer-to-reviewer edge;
- high creates `simple-task-1-implementer`, `simple-task-1-reviewer-primary`, and `simple-task-1-reviewer-auxiliary` with two fan-out edges;
- both high reviewers with `agent_type=codex`, identical profile, and separate children remain distinct nodes;
- the next Task implementer depends on every reviewer from the previous Task;
- a reviewer run created before the latest implementer/fix run is stale, makes only that reviewer node out-of-sync, and adds `simple_task_review_stale`;
- completed progress missing one expected latest reviewer run cannot make the delivery complete and adds `simple_completed_task_route_incomplete`;
- a malformed/mismatched route produces warnings and no platform Gate;
- a legacy Plan/progress fixture still projects exactly one `simple-task-N` node per Task;
- archived manifest projection is unchanged.

- [ ] **Step 2: Run graph tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_ -- --nocapture
```

Expected: routed fixtures still collapse all runs into one aggregate Task node.

- [ ] **Step 3: Implement route-aware node construction**

For routed Tasks, group durable/progress runs by exact expected key. Compute each node from its own latest generation/run. Use stable IDs:

```text
simple-task-{index}-implementer
simple-task-{index}-reviewer-primary
simple-task-{index}-reviewer-auxiliary
```

Use Task title plus `Implementation`, `Primary review`, or `Auxiliary review` as the bounded display title. An admitted `reserving`/`running` run overrides pending state for only its node. A completed route node is current only when its latest terminal run is completed and, for a reviewer, its `created_at` is not older than the latest implementer/fix run's `created_at`; failed/canceled required route nodes are blocked. Aggregate Task `completed` status cannot fill in a missing expected node. Keep `gates: []`, `workflow_id: None`, `manifest_revision: None`, and `compatibility: Simple`.

For routed edges, connect prior Task reviewer node(s) to the next implementer; connect the current implementer to each current reviewer. For legacy Plans, execute the existing aggregate node branch without changing IDs or state rules.

- [ ] **Step 4: Run Task 4 GREEN and projection regressions**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_ -- --nocapture
cargo test --lib --features test-utils workflow::project::tests -- --nocapture
cargo check --lib --features test-utils
```

Expected: all route-aware, legacy Simple, observed-only, and archived projection tests PASS; Rust library compiles.

- [ ] **Step 5: Commit Task 4**

```bash
git add -- src-tauri/src/acp/delegation/workflow/project.rs
git commit -m "feat(workflow): project adaptive Simple task routes"
```

- [ ] **Step 6: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-4-report.md` with node/edge fixtures, state/edge outcomes, commands, commit hash, and retained Minors. Do not stage it.

---

