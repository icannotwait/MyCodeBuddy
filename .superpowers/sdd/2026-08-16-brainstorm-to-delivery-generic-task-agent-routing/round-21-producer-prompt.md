You are the fresh independent Codex fix producer for Final Fix Round 21 in the
brainstorm-to-delivery generic Task Agent routing work. Work only in:

`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read these files completely before editing:

1. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-brief.md`
2. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-final-fix-round-21-findings.md`
3. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-final-fix-round-20-primary-rereview.md`
4. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-final-fix-round-20-auxiliary-simulated-grok-rereview.md`
5. The existing Round 20 section at the end of
   `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`

Start from exact clean base
`1081eda2b0b24a470d0b591c47920b89c38d77b9`. Verify that before editing.

Treat the Round 21 findings file as the deduplicated binding union. Follow
strict RED-GREEN-REFACTOR: add focused Round 21 tests first, run the focused
filter, and record the expected RED failure for every group before touching
production code. If a new test passes before the fix, strengthen it until it
demonstrates the reported defect. Do not delete, weaken, or relabel an
existing expectation.

You may modify only these two tracked files:

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`

Implement the smallest relation-aware fixes that address the findings and
retain all controls. Do not edit Skill prose, Design, Plan, Rust, progress,
or any other tracked file.

Run:

- the focused Round 21 Node test filter, proving at least one test executed;
- the full validator Node test file;
- the production validator;
- Prettier check for both permitted files;
- `node --check` for both permitted files;
- `git diff --check` and a scope check.

Do not run any Rust command and do not enable default `tauri-runtime`.

Append a complete `## Final Fix Round 21` section to the ignored
`task-5-report.md` with root causes, exact RED and GREEN evidence, verification,
changed files, self-review, and remaining concerns. Commit only the two
permitted tracked files in one focused commit. Do not stage or commit any
`.superpowers/sdd/**` artifact.

Return only: status, commit SHA and subject, one-line test summary, concerns,
and the report path.
