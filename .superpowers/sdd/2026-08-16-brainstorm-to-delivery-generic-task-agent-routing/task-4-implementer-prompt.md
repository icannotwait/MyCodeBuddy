You are the fresh independent Codex implementer for high-risk Task 4: project
routed producers and reviewers as independent Simple nodes.

Work only in:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read this first; it is your complete requirements brief with exact values:
`.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-brief.md`

Context from the completed dependency: Task 3 provides validated
`SimpleExpectedRoute`, `derive_simple_expected_route`,
`reconcile_simple_progress_route`, exact-key matching, canonical risk
validation, and warning-only legacy fallback in `project.rs`. Build on those
interfaces. Do not change the approved route contract or broaden file scope.

Follow RED-GREEN-REFACTOR. Keep all production/test changes within
`src-tauri/src/acp/delegation/workflow/project.rs`. Preserve legacy aggregate
projection and archived manifest behavior. Do not add Simple Gates, admission
authority, manifests, or completion settlement.

Binding test constraint: never run Rust tests or checks with default features.
Rust verification may be deferred until all Tasks, but every Rust command you
do run must use `--no-default-features --features server,test-utils`. Translate
the brief's default-feature commands accordingly. Do not start or claim any
default `tauri-runtime` run.

Once clear:

1. Implement exactly the brief using TDD.
2. Run focused permitted tests, formatting, and diff checks as feasible.
3. Commit only `src-tauri/src/acp/delegation/workflow/project.rs` with
   `feat(workflow): project adaptive Simple task routes`.
4. Write the full report to
   `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-report.md`
   without staging it. Include RED/GREEN evidence, node/edge/state outcomes,
   changed files, commit, self-review, and concerns.

Return only: status, commit, one-line test summary, concerns, and report path.
If requirements become ambiguous or need architecture beyond the brief, stop
with NEEDS_CONTEXT or BLOCKED instead of guessing.
