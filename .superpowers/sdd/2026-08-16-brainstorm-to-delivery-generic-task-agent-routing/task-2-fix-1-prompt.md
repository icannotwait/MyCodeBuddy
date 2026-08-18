You are the Codex implementer responsible for Task 2 fix round 1.

Work only in:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read these files first:

- Task brief: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-brief.md`
- Existing report: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-report.md`
- Primary review: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-primary-review.md`

The prior Task 2 implementation is commit `6cfd1830`. Address exactly this open
Important finding from the independent primary review:

> `src-tauri/src/acp/delegation/workflow/simple_parse.rs:395`: Marker recognition uses only `starts_with(marker)`, contrary to the exact-marker requirement in `task-2-brief.md:193`. Prefix lookalikes such as `<!-- codeg-b2d-routing-v10` or `<!-- codeg-simple-progress-v1-extra` are counted as v1 blocks. If such a block precedes a real v1 block, the parser selects the lookalike body, emits misleading warnings, and discards valid metadata. Require a documented delimiter boundary after the marker, then add routing and progress tests covering version/prefix lookalikes followed by a valid marker.

The reviewer also reported a Minor about CommonMark backtick info strings. It
is recorded for final triage and is not in this fix round. Do not address it.

Follow strict RED-GREEN-REFACTOR. Add focused tests in
`src-tauri/src/acp/delegation/workflow/simple_parse.rs` first and observe the
expected failure. The tests must exercise both routing and progress lookalike
markers before valid live markers. Then implement the smallest exact-marker
boundary fix while preserving existing inline-space and newline marker forms,
warning ordering, bounds, and legacy behavior.

Run at minimum from `src-tauri/`:

```text
cargo test --lib --features test-utils simple_parse::tests::simple_parse_routing -- --nocapture
cargo test --lib --features test-utils simple_parse::tests::simple_parse_progress -- --nocapture
cargo fmt --all -- --check
```

Run `git diff --check` from the repository root. Every test filter must execute
at least one test. Commit only the Task-owned file with subject:
`fix(workflow): require exact Simple markers`

Append a Fix Round 1 section to the existing Task report with the RED evidence,
covering tests, commands and outputs, commit hash, and self-review. The report is
ignored and must not be staged.

Finish with only:

- Status: DONE | DONE_WITH_CONCERNS | BLOCKED
- Commit SHA and subject
- One-line test summary
- Concerns
- Report path
