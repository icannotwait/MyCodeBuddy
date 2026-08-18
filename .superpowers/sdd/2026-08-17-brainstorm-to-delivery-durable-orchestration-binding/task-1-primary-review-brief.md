# Task 1 Primary Review Brief

You are the independent Codex primary reviewer for high Task 1. You did
not implement this Task. Review only Task 1's latest producer result.

## Inputs

- Brief: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-brief.md`
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-report.md`
- Review package: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/review-db8c14c3..457f536c.diff`
- Plan Global Constraints:
  `docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md`

Working directory:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Range: `db8c14c3..457f536c`

## Binding constraints

- Simple remains manifest-free and platform-gate-free.
- Four nullable columns, no backfill, named shape/immutability triggers,
  and the exact lookup index.
- Binding is insert-fixed in the reserving transaction.
- Unbound request fingerprints stay byte-compatible.
- Bound fingerprints use the Design's 12-string v2 array.
- `DelegationRequest` and `ContinueDelegationRequest` must not yet expose
  `orchestration_binding`.
- Every Rust command uses `--no-default-features --features server,test-utils`.
- Shared fixture `src-tauri/tests/fixtures/orchestration_binding_v1.json`
  is the only grammar corpus.
- `agent_type` and `profile_id` are insert-fixed and must survive the
  lifecycle fault matrix.
- Task 6 still owns later `project.rs` warning logic; Task 1 may only
  update Model literals there.

Do not re-run the implementer's full library suite. Do not pre-clear
issues. Do not implement fixes.

Write the full review to:
`.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-primary-review.md`

Verdict both spec compliance and task quality. Return verdict, counts,
and finding one-liners.
