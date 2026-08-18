# Task 4 Report: Route-Aware Simple Graph Projection

## Status

DONE_WITH_CONCERNS

Commit: `1fee56a033dae3f8b749e0d335140c20ee535afd feat(workflow): project adaptive Simple task routes`

## Files Changed

Committed:

- `src-tauri/src/acp/delegation/workflow/project.rs` (`+700/-14`)

Uncommitted by requirement:

- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-report.md`

No production file was changed while reconstructing this report, and the Task
4 commit was not amended.

## Implementation

- Added route-aware projection for every Task whose routing block validates
  through Task 3's `derive_simple_expected_route` contract.
- Grouped durable and progress-referenced runs by exact expected work-unit key,
  preserving implementer, primary reviewer, and auxiliary reviewer as separate
  nodes even when Agent type and profile are identical.
- Added bounded route titles and projected expected Agent/profile metadata from
  the canonical route key.
- Derived each route node from its own latest generation/run. Reserving and
  running affect only the matching node; completed terminal runs complete only
  their matching node; failed/canceled required runs block their matching node.
- Added stale-review detection against the latest implementer/fix run timestamp.
  A stale completed reviewer becomes `waiting_review`, is the only otherwise
  clean node marked out of sync, and receives `simple_task_review_stale`.
- Prevented declared aggregate Task completion from filling a missing or stale
  expected route node. Such a node receives
  `simple_completed_task_route_incomplete`, and the delivery cannot project as
  complete.
- Retained the legacy aggregate branch for Plans with no valid route. Invalid
  or mismatched routing remains warning-only and does not create a platform
  Gate.
- Updated routed current-node selection so an active Task points to its active
  or dependency-ready route nodes instead of a nonexistent aggregate node.

## Node And Edge Outcomes

Normal route:

```text
simple-task-1-implementer
  -> simple-task-1-reviewer-primary
```

High route:

```text
simple-task-1-implementer
  -> simple-task-1-reviewer-primary
  -> simple-task-1-reviewer-auxiliary

simple-task-1-reviewer-primary   -> simple-task-2-implementer
simple-task-1-reviewer-auxiliary -> simple-task-2-implementer
```

- The high-route fixture projects primary and auxiliary reviewers as distinct
  nodes with `agent_type=codex`, the same profile, and separate child
  conversation IDs.
- The next Task implementer depends on every reviewer node from the previous
  Task.
- Routed nodes keep `required: true`, `gates: []`, `workflow_id: None`,
  `manifest_revision: None`, and `compatibility: Simple`.
- A malformed route falls back to the stable legacy `simple-task-N` node and
  adds `simple_plan_routing_invalid`; legacy Plans retain one aggregate node per
  Task.
- Existing archived manifest projection tests pass unchanged.

## Test-First And RED Evidence

The commit contains six focused route-projection regression tests plus routed
fixture helpers in the same required source file. They cover normal fan-out,
high fan-out and cross-Task dependencies, reviewer identity independence,
stale review state, incomplete completed routes, invalid-route fallback, and
failed/canceled required nodes.

No Task 4 command transcript or pre-implementation report was preserved before
the prior process was interrupted, and the tests and implementation were
squashed into the same commit. Therefore chronological test-first execution
and an exact RED command result cannot be independently recovered from Git and
are not claimed here.

The observable static RED evidence is the `ee2dfd62..1fee56a0` diff:

- At `ee2dfd62`, `project_simple_mode` always constructed one
  `simple-task-{index}` aggregate node and a single linear edge per Task.
- The added normal-route test requires two route-specific IDs and an
  implementer-to-primary edge, none of which the base projector produced.
- The added high-route test requires three distinct Task 1 nodes, two fan-out
  edges, and both reviewer dependencies on the next implementer, none of which
  the base projector produced.
- The stale and missing-review tests look up route-specific reviewer nodes;
  those nodes did not exist in the base projector.
- The failed/canceled test requires route-local blocked states, while the base
  projector had only aggregate Task state.

Thus the added regression expectations are observably incompatible with the
base implementation and establish the intended RED condition statically. No
failure count or runtime output is invented.

## Fresh GREEN Evidence

All Rust verification used the binding server-only feature set; no default
`tauri-runtime` command was started.

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_ -- --nocapture
```

Result: PASS, 22 passed / 0 failed / 4589 filtered out in 2.42s.

```text
cargo test --no-default-features --features server,test-utils --lib workflow::project::tests -- --nocapture
```

Result: PASS, 61 passed / 0 failed / 4550 filtered out in 6.50s.

```text
cargo check --no-default-features --features server,test-utils --lib
```

Result: PASS, exit 0; finished in 43.21s.

Formatting and diff checks:

```text
cargo fmt --check -- src/acp/delegation/workflow/project.rs
git diff-tree --check ee2dfd62 1fee56a033dae3f8b749e0d335140c20ee535afd
git diff --check
git diff --cached --check
```

