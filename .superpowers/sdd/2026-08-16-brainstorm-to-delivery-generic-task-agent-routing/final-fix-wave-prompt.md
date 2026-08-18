# Final Fix Wave: Task 5 Skill/Validator Findings

You are the Codex producer for the only allowed final-review fix wave on the
brainstorm-to-delivery generic Task Agent routing branch.

Work in:

`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read these files first:

1. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/final-whole-branch-review.md`
2. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-brief.md`
3. `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
4. `docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md`
5. `.agents/skills/brainstorm-to-delivery/SKILL.md`
6. `/Users/pengchao/.codex/skills/systematic-debugging/SKILL.md`
7. `/Users/pengchao/.codex/skills/test-driven-development/SKILL.md`
8. `/Users/pengchao/.codex/plugins/cache/gf-team/superpowers/6.2.0/skills/writing-skills/SKILL.md`

Own all four Important findings in the final review as one coherent Task 5 fix:

1. Authoritative `validateSimpleDocuments` must fail closed when routing is
   absent. Preserve markerless compatibility only in the lower-level legacy
   parser/Rust warning-only projection; do not add an unverified boolean escape
   hatch that lets a new v2 workflow skip routing.
2. A new empty pending generation boundary is valid only when the entire
   remaining suffix is pending with empty `runs`. Preserve valid historical
   adopted generations. Add the clean-boundary/dirty-later-Task regression.
3. Replace the seven canned-sentence contradiction check with a bounded,
   structural directive strategy that rejects clear ownership/route
   paraphrases, including the review probe. Do not pretend a longer ad hoc list
   is semantic validation. Keep the positive contract authoritative.
4. Make the installed Skill operationally self-contained. Include the exact
   Design review triggers, all six hard triggers, all six weighted soft
   signals, evidence object rules, score threshold/arithmetic, complete Plan
   routing and progress JSON shapes, and the 2 MiB / 256 KiB / 512 KiB / 64 KiB
   bounds. Keep `SKILL.md` imperative and under 500 physical lines. Inline the
   policy unless a bundled required reference is demonstrably cleaner and is
   validated as part of the production contract.

Follow RED-GREEN-REFACTOR. Add focused tests first and record the exact expected
failures before production edits. Also add a totality regression for the
retained `tasks: [null]` Minor if it naturally falls within the generation
validation change; otherwise leave that Minor explicitly recorded.

Run only these checks for this JS/Skill fix wave:

- focused Node regressions for each amended behavior;
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`;
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`;
- Prettier on the changed JavaScript files;
- `git diff --check`.

Do not run any Rust command in this fix wave. Final controller verification
will use only `--no-default-features --features server,test-utils` for Rust.

Append a `Final Fix Wave` section to the existing Task 5 report with root
cause, RED/GREEN commands and exact outputs, changed files, self-review, and
remaining concerns. Do not stage `.superpowers/sdd/**`.

Commit all owned production/tests with a focused message. Preserve unrelated
changes and do not merge, push, or create a PR.

Return only: status, commit hash, one-line test summary, and concerns.
