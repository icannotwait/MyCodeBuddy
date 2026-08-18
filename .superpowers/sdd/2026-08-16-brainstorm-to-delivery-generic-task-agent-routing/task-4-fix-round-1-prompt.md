You are the fresh independent Codex implementer for Task 4 fix round 1.

Work only in:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read first:

- Task brief: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-brief.md`
- Implementer report: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-report.md`
- Primary review: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-primary-review.md`
- Current implementation commit: `1fee56a033dae3f8b749e0d335140c20ee535afd`

Fix this open Important finding, preserving its technical intent:

> Progress runs are reduced to optional durable-row lookups by `task_id`; their
> own `work_unit_key` and `state` never participate in node grouping or node
> derivation. The later implementer/reviewer filters operate exclusively on
> durable rows. Consequently, a progress-only exact-key run remains an
> unobserved pending node, and a progress entry whose expected reviewer key
> points to a durable implementer row silently populates the implementer rather
> than emitting a bounded progress/durable disagreement warning. Build
> route-local groups from both sources, validate a progress reference's key
> against the resolved durable row before accepting it, emit a bounded mismatch
> warning, and add focused progress-only and conflicting-key tests.

The finding is consistent with the brief's explicit requirement to group
durable/progress runs by exact expected key. Implement conservatively: do not
invent authoritative platform state, admission rules, Gates, manifests, or new
document fields. Progress remains non-authoritative and discrepancies remain
bounded projection warnings. If the existing progress shape cannot support a
safe part of the requested behavior, stop with NEEDS_CONTEXT and explain the
specific ambiguity rather than broadening scope.

Follow RED-GREEN-REFACTOR. Before production edits, add focused tests that fail
for the intended missing behavior. Keep all production/test edits in
`src-tauri/src/acp/delegation/workflow/project.rs`. Run only Rust commands with
`--no-default-features --features server,test-utils`; never enable default
`tauri-runtime`. Use focused permitted tests plus formatting/diff checks.

Append a fix-round section to the existing Task 4 report with RED/GREEN
evidence, exact commands/counts, changed behavior, self-review, and concerns.
Commit only `project.rs` with message:
`fix(workflow): reconcile Simple progress route nodes`

Write your final status to:
`.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-fix-round-1-result.md`
without staging it. Return only status, commit, one-line test summary,
concerns, and result/report paths.
