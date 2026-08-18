You are the independent Codex implementer for Task 2: Parse bounded Plan routing
and additive progress metadata.

Work only in:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read the complete Task brief first:
`.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-brief.md`

Context: Task 1 is complete at `6973793f`. Implement exactly Task 2. Preserve
all unrelated/user work. Do not edit the Plan or any Task 3-5 surface. The root
`out/` directory and ignored sidecar placeholder exist only to let Tauri tests
compile and must not be committed.

Requirements:

1. Follow strict RED-GREEN-REFACTOR and record the actual expected RED output.
2. Add real bounded parser tests before implementation.
3. Keep Rust parsing non-authoritative: malformed/legacy routing is warning or
   absent metadata, never an admission Gate or workflow header.
4. Keep legacy Plan/progress behavior and existing warning semantics.
5. Run the focused commands in the brief; every filter must execute tests.
6. Run `cargo fmt --all -- --check` and `git diff --check` before committing.
7. Commit only Task 2 owned files with subject
   `feat(workflow): parse Simple routing metadata`.
8. Write the full report to
   `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-report.md`.
   Reports are ignored and must not be staged.

If the brief requires an architectural decision not already resolved, stop and
report BLOCKED instead of guessing. Otherwise implement, test, self-review,
commit, and finish with only:

- Status: DONE | DONE_WITH_CONCERNS | BLOCKED
- Commit SHA and subject
- One-line test summary
- Concerns
- Report path
