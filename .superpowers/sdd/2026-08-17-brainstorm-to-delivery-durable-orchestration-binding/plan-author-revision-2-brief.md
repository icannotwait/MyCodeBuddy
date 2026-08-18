# Plan Author Revision 2

Continue the same Plan Author work unit. Re-inspect Git, the latest Plan,
plan-rereview-1.md, the Design, and current sources. Treat earlier reasoning
as provisional. Edit only the Plan file. Append to the Plan Author report.
Do not implement production code.

Independent re-review is CHANGES REQUIRED: 0 Critical, 2 Important.

- I-1, I-3, I-4, I-5, I-6, I-7, M-1: ADDRESSED. Do not reopen them.
- I-2: NOT ADDRESSED. Fix it.
- I-8: new Important. Fix it.

## I-2 remaining

Task 1 adds four fields to `delegation_task_run::Model`. Current HEAD has
complete Model literals that must name those fields, including:

- `src-tauri/src/acp/delegation/workflow/project.rs` around lines 4785,
  4834, and 4852
- Re-scan every `delegation_task_run::Model {` literal and any other
  complete Model constructor, including `listener.rs`, `run_store.rs`,
  and `workflow/completion_evidence.rs`

Add every compile-breaking Model literal to Task 1 ownership, GREEN
`cargo test --lib` / `cargo check --tests` evidence, and the commit
`git add` list. Task 6 may still own later warning logic in `project.rs`,
but Task 1 must make the serial commit compile.

## I-8

Do not call `workflow_auth_context` for
`get_delegation_orchestration_bindings`. At HEAD that helper requires
`entry.workflow_v2`, and production feature parsing leaves `workflow_v2`
false. Using it would hide the new query in production; loosening the
shared helper would risk retired workflow-v2 mutation paths.

Specify a separate read-only auth path in Task 3:

- token lookup
- root role
- the intended coordination/delegation gate
- current parent-conversation resolution
- no `workflow_v2` requirement
- tests that succeed while `workflow_v2` is false
- tests that all workflow-v2 mutation tools remain retired/unavailable

Keep the query token-scoped and parent-id-free.

Return status, what changed, and remaining concerns.