Result: PASS with no output.

## Self-Review

- Compared the committed implementation and tests against every Task 4 brief
  outcome.
- Confirmed exact-key grouping prevents same-Agent/same-profile reviewers from
  collapsing into one node.
- Confirmed routed edges fan out from the implementer and fan in from all prior
  reviewers to the next implementer.
- Confirmed stale review and incomplete completion warnings are bounded through
  the existing warning helper and flow to the graph-level warning list.
- Confirmed failed/canceled routed runs block only their matching required node,
  while admitted reserving/running runs override only their matching node.
- Confirmed the valid routed branch does not add Gates, admission authority,
  manifests, or completion settlement.
- Confirmed invalid/no routing continues through the existing aggregate branch,
  retaining legacy IDs and state rules.
- Confirmed the commit changes only the required production/test source file.

## Concerns

- No executable Task 4 RED transcript survived the interrupted prior process;
  only the static, observable pre/post diff evidence above is available.
- The macOS linker emits the pre-existing `__eh_frame section too large`
  warning for the Rust library test binary. Both fresh permitted test suites
  still completed with zero failed tests.
- Default `tauri-runtime` verification is intentionally not run or claimed due
  to the binding server-only instruction.

## Fix Round 1

### Status

DONE_WITH_CONCERNS

Commit: `145651efcebb770fcb72b46061b8c5921172e5dc fix(workflow): reconcile Simple progress route nodes`

### Changed Behavior

- Routed Task nodes now use route-local groups keyed by the complete expected
  work-unit key, with separate durable and progress observations.
- An exact-key progress-only run now contributes its state and bounded display
  metadata to only its matching route node. The last progress entry for that
  key is used only when the group has no durable row.
- Durable rows remain the preferred observation whenever present; progress
  does not override durable status or create platform authority.
- A progress reference that resolves to a durable row with a different
  work-unit key is excluded from the claimed progress group and emits the
  bounded/deduplicated `simple_progress_run_durable_key_mismatch` projection
  warning.
- Gates, manifests, admission rules, document fields, and legacy aggregate
  projection behavior are unchanged.

### RED Evidence

The two focused tests were added before production edits, then run with:

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_progress_route_ -- --nocapture
```

Result: expected failure, 0 passed / 2 failed / 4611 filtered out. The
progress-only reviewer was `Pending` instead of `Running`, and the conflicting
progress/durable key emitted no mismatch warning.

### GREEN Evidence

Focused fix verification:

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_progress_route_ -- --nocapture
```

Result: PASS, 2 passed / 0 failed / 4611 filtered out in 0.26s.

Simple projection regression suite:

```text
cargo test --no-default-features --features server,test-utils --lib simple_projection_ -- --nocapture
```

Result: PASS, 24 passed / 0 failed / 4589 filtered out in 2.94s.

Project module regression suite:

```text
cargo test --no-default-features --features server,test-utils --lib workflow::project::tests -- --nocapture
```

Result: PASS, 63 passed / 0 failed / 4550 filtered out in 7.90s.

Server/test-utils library check:

```text
cargo check --no-default-features --features server,test-utils --lib
```

Result: PASS, exit 0; finished in 53.83s.

Formatting and diff checks:

```text
cargo fmt --check -- src/acp/delegation/workflow/project.rs
git diff --check
git diff --cached --check
```

Result: PASS with no output. The staged file list contained only
`src-tauri/src/acp/delegation/workflow/project.rs` before commit.

One initial post-implementation focused run compiled successfully but failed
at link time with `errno=28` before executing tests. The generated
`src-tauri/target` directory occupied 19 GiB while the volume had 1.4 GiB
available. `cargo clean` removed 12,355 generated files (27.5 GiB reported by
Cargo), after which all GREEN commands above completed. This failed link is
not counted as GREEN evidence.

### Self-Review

- Confirmed progress grouping uses the complete expected route key, never
  generic role or Agent identity.
- Confirmed an unresolved exact-key progress reference can observe only its
  matching node, while a resolved key conflict is warning-only and cannot
  populate the claimed reviewer group.
- Confirmed matching progress references are not double-counted when their
  durable row already represents the run.
- Confirmed durable sorting, reviewer staleness checks, route topology, legacy
  projection, and bounded warning propagation remain intact.
- Mutation check: removing progress grouping makes the progress-only test fail;
  accepting a differently keyed durable reference or removing the warning
  makes the conflict test fail.
- Confirmed the commit contains only production/tests in `project.rs`; this
  report and the result file remain ignored and unstaged.

### Concerns

- Progress entries expose no timestamp or generation comparable to durable
  rows. The projection therefore conservatively prefers durable observations
  whenever a route group has one and does not infer cross-source ordering.
- The macOS linker continues to emit the pre-existing `__eh_frame section too
  large` warning for the Rust library test binary, although all permitted test
  suites completed with zero failures.
- Default `tauri-runtime` verification was not started or claimed.
