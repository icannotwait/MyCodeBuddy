You are the Task 3 implementer resuming for fix round 1/5. Work only in:

`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read first:

- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-brief.md`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-report.md`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-primary-review.md`

The independent primary review found two Important issues. They have been
verified against the Task brief and codebase and both require fixes:

1. Route derivation accepts the recorded `risk.level` without validating
   `b2d_task_risk_v1`: hard triggers must force `high`; otherwise soft evidence
   is deduplicated and score `0..=2` is `normal`, `>=3` is `high`; the recorded
   score and level must be consistent. Add focused boundary, hard-trigger, and
   duplicate-evidence tests.
2. Child independence currently checks progress-file runs only. Group admitted
   durable runs by non-null child conversation ID and complete expected key;
   emit `simple_progress_route_child_not_independent` when one child appears
   under two different expected keys, even if progress is missing or stale.
   Add a database-backed projection test.

Follow RED-GREEN-REFACTOR for each behavior. Keep the fix inside Task 3's
`project.rs` scope. Preserve warning-only projection and legacy aggregate
fallback; do not add admission or Gate authority.

Binding test constraint: do not run Rust tests with default features. Rust
tests may be deferred until all Tasks, but any test you run must use
`--no-default-features --features server,test-utils`. Do not start or claim a
default `tauri-runtime` run.

After the fixes, run focused permitted tests if feasible, run formatting/diff
checks, commit with a focused fix commit, and append Fix Round 1 evidence to
`.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-report.md` without staging the report.

Return only: status, commit, one-line test summary, concerns, and report path.
