# Task 5 Fix Round 1

Read the original Task 5 brief and report first:

- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-brief.md`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`

Fix these open review findings with TDD. Append exact RED/GREEN evidence and
commands to the existing Task 5 report, then commit the fix. Do not edit files
outside Task 5 scope.

1. A later Task-Agent generation is accepted only while its boundary Task is
   pending with `runs: []`. After legitimate admission or completion, validation
   rejects the already-adopted generation, deadlocking reviewer dispatch and
   later Tasks. Preserve the empty-pending adoption rule, but accept historical
   adopted boundaries whose frozen progress route still matches the Plan. Add a
   lifecycle test covering pre-admission, admitted/active, reviewer dispatch,
   completed, and the following Task.

2. The authoritative on-disk routing validator silently synthesizes a missing
   `task_agent_generations` field as generation 1 Grok. Require a non-empty
   serialized generation array; omitted invocation selection must be resolved
   before Plan authoring, not reconstructed during Plan validation. Replace the
   current test that locks in fallback behavior.

3. The Design phase lets a user-named Design Reviewer replace the mandatory
   conditional independent Codex Design Reviewer. Always dispatch the Codex
   reviewer when Design review is triggered, then add user-named reviewers as
   separate document-only work units. Add a contradiction/prose regression.

4. Valid maximum Agent/profile tokens can derive work-unit keys longer than the
   canonical 200-byte key limit. Validate every derived route key through the
   same canonical parser/builder constraints and fail before progress or
   delegation. Add maximum-boundary tests for implementer and slotted reviewer
   keys.

5. A completed routed Task accepts terminal `completed` runs without a
   non-empty `task_id` or non-null valid `child_conversation_id`, so fabricated
   never-admitted lineages satisfy completion. Require admission identity for
   every terminal completed expected-key lineage and add negative tests.

6. Remove the obsolete Grok-hard-coded nine-scenario Rust policy matrix instead
   of retaining conflicting legacy routing expectations. Keep only genuinely
   necessary legacy key/session compatibility coverage; the approved eleven v2
   scenarios must be the routing policy matrix.

Covering checks:

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `cargo test --no-default-features --features server,test-utils --test delegation_session_reuse_integration skill_forward_ -- --nocapture`
- `cargo fmt --all -- --check`
- `git diff --check`

Do not run Rust with default features. Every Rust command must include
`--no-default-features --features server,test-utils`. Rust may be deferred until
the source/test changes are complete.

Retained Minor, not part of this fix round: malformed multi-generation progress
with `tasks: [null]` can throw instead of returning deterministic failures.
