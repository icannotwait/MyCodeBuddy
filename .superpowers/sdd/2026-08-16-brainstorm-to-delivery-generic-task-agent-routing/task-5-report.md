# Task 5 Report

## Result

Shipped the brainstorm-to-delivery Skill contract v2, bounded authoritative
Plan routing validation, additive routed progress agreement, per-key lineage
validation, and the approved eleven Skill-forward routing scenarios.

Commit: 56ce2486 feat(skill): route brainstorm delivery by task risk

The Skill contains one codeg-b2d-skill-contract-v2 block, nine imperative
phases, Grok as the omitted-selection default, invocation-selected Task Agent
generations, independent Design/Plan producers and reviewers, deterministic
normal/high routes, boundary-only Agent changes, owning-producer fixes,
generic recovery rails, and local-only final delivery. SKILL.md is 257 physical
lines.

## TDD Evidence

RED:

- node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
  initially failed at module load because MAX_ROUTING_BLOCK_BYTES and the v2
  pure routing interfaces were absent.
- After the first implementation pass, focused additions separately failed for
  duplicate soft evidence, oversized profiles, and malformed multi-generation
  input before those cases were implemented.

GREEN:

- Node validator suite: 27 passed, 0 failed.
- Production Skill validator: PASS, 0 failures.
- Rust Skill-forward filter: 5 passed, 0 failed, 16 filtered out.
- Rust workflow key filter: 15 passed, 0 failed, 4598 filtered out.
- Rust Simple parse filter: 16 passed, 0 failed, 4597 filtered out.
- Rust Simple projection filter: 24 passed, 0 failed, 4589 filtered out.

## Scenario Coverage

The v2 Rust matrix covers:

1. default normal Grok implementer plus Codex primary;
2. selected non-Grok normal route;
3. high Codex implementer plus Codex primary and Task Agent auxiliary;
4. Codex Task Agent with three distinct keys and children;
5. high fix with both reviewers re-reviewing;
6. separate conditional Design Reviewer and Design Fixer;
7. stable Plan Author with a separate Plan Reviewer;
8. boundary Agent changes for pending Tasks;
9. active-Task Agent changes blocked without handoff;
10. recovery preserving Agent, profile, key, and budgets;
11. final findings returning to normal/high owners and reopening reviews.

The integration setup also proves distinct route keys receive distinct child
conversations for Design producer/reviewer, Plan producer/reviewer, normal and
high Task routes, a Codex/Codex/Codex high route, and final review.

## Commands And Outcomes

- node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
  - PASS: 27 tests.
- node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
  - PASS: production Skill, 0 failures.
- pnpm exec prettier --check on both changed validator files
  - PASS.
- cargo test --no-default-features --features server,test-utils --test
  delegation_session_reuse_integration skill_forward_ -- --nocapture
  - PASS: 5 tests.
- cargo test --no-default-features --features server,test-utils --lib
  workflow::key::tests -- --nocapture
  - PASS: 15 tests.
- cargo test --no-default-features --features server,test-utils --lib
  simple_parse -- --nocapture
  - PASS: 16 tests.
- cargo test --no-default-features --features server,test-utils --lib
  simple_projection_ -- --nocapture
  - PASS: 24 tests.
- cargo check --no-default-features --features server,test-utils --lib
  - PASS.
- cargo clippy --no-default-features --features server,test-utils --lib --
  -D warnings
  - PASS.
- cargo fmt --all -- --check
  - PASS.
- git diff --check
  - PASS.

## Retained Minors And Environment Notes

- Retained Task 2 Minor: the shared Rust extractor accepts a backtick fence
  opener whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: isolated failed/canceled route-locality coverage is
  not explicit.
- The macOS linker emitted its pre-existing compact-unwind size warning during
  library test linking. Tests passed.
- The first Rust verification attempt exhausted the filesystem while linking.
  The generated Task worktree Cargo target was cleaned and rebuilt; a later
  incremental directory cleanup freed space, and all recorded Rust checks then
  passed.
- The skill-creator quick_validate.py helper could not run because its host
  Python environment lacks PyYAML. The repository production validator and
  exact frontmatter/contract tests passed.

## Fix Round 1

### Result

Fixed all six open review findings. Historical Task Agent generations now
remain valid after admission when their frozen progress route matches the
Plan, while new generations still require an empty pending boundary. Routing
validation requires an explicit non-empty serialized generation array and
rejects derived Task keys that fail the canonical key parser. Completed routed
lineages require both task and child-conversation admission identity.

The Design phase now always dispatches the independent Codex Design Reviewer
when review is triggered and treats user-named reviewers as additional
document-only units. The obsolete Grok-hard-coded nine-scenario Rust matrix was
removed; the approved eleven v2 scenarios are the sole routing policy matrix.

Fix commit: 16ee423c fix(skill): harden task agent routing validation

### TDD Evidence

Baseline:

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS before new regressions: 27 passed, 0 failed.

RED:

- The same Node command after adding the first regression set failed exactly
  6 of 31 tests: Design Reviewer substitution, omitted serialized generations,
  overlong implementer key, overlong slotted reviewer key, completed lineage
  without admission identity, and admitted generation lifecycle.
- After adding the stricter pending-boundary admission case, the same command
  failed exactly 1 of 31 tests: `adopts a later generation only at an empty
  pending boundary`.

GREEN:

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: 31 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - PASS: production Skill, 0 failures, 1 check; SKILL.md has 260 lines.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: both files use Prettier style.
- `cargo test --no-default-features --features server,test-utils --test delegation_session_reuse_integration skill_forward_ -- --nocapture`
  - First attempt: build failed before tests with `No space left on device`.
  - Recovery: `cargo clean` removed 9.9 GiB of rebuildable artifacts from this
    worktree only.
  - Retry PASS: 5 passed, 0 failed, 16 filtered out.
- `cargo fmt --all -- --check`
  - PASS.
- `git diff --check`
  - PASS.

### Retained Minor

- As directed by the fix prompt, malformed multi-generation progress with
  `tasks: [null]` may still throw instead of returning deterministic failures;
  this remains outside Fix Round 1.

## Fix Round 2

### Result

Historical generation adoption now requires an admitted run for the boundary
Task's exact implementer work-unit key. Admitted primary-reviewer,
auxiliary-reviewer, and combined reviewer-only runs no longer establish
adoption.

Fix commit: caaae2fe fix(skill): require implementer generation adoption

### TDD Evidence

RED:

- Focused Node regression for reviewer-only historical adoption failed before
  the implementation change: 0 passed, 1 failed. The real-document validator
  returned no failures for an admitted reviewer-only boundary instead of
  `B2D-ROUTING-007`.

GREEN:

- Focused reviewer-only regression: 1 passed, 0 failed.
- Full Node validator suite: 32 passed, 0 failed.
- Production Skill validator: PASS, 0 failures, 1 check.
- Prettier check on both changed validator files: PASS.
- `git diff --check`: PASS.

### Commands And Outcomes

- `node --test --test-name-pattern='rejects historical generation adoption by admitted reviewer-only runs' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - RED before implementation: 0 passed, 1 failed.
  - GREEN after implementation: 1 passed, 0 failed.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: 32 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - PASS: production Skill, 0 failures, 1 check; SKILL.md has 260 lines.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS.
- `git diff --check`
  - PASS.

No Rust command was run in Fix Round 2.

## Final Verification At `caaae2fe`

All Rust test, check, and clippy commands used the binding server-only feature
set `--no-default-features --features server,test-utils`; no default
`tauri-runtime` Rust verification was run.

- Node validator suite: PASS, 32 passed and 0 failed.
- Production Skill validator: PASS, 0 failures and 1 check.
- Prettier check for both validator files: PASS.
- Rust Skill-forward integration filter: PASS, 5 passed and 0 failed.
- Rust workflow key filter: PASS, 15 passed and 0 failed.
- Rust Simple parse filter: PASS, 16 passed and 0 failed.
- Rust Simple projection filter: PASS, 24 passed and 0 failed.
- `cargo check --no-default-features --features server,test-utils --lib`:
  PASS.
- `cargo clippy --no-default-features --features server,test-utils --lib --
  -D warnings`: PASS.
- `cargo fmt --all -- --check`: PASS.
- `git diff --check 896be5f8..HEAD`: PASS.

The first final `cargo check` attempt failed before completion because the
volume had only 196 MiB free. `cargo clean` removed 12.4 GiB of rebuildable
artifacts from this worktree's `src-tauri/target`; the fresh server/test-utils
check and clippy runs then passed. Rust library tests retained the pre-existing
macOS compact-unwind linker warning and completed with zero failures.

## Final Fix Wave

### Result And Root Cause

Resolved all four Important final-review findings as one Task 5 contract fix.
The authoritative document validator had inherited the lower-level parser's
markerless compatibility and therefore skipped routing entirely. Generation
adoption checked only the boundary Task, so a later reserving run could make a
new pending suffix dirty without invalidating adoption. Skill contradiction
resistance compared seven exact sentences instead of bounded directive
structures. The installed Skill depended on branch-only Design/risk/schema
details that were absent from its operational body.

`validateSimpleDocuments` now requires routing without a compatibility flag,
while `parseSimplePlan` and `parseSimpleProgress` still read markerless legacy
documents. New pending generations require every routed Task from the boundary
through the remaining suffix to be pending with `runs: []`; an admitted
implementer at the historical boundary still preserves that generation. The
same indexed-Task path also makes malformed `tasks: [null]` total and resolves
the retained Task 5 Minor.

Contradiction resistance now tokenizes bounded 64-token directive windows with
16-token overlap and checks actor/action/scope, conversation identity,
active-Task switching, and required-review bypass structures. It does not
claim general semantic interpretation; the exact positive v2 JSON contract
remains authoritative. The Skill now carries the exact Design review triggers,
six hard triggers, six weighted soft signals, evidence fields, arithmetic,
complete Plan/progress JSON shapes, and all four byte bounds inline. The Skill
is imperative with 417 physical lines; the production validator reports 418
split lines including the trailing empty line, below 500.

### RED Evidence

Each command ran after the focused tests were added and before any production
file was edited.

- `node --test --test-name-pattern='requires routing authoritatively while preserving markerless parser compatibility' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 1`, `pass 0`, `fail 1`, exit 1.
  - Exact failure: `AssertionError [ERR_ASSERTION]: expected B2D-ROUTING-001; got `.
- `node --test --test-name-pattern='requires the entire pending suffix to have empty runs at a new generation boundary' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 1`, `pass 0`, `fail 1`, exit 1.
  - Exact failure: `AssertionError [ERR_ASSERTION]: expected B2D-ROUTING-007; got `.
- `node --test --test-name-pattern='rejects bounded ownership and route directive paraphrases' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 1`, `pass 0`, `fail 1`, exit 1.
  - Exact failure: `AssertionError [ERR_ASSERTION]: expected B2D-SKILL-005; got `.
- `node --test --test-name-pattern='contains the complete operational policy and document JSON shapes' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 1`, `pass 0`, `fail 1`, exit 1.
  - Exact failure: `AssertionError [ERR_ASSERTION]: missing Skill heading: ### Operational policy JSON`.
- `node --test --test-name-pattern='returns deterministic failures for null Tasks during generation validation' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 1`, `pass 0`, `fail 1`, exit 1.
  - Exact failure: `AssertionError [ERR_ASSERTION]: Got unwanted exception.`
  - Exact cause: `Cannot read properties of null (reading 'index')` at `validate-contract.lib.mjs:1688:22`.

### GREEN Evidence

The five focused commands above each returned exactly `tests 1`, `pass 1`,
`fail 0`, exit 0 after implementation and JavaScript formatting.

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact final result: `tests 36`, `suites 4`, `pass 36`, `fail 0`, exit 0.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact final output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- `pnpm exec prettier --write .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output named both files; exit 0.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run in this Final Fix Wave.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/SKILL.md`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`

Fix commit: 5ddf75b0 fix(skill): close task routing validation gaps

### Self-Review

- Confirmed the authoritative validator has no boolean or caller-selected
  legacy bypass; only lower-level parsing remains markerless-compatible.
- Confirmed the clean-suffix rule applies only to unadmitted generation
  adoption and the admitted-implementer path still accepts active, reviewing,
  completed, and later Tasks on a historical generation.
- Confirmed the review probe and three additional ownership/route paraphrases
  fail, the original seven negative fixtures still fail, and explicit prose
  prohibiting parent Plan writing remains valid.
- Confirmed the inline policy contains all required enumerations, fixed scores,
  evidence shapes, thresholds, arithmetic, JSON fields, and byte values without
  a bundled-reference dependency.
- Confirmed the Task 5 `tasks: [null]` Minor now returns deterministic progress
  failures rather than throwing.
- Confirmed only the three owned production/test files are intended for the
  commit and this `.superpowers/sdd/**` report remains unstaged.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector still accepts a backtick
  opener whose info string contains a backtick, although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The contradiction grammar intentionally recognizes bounded explicit English
  directives rather than arbitrary semantic obfuscation. The exact structured
  positive contract remains the authority for workflow meaning.

## Final Fix Wave Re-review Fix

### Root Cause

The first structural contradiction grammar recognized only actor-first parent
and Task Agent production directives. Its action vocabulary omitted direct
`route` and `delegate` verbs, Task activity omitted `running`, and review
bypass detection omitted `optional`, `omitted`, and past-tense forms. The
required-review check was also limited to the two original review contexts,
so a normal-Task primary-review bypass was accepted.

The bounded grammar now recognizes parent and Task Agent directives in active
and passive word order, direct route/delegation actions, running/current Task
switches, and primary/auxiliary review bypasses. Negated directives remain
permitted. The exact positive v2 contract remains authoritative; this matcher
is intentionally bounded structural validation, not general semantic parsing.

### RED Evidence

Before production edits, all eight concrete probes from the two scoped
re-reviews had individual fixtures. The current-Task switch and six explicit
prohibitions were added as positive controls.

- Command: `node --test --test-name-pattern='rereview' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Exact summary: `tests 15`, `suites 1`, `pass 7`, `fail 8`, `cancelled 0`,
  `skipped 0`, `todo 0`; exit 1.
- Each of the following failed with
  `AssertionError [ERR_ASSERTION]: expected B2D-SKILL-005; got `:
  `passive parent ownership`, `passive Task Agent route`,
  `running Task switch`, `optional auxiliary review`,
  `normal primary bypass`, `omitted auxiliary reviewer`,
  `direct high route`, and `direct high delegation`.
- `current Task switch` and all six
  `rereview permits explicit prohibition ...` controls passed.

### GREEN Evidence

- Focused command: `node --test --test-name-pattern='rereview' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 15`, `suites 1`, `pass 15`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 51`, `suites 4`, `pass 51`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format write: `pnpm exec prettier --write .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output named both JavaScript files; exit 0.
- Format check: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run. `SKILL.md` was not edited and remains 417 physical
lines (`wc -l`), while the production validator reports 418 split lines
including the trailing empty line.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`

Fix commit: ef10695a fix(skill): reject direct routing contradictions

### Self-Review

- Confirmed each of the eight exact review probes returns `B2D-SKILL-005`.
- Confirmed the production Skill still validates with no contradiction failure.
- Confirmed active and passive explicit prohibitions for ownership/routing,
  running/current Task switching, and primary/auxiliary review requirements do
  not produce false positives.
- Confirmed the grammar remains bounded to 64-token directive windows with a
  16-token overlap and uses actor/action/scope structures rather than an
  expanded list of canned sentences.
- Confirmed Design, Plan, and `SKILL.md` were not edited, and only the two
  owned JavaScript production/test files are intended for the commit.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick, although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The contradiction grammar deliberately rejects clear bounded English
  directives and does not claim to detect arbitrary semantic obfuscation.

## Final Fix Wave Round 2

### Result And Root Cause

Resolved the four Important false-positive classes from the authoritative
round-2 primary re-review without changing the Skill or any routing invariant.
The contradiction matcher still treated each bounded directive window mostly
as a bag of actor, action, and target tokens. It therefore attributed a Plan
Author action to the earlier parent, attached remote Codex production verbs to
a Task Agent reviewer, treated `current` as active even after an explicit
completion boundary, and combined unrelated review roles with the first
bypass-like token in the window.

The matcher now recognizes an intervening named document producer, binds a
Task Agent only to its nearest active/passive production attachment, honors an
explicit negated `to the Task Agent` route, recognizes an `after ... Task
completes` boundary, splits semicolon-separated directive clauses, and checks
each review bypass against its nearby required reviewer role and its own
negation. Optional user-named Design/Plan reviewers are treated as the
document-only role defined by the approved Design.

### RED Evidence

All seven compliant statements from the four findings were added as separate
accepted positive controls before production edits.

- Focused command: `node --test --test-name-pattern='round-2' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 7`, `suites 1`, `pass 0`, `fail 7`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - Every control returned
    `[B2D-SKILL-005] Skill prose contradicts required v2 ownership or routing`
    instead of the expected empty failure list.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 58`, `suites 4`, `pass 51`, `fail 7`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The only failures were the seven new `round-2 accepts compliant ...`
    controls; all prior contradiction and explicit-prohibition tests passed.

The first minimal implementation pass made six of seven focused controls pass.
The remaining parent-orchestration control exposed the same root cause at a
second level: `author` was both a production verb and the role noun in `Plan
Author`. Including the complete producer phrase in the intervening-role range
made the focused result `tests 7`, `pass 7`, `fail 0`.

### GREEN Evidence

- Focused round-2 command: `node --test --test-name-pattern='round-2' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 7`, `suites 1`, `pass 7`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Prior-regression command: `node --test --test-name-pattern='rereview' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 15`, `suites 1`, `pass 15`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 58`, `suites 4`, `pass 58`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`

Fix commit: 7173d031 fix(skill): scope contradiction matching by role

### Self-Review

- Confirmed all seven exact compliant probes now return no failures.
- Confirmed all nine prior contradiction probes still return `B2D-SKILL-005`
  and all six explicit-prohibition controls remain accepted.
- Confirmed the production Skill remains accepted and `SKILL.md`, Design,
  Plan, review reports, and controller ledger were not edited.
- Confirmed the change is confined to contradiction prose classification;
  fail-closed document validation, routing parsing, generation adoption,
  risk derivation, route agreement, and lineage validation are untouched.
- Confirmed only the two tracked producer-owned JavaScript files are intended
  for the commit; this report remains ignored and unstaged.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The contradiction matcher remains intentionally bounded structural English
  validation; the exact embedded v2 contract remains authoritative.

## Final Fix Wave Round 3

### Result And Root Cause

Resolved the four bidirectional Important classes from the authoritative
Round-3 primary review, the two explicit boundary probes from the auxiliary
workflow test double, both primary regression-control sentences, and all four
controller pressure controls.

The common root cause was architectural within the contradiction matcher: it
still searched bounded token windows with independent actor/action/scope
helpers, then repaired misattachments through local exemptions. An actor name,
negation, review noun, or completion token could therefore affect an unrelated
predicate in the same clause. Adding another phrase-specific exception would
have preserved that failure mode.

The matcher now parses each bounded clause into actor spans, typed actions,
Task mentions and scopes, and reviewer mentions. Conflict checks attach active
and passive actors to predicates, attach `to`/`by` targets and their local
negation across conjunction/contrast boundaries, keep action-local Task scope,
resolve a coordinated `them` target, distinguish review attachments from
production attachments, model delegation through an orchestrator action plus
producer infinitive, and classify active/completed timing and review bypasses
against their own targets. The 64-token windows and 16-token overlap remain.

### RED Evidence

All 20 Round-3 controls were added before any production edit: ten accepted
statements and ten rejected statements.

- Focused command: `node --test --test-name-pattern='round-3' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 20`, `suites 1`, `pass 2`, `fail 18`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The only initially correct controls were the two semicolon-separated
    review cases. Nine accepted controls returned `B2D-SKILL-005`, and nine
    rejected controls returned no failure.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 78`, `suites 4`, `pass 60`, `fail 18`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - All 58 pre-existing tests passed; only the 18 expected new controls failed.

The first structural implementation pass produced `tests 20`, `pass 19`,
`fail 1`: the Task Agent actor span ended exactly where its following active
verb began, exposing an exclusive-end comparison error. Correcting that span
attachment made all 20 focused controls pass. The first full run then exposed
a production-Skill clause containing both Task Agent and Codex producer roles;
binding an active predicate to the nearest explicit actor, rather than any
earlier Task Agent, restored production acceptance without a prose exemption.

### GREEN Evidence

- Round-3 command: `node --test --test-name-pattern='round-3' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 20`, `suites 1`, `pass 20`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Round-2 command: `node --test --test-name-pattern='round-2' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 7`, `suites 1`, `pass 7`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Prior-regression command: `node --test --test-name-pattern='rereview' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 15`, `suites 1`, `pass 15`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 78`, `suites 4`, `pass 78`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`

Fix commit: bf41df66 refactor(skill): attach contradiction grammar relations

### Self-Review

- Confirmed all 20 exact Round-3 controls classify as required.
- Confirmed the 7 Round-2 accepted controls, 9 prior contradiction probes,
  and 6 explicit-prohibition controls retain their classifications.
- Confirmed the production Skill validates and no Skill prose, Design, Plan,
  review report, or controller ledger was edited.
- Confirmed only contradiction prose parsing changed. Authoritative
  fail-closed routing, generation adoption, risk and route derivation,
  Plan/progress agreement, and lineage validation remain untouched.
- Confirmed the refactor replaces the previous exemption stack with one
  clause-local relation model shared by ownership, route, timing, and reviewer
  classification.
- Confirmed only the two tracked producer-owned JavaScript files are intended
  for the commit; this ignored report remains unstaged.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The matcher remains a bounded structural English classifier, not a general
  semantic parser; the exact embedded v2 contract remains authoritative.

## Final Fix Wave Round 3 Reviewer Attachment Follow-up

### Result And Root Cause

Resolved the accepted bidirectional reviewer-attachment finding with four
test-first controls. `nearestReviewTarget` searched the entire bounded clause
and always preferred the first reviewer after a bypass token. That crossed an
`and` coordination boundary, so `optional` on user-named Design reviewers was
attached to a later Codex reviewer, while `optional` on a Codex reviewer was
attached to later user-named Design reviewers.

The matcher now resolves review targets inside the bypass token's coordinated
predicate segment first. A copular or `remain` predicate binds a following
state to the preceding reviewer subject; otherwise a following reviewer stays
the target of a direct bypass/replacement action. Only a predicate without a
local reviewer falls back to its preceding coordinated subject. This keeps the
fix role- and relation-based rather than sentence-specific.

### RED Evidence

All four exact pressure controls were added before the production edit.

- Focused command: `node --test --test-name-pattern='round-3 reviewer attachment' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 4`, `suites 1`, `pass 0`, `fail 4`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - Both accepted controls returned
    `[B2D-SKILL-005] Skill prose contradicts required v2 ownership or routing`;
    both rejected controls reported `expected B2D-SKILL-005; got` with no
    failure code.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 82`, `suites 4`, `pass 78`, `fail 4`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - All 78 pre-existing tests passed; only the four new reviewer-attachment
    controls failed.

### GREEN Evidence

- Focused command: `node --test --test-name-pattern='round-3 reviewer attachment' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 4`, `suites 1`, `pass 4`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 82`, `suites 4`, `pass 82`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
  (ignored and not staged)

Fix commit: 20f19035 fix(skill): attach review bypasses to reviewer subjects

### Self-Review

- Confirmed both compliant controls return no failures and both bypass controls
  return `B2D-SKILL-005`.
- Confirmed all 78 prior tests retain their classifications, including prior
  contradiction rejections, explicit-prohibition acceptances, and Round-2 and
  Round-3 controls.
- Confirmed the production Skill remains accepted at 418 lines; Skill prose,
  Design, Plan, review reports, and the controller ledger were not edited.
- Confirmed authoritative fail-closed validation and all v2 routing,
  generation, risk, agreement, and lineage invariants are untouched.
- Confirmed the earlier auxiliary simulated Grok report remains explicitly a
  workflow test double only, not a real Grok verdict.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The matcher remains a bounded structural English classifier, not a general
  semantic parser; the exact embedded v2 contract remains authoritative.

## Final Fix Wave Round 4

### Result And Root Cause

Resolved all eight Important relation classes from the authoritative Round-4
primary re-review and the auxiliary simulated Grok workflow test double. The
auxiliary report is a workflow test double only, not a real Grok verdict.

The common root cause was that the bounded parser had typed actors, actions,
Tasks, and reviewers but still attached them through nearest-token rules that
treated every coordinator as a hard predicate boundary. This lost producer
subjects across coordinated infinitives, shared Task scopes and passive
actors, route purpose and reviewer slots, inactive/completed Task state,
replacement antecedents, and the polarity of required review.

The corrected relation model now groups coordinated actions only when they
share a subject/object relation, carries local Task scope and affirmative
passive actors across that group, retains coordinated actor subjects, and
evaluates normal/high production and review roles in both directions. Route
targets are classified by production versus review purpose and primary versus
auxiliary slot. Parent orchestration accepts direct/route delegation to named
document producers without granting parent ownership. Timing attaches
affirmative active, negated inactive, and completed states to each Task with
active-state precedence. Review relations resolve predicative and
parenthetical states, pronoun/passive antecedents, required polarity,
avoid/refuse negation, and `in place of` substitution.

### RED Evidence

All 24 exact accepted/rejected sentences from both reports were added before
the production edit; there were no literal duplicates.

- Initial focused command: `node --test --test-name-pattern='round-4' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 24`, `suites 1`, `pass 0`, `fail 24`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - All nine accepted controls returned `B2D-SKILL-005`; all 15 rejected
    controls returned no contradiction failure.
- The first exact-control GREEN run was `tests 24`, `pass 24`, `fail 0`.
  The first full run then reported `tests 106`, `pass 105`, `fail 1`: the
  existing `Task Agent implementation after Codex exclusion` control exposed
  an affirmative passive target after `but` being erased by predicate-level
  negation. Relation-local passive negation restored that control, after which
  the full suite reported `tests 106`, `pass 106`, `fail 0`.
- Nearby pressure initially reported `19/20 correct`. The one failure routed a
  high Task primary review to Grok. That sentence was added as an automated
  control before the slot-aware production correction.
- Nearby-control focused RED: `tests 25`, `suites 1`, `pass 24`, `fail 1`,
  `cancelled 0`, `skipped 0`, `todo 0`; exit 1. The sole failure reported
  `expected B2D-SKILL-005; got` for the primary-review route.

### GREEN Evidence

- Focused command: `node --test --test-name-pattern='round-4' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 25`, `suites 1`, `pass 25`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 107`, `suites 4`, `pass 107`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.
- Nearby relation pressure: `nearby pressure: 20/20 correct`; exit 0.

No Rust command was run.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
  (ignored and not staged)

Fix commit: 899e961c fix(skill): bind coordinated directive relations

### Self-Review

- Confirmed all 24 exact report controls and the additional primary-slot
  pressure control classify as required.
- Confirmed every prior contradiction rejection, explicit-prohibition
  acceptance, and Round-2/Round-3 control remains green in the 107-test suite.
- Confirmed production Skill acceptance at 418 lines and did not edit Skill
  prose, Design, Plan, review reports, or the progress ledger.
- Confirmed the change is confined to the bounded contradiction relation
  model. Authoritative fail-closed validation and every v2 routing,
  generation, risk, agreement, and lineage invariant remain unchanged.
- Confirmed only the two tracked producer-owned JavaScript files are intended
  for the commit; this report remains ignored and unstaged.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator deliberately implements a bounded structural directive
  grammar, not unrestricted semantic inference; the embedded v2 contract
  remains authoritative.

## Final Fix Wave Round 6

### Finding Disposition And Root Cause

Verified all four Important classes in the authoritative Round-5 primary
review and all four Important classes in the auxiliary simulated Grok
workflow test double against the approved Design, Plan, and production Skill.
The auxiliary report is a workflow test double only, not a real Grok verdict.
All 31 exact controls are valid: 26 directives contradict the approved parent
ownership, normal/high Task route, completed-boundary, or mandatory Codex
review invariants, while five controls preserve approved producer ownership,
completed boundaries, and explicit replacement prohibitions. No technical
ruling rejected or modified a reported case.

The shared root cause was relation collapse. The bounded matcher used one
nearest-token segment for independent grammatical relations, so a delegated
infinitive could lend ownership to a later finite parent predicate; a Task
scope or passive actor could be lost or merged across predicate coordination;
review purpose and slot could detach from a route target; completion tokens
could override temporal direction; and a replacement pronoun could attach to
its local optional subject instead of the mandatory reviewer antecedent.

The corrected bounded model keeps those relations separate. It distinguishes
bare coordinated producer infinitives from finite parent predicates and
explicitly repeated producer subjects; carries predicate-local Task scopes and
shared passive actors across conjunction and contrast without collapsing
mixed normal/high scopes; binds each route action to its own targets, review
purpose, and complete primary/auxiliary slot; evaluates active, completed, and
pre-completion timing on either side of the switch with active-state
precedence; and carries only typed reviewer antecedents across adjacent
semicolon/sentence clauses. Replacement objects now cover plural and
demonstrative pronouns, passive replacement, `instead of`, `in the place of`,
`stand in for`, and `take the place of`, while local negation and explicit
prohibitions remain authoritative. The 64-token windows and 16-token overlap
remain bounded.

### RED Evidence

All 31 exact report controls were added before the production edit; there were
no literal duplicates.

- Focused command: `node --test --test-name-pattern='round-6' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 31`, `suites 1`, `pass 0`, `fail 31`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - All five accepted controls returned `B2D-SKILL-005`; all 26 rejected
    controls returned no contradiction failure.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 138`, `suites 4`, `pass 107`, `fail 31`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - All 107 pre-existing tests passed; only the 31 new exact controls failed.
- Nearby relation RED: `node --test --test-name-pattern='nearby relation pressure' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 2`, `suites 1`, `pass 1`, `fail 1`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The coordinated legal high implementation/auxiliary-review routes were
    initially merged into one target relation.

### GREEN Evidence

- Focused command: `node --test --test-name-pattern='round-6' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 31`, `suites 1`, `pass 31`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Nearby command: `node --test --test-name-pattern='nearby relation pressure' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 2`, `suites 1`, `pass 2`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 140`, `suites 4`, `pass 140`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.
- Manual accepted/rejected pressure probe: `nearby pressure: 23/23 correct`;
  exit 0.

No Rust command was run.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
  (ignored and not staged)

Fix commit: bb5d52d608df8d9b0de982c9aa90e1043d9c115d fix(skill): bind directive relations by clause

### Self-Review

- Confirmed every exact sentence from both Round-5 reports classifies as
  required, plus two automated and 23 manual nearby accepted/rejected
  pressure controls.
- Confirmed all earlier contradiction rejections, explicit-prohibition
  acceptances, and Round-2 through Round-4 controls remain green in the
  140-test suite.
- Confirmed the production Skill remains accepted at 418 physical lines and
  did not edit Skill prose, Design, Plan, review reports, progress, Rust, or
  unrelated files.
- Confirmed the change is confined to the bounded contradiction relation
  model. Authoritative fail-closed validation, Grok default selection, exact
  v2 route derivation, generation boundaries, risk arithmetic, Plan/progress
  agreement, and lineage validation remain unchanged.
- Confirmed the commit contains only the two tracked producer-owned validator
  files; this report remains ignored and unstaged.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the embedded v2 contract remains authoritative.

### Controller Passive-Actor Follow-Up

#### Root Cause

Verified `High Tasks are reviewed by Codex but implemented by Grok.` against
the approved Design Task-route table, Plan global and embedded routing
contracts, and production Skill routing policy. Grok is the default Task Agent,
so assigning it high-Task implementation contradicts the required Codex
implementer route.

The direct passive actors were already predicate-local, but the matcher used
that actor-sensitive predicate group to inherit Task scope. The intervening
`Codex` actor therefore prevented the second coordinated predicate from
inheriting the leading `High Tasks` scope. The fix gives Task-scope sharing its
own bounded `and`/`but` relation while retaining the actor-sensitive group for
passive-subject sharing.

#### RED Evidence

- Focused command: `node --test --test-name-pattern='explicit passive actors' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 1`, `suites 1`, `pass 0`, `fail 1`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - Exact failure: `expected B2D-SKILL-005; got` for the controller sentence.
- Nearby command: `node --test --test-name-pattern='passive actor relation pressure' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 5`, `suites 1`, `pass 4`, `fail 1`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The analogous `and` high-Task/Grok implementation control failed; the
    remaining four accepted/rejected controls retained their expected result.

#### GREEN Evidence

- Focused command: `node --test --test-name-pattern='explicit passive actors' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 1`, `suites 1`, `pass 1`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Nearby command: `node --test --test-name-pattern='passive actor relation pressure' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 5`, `suites 1`, `pass 5`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command: `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 146`, `suites 4`, `pass 146`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command: `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command: `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run.

#### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
  (ignored and not staged)

Fix commit: 6c2e0ca4a688dacd04b87826ddb89e3dd6fa92a4 fix(skill): preserve task scope across passive actors

#### Self-Review

- Confirmed the exact controller sentence and the analogous `and` form now
  reject while compliant high and normal routes with explicit actors on both
  sides of conjunction/contrast remain accepted.
- Confirmed the inverse normal Codex-implementation pressure control still
  rejects and all 140 pre-existing tests remain green.
- Confirmed passive actors remain predicate-local; only Task-scope inheritance
  uses the new relation group, so actor ownership is not merged.
- Confirmed authoritative fail-closed v2 validation and all routing,
  generation, risk, progress, and lineage invariants are unchanged.
- Confirmed the commit contains only the two tracked producer-owned validator
  files. Design, Plan, Skill prose, review reports, progress, Rust, and
  unrelated files were not edited; this report remains ignored and unstaged.

#### Remaining Concerns

- The previously recorded Task 2 and Task 4 Minors remain unchanged.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the embedded v2 contract remains authoritative.

### Round-8 Coordinated Actors And Exact Reviewer Set Follow-Up

#### Root Cause

The passive/direct-route relation parser selected only the first actor after a
`by` or `to` link. A valid actor first in a coordinated list therefore hid
later contradictory producers, reviewers, or route targets. Separately, review
validation recognized a route's broad purpose but did not bind primary and
auxiliary slots to individual actors or enforce explicit reviewer-set and
cardinality assertions.

The fix binds every actor up to the next action or actor link, preserves each
predicate's own actor relation, and associates preposed or postposed reviewer
slots with the correct actor/target. Normal and high review statements now
enforce their exact actor, slot, explicit absence, and explicit numeric
cardinality constraints while retaining partial legal review statements and
explicit prohibitions.

#### RED Evidence

- Initial focused command:
  `node --test --test-name-pattern='round-8' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 42`, `suites 1`, `pass 19`, `fail 23`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - All 19 Round-7 report contradictions were accepted. Eleven complete
    coordinated-list controls were also accepted, while both legal two-link
    reviewer-order controls exposed the existing global-purpose binding.
- Exact-set follow-up RED:
  `node --test --test-name-pattern='round-8 exact reviewer set' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 14`, `suites 1`, `pass 4`, `fail 10`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - Explicit missing-primary, zero/one/extra reviewer, and missing required
    Codex/Task-Agent role controls all failed open.

#### GREEN Evidence

- Focused command:
  `node --test --test-name-pattern='round-8' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 56`, `suites 1`, `pass 56`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full command:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 202`, `suites 4`, `pass 202`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.
- Independent exact Round-7 report pressure matrix:
  `round-8 report pressure matrix: 29/29 correct`; exit 0.

No Rust command was run.

#### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
  (ignored and not staged)

Fix commit: e660f404cef1ab4d0fd552eb24df75cdad821fb2 fix(skill): enforce exact task route relations

#### Self-Review

- Confirmed all 19 Round-7 contradiction examples reject, all report-adjacent
  legal order/role controls accept, and comma/duplicate/extra-target forms are
  order-independent.
- Confirmed normal review permits only one Codex primary reviewer and high
  review permits Codex primary plus Task Agent auxiliary in either order,
  including explicit two-link routes and the Task-Agent-as-Codex role model.
- Confirmed explicit prohibitions remain accepted and explicit absence/count
  statements fail closed only when they contradict the scoped normal/high
  reviewer set.
- Confirmed all prior contradiction, routing, generation, risk, progress, and
  lineage controls remain green, and production Skill prose remains unchanged.
- Confirmed the commit contains only the two tracked producer-owned validator
  files. Design, Plan, Skill prose, review reports, progress, Rust, and
  unrelated files were not edited; this report remains ignored and unstaged.

#### Remaining Concerns

- The previously recorded Task 2 and Task 4 Minors remain unchanged.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the embedded v2 contract remains authoritative.

### Round-9 Final Independent Re-review Follow-Up

#### Root Cause And Scope

The two Round-8 rejection reports exposed ten overlapping structural gaps.
Producer infinitives did not distinguish ordinary `or`/Oxford coordination
from a later finite parent predicate; pre-completion timing attached `before`
to a completion on the wrong side; reviewer replacement, Task scope, document
targets, and active state lost bounded antecedents; and actor polarity,
purpose, slot, and cardinality were not local to each relation. Historical
generation admission also stopped checking the cleanliness of future pending
Tasks.

The fix keeps those relations bounded to directive windows and local clauses,
with one immediately prior clause carried only for typed Task, document, and
reviewer antecedents. It assigns polarity, review purpose, and reviewer slots
per actor relation; permits the same Codex identity in distinct implementer
and primary-review roles; recognizes bounded reviewer count/absence variants
and concrete Agent switch names; and requires every future pending Task to
retain an empty run list after generation-boundary admission. No exact report
sentence regex was added.

#### RED Evidence

- Pre-change full command:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 202`, `suites 4`, `pass 202`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Pre-change independent union probe:
  - Exact result: `round-8 union prose pressure: 0/38 correct`; exit 1.
- Focused command after adding Round-9 regressions and before production edits:
  `node --test --test-name-pattern='round-9' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 10`, `suites 2`, `pass 0`, `fail 10`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
- Initial full-suite regression check after the focused implementation:
  - Exact summary: `tests 212`, `suites 4`, `pass 209`, `fail 3`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - One shared-passive-actor rejection and two legal coordinated reviewer-slot
    controls exposed relation polarity and between-actor slot attachment
    regressions. The structural bindings were corrected before final
    verification.

#### GREEN Evidence

- Final focused command after formatting:
  `node --test --test-name-pattern='round-9' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 10`, `suites 2`, `pass 10`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Final full command after formatting:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 212`, `suites 4`, `pass 212`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Independent exact union of both Round-8 reviewers' unique prose probes:
  - Exact result: `round-8 union prose pressure: 38/38 correct`; exit 0.
- Production command:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.

No Rust command was run.

#### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-report.md`
  (ignored and not staged)

Fix commit: 2d7467ab8c578a917d5ecfbc1d496cb0f3a48abf fix(skill): close final routing review gaps

#### Self-Review

- Confirmed the exact 38 unique rejection-report prose cases now classify
  correctly and 21 additional neighboring Round-9 prose controls preserve
  legal coordination, negation, reviewer ordering, clause antecedents, and
  completed-Task boundaries.
- Confirmed the post-admission dirty future suffix rejects with
  `B2D-ROUTING-007`, while the otherwise identical clean suffix validates.
- Confirmed all 202 pre-existing tests remain green alongside the ten new
  grouped regression tests.
- Confirmed the commit contains only the two tracked producer-owned validator
  files. Design, Plan, Skill prose, review reports, progress, Rust, and
  unrelated files were not edited; this report remains ignored and unstaged.
- Confirmed `git status --short --untracked-files=all` has no tracked or
  untracked output after the commit; the report is excluded by the existing
  `.superpowers` ignore rule.

#### Remaining Concerns

- The previously recorded Task 2 CommonMark fence Minor remains unchanged.
- The previously recorded Task 4 failed/canceled projection-locality Minor
  remains unchanged.
- The validator intentionally remains a bounded structural English classifier,
  not a general semantic parser; the embedded v2 contract remains
  authoritative.

## Final Fix Round 10

### Result And Root Cause

Resolved the complete union of the seven primary and seven auxiliary Round-9
Important findings without changing Skill prose or the structured routing,
generation, progress, or lineage validators.

The bounded contradiction parser had seven connected binding gaps: concrete
and custom Task Agent names were not normalized to the Task Agent role;
alternative polarity and coordinated route purposes were relation-global;
reviewer absence, surplus cardinality, and replacement antecedents were
incomplete; active/completed timing and document/reviewer antecedents lost
their typed prior-clause state; and active/passive parent delegation did not
distinguish the producer subject from the coordinator. The fix normalizes
built-in/custom Agent spans, including Task-Agent Codex; binds actor polarity,
implementation/review purpose, reviewer slot, and absence locally; carries
typed Task/document/reviewer antecedents; evaluates completion timing by its
temporal segment; and recognizes both active and passive producer delegation.

The 14 Round-10 groups cover every exact report sentence and neighboring
accepted/rejected matrices, including multi-token Agent aliases, explicit
document/artifact objects, custom active switches, duplicate and postposed
reviewer omissions, Task-Agent Codex review identity, and single-link mixed
implementation/review routes.

### RED Evidence

- Initial focused command after adding all 14 finding groups and before any
  validator edit:
  `node --test --test-name-pattern='round-10' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 14`, `suites 1`, `pass 0`, `fail 14`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - Each group failed on its first expected classification mismatch, covering
    all 14 Round-9 Important findings.
- Neighboring Agent-alias/Task-Agent-Codex RED after adding the final
  self-review controls:
  `node --test --test-name-pattern='round-10 rejects concrete|round-10 rejects incomplete' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 2`, `suites 1`, `pass 0`, `fail 2`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.

### GREEN Evidence

- Focused Round-10 suite after final formatting:
  `node --test --test-name-pattern='round-10' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 14`, `suites 1`, `pass 14`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full Node suite after final formatting:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 226`, `suites 4`, `pass 226`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier write and final check on both owned JavaScript files:
  - Final check output: `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.
- No Rust command was run, as required for this fix wave.

### Commit And Scope

Fix commit:
`943bfc291e7fa30d49c94b845e3528ba415a85a3 fix(skill): bind generic task routing directives`

- The commit contains only
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  and
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`.
- `git diff --cached --check` passed before commit.
- After commit, `git diff --exit-code` and `git diff --cached --exit-code`
  both returned exit 0, and
  `git status --short --branch --untracked-files=all` showed only the branch
  header. This ignored report remains unstaged.
- Design, Plan, Skill prose, progress, review reports, Rust, root checkout,
  and unrelated files were not edited.

### Self-Review And Residual Concerns

- Confirmed the exact Round-9 union and adversarial neighboring controls
  preserve generic Task Agent selection, Grok default semantics, Codex primary
  review, high-route Codex implementation plus Task Agent auxiliary review,
  boundary-only Agent changes, parent coordination-only ownership, and
  explicit-prohibition polarity.
- Confirmed prior Round-2 through Round-9 prose controls and every structured
  routing/progress/lineage test remain green in the 226-test suite.
- The previously recorded Task 2 CommonMark fence Minor remains unchanged.
- The previously recorded Task 4 failed/canceled projection-locality Minor
  remains unchanged.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 11

### Result And Root Cause

Resolved the deduplicated union of both Round-10 re-reviews: all six open
primary invariant classes, both new primary regressions, all four open
auxiliary invariant classes, and all four auxiliary regression groups. Skill
prose and the structured routing, risk, generation, progress, and lineage
validators were not changed.

The remaining failures shared bounded relation-binding causes. Actor links
discarded otherwise valid qualified Task Agent spans; alternative polarity and
postposed absence did not remain attached to their grammatical relation;
plural artifact and people antecedents were reduced to the presence of any
known actor; completion timing was evaluated after raw active state; and
reviewer cardinality/replacement logic treated isolated words such as
`another`, `missing`, and `take*` as global assertions. Plain `Codex Agent` was
also collapsed into the selected `Codex Task Agent` role.

The fix now validates bounded actor prefixes by structural boundaries, carries
typed plural document/people antecedents, propagates alternatives across
coordinated and repeated links, orders completion and later active-state
relations, binds exhaustive/cardinality/absence terms to their reviewer noun
phrases, and recognizes only complete take-over/take-the-place replacement
constructions. Bare Codex remains the Codex role; only explicit `Codex Task
Agent` selects the Task Agent role.

### RED Evidence

- Focused command before any production edit:
  `node --test --test-name-pattern='round-11' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 12`, `suites 1`, `pass 0`, `fail 12`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The aggregated assertions reported 30 expected classification mismatches
    across qualified actors, carried completion, reviewer-role antecedents,
    surplus cardinality, coordinated/repeated-link alternatives, typed
    documents/people, explicit active-completion order, exhaustive high review,
    Codex Agent identity, take-over versus unrelated takes, reviewer-bound
    another, and postposed missing evidence.
- Additional typed-role control before its production edit:
  `node --test --test-name-pattern='round-11 keeps typed document' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 1`, `suites 1`, `pass 0`, `fail 1`; exit 1.
  - Exact mismatch: expected acceptance for parent communication to the
    Document Producer role.

### GREEN Evidence

- Focused command:
  `node --test --test-name-pattern='round-11' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 12`, `suites 1`, `pass 12`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full Node command:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 238`, `suites 4`, `pass 238`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production command:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Format command:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact output: `Checking formatting...`,
    `All matched files use Prettier code style!`; exit 0.
- `git diff --check`
  - Exact output: none; exit 0.
- Committed base-to-HEAD classification matrix loaded the validator at
  `2d7467ab`, `943bfc29`, and `HEAD` with `node --input-type=module`.
  - Exact result: `round-11 committed regression matrix: 12/12 correct`;
    every regression was correct at the base, wrong at Round-10 HEAD, and
    restored at Round-11 HEAD.

No Rust command was run and no default `tauri-runtime` feature was enabled.

### Commit And Scope

Fix commit:
`94ba94f92b914b1dec4b5eb7833146bea28d1c33 fix(skill): bind remaining routing relations`

- The commit contains only
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  and
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`.
- This report is ignored and remained unstaged. Design, Plan, Skill prose,
  progress, review reports, Rust, root checkout, and unrelated files were not
  edited.
- `git diff --cached --check`, committed-file inspection, working-tree diff,
  and index diff checks all returned exit 0.

### Self-Review And Residual Concerns

- Confirmed every exact failing probe from both Round-10 reports is an
  automated Round-11 control, with neighboring legal/illegal cases for all
  requested disambiguation classes.
- Confirmed the two earlier full-suite regressions found during implementation
  were relation-order issues, not expectation changes: explicit reviewer
  objects still beat anaphoric role fallback, and a later active-state
  assertion still overrides an earlier completion clause.
- Confirmed the twelve reported fix-diff probes reproduce the claimed
  base-to-Round-10 changes and all twelve return to their base classification
  at the committed Round-11 HEAD.
- Confirmed the authoritative v2 contract, Grok default, route derivation,
  risk arithmetic, generation boundaries, Plan/progress agreement, and
  per-key lineage behavior remain unchanged.
- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator intentionally remains a bounded structural English classifier,
  not a general semantic parser; the embedded v2 contract remains
  authoritative.

## Final Fix Round 12

### Result And Root Cause

Resolved the deduplicated Critical/Important union from both Round-11
re-reviews. The auxiliary report was treated only as a simulated Grok workflow
test double, not as a real Grok verdict. Skill prose and the structured
routing, risk, generation, progress, and lineage validators were not changed.

The remaining failures came from eight relation-binding roots. Passive actor
collection attached later nominal `by` relations and nested instruction
sources to the production predicate. Completion terms were associated by
distance instead of by their Task subject. `take` replacement handling did
not distinguish role-taking from note-taking, and generic role determiners
overrode explicit advisory-role qualifiers. Reviewer surplus markers were
discarded without checking whether they introduced the complementary route
slot. Alternative polarity leaked beyond a positive contrast and did not
cover a repeated predicate. Plural artifact objects overrode plural people
subjects and recipients. Review quantifiers were treated as clause-global,
and postposed reviewer absence depended on a closed modifier list.

The corrected bounded model now binds passive actors only to the governing
predicate relation, stops at nested/subordinate relation boundaries, and
tracks alternative scope through repeated predicates and positive contrast
resets. It associates completion with the Task noun phrase or an unambiguous
carried Task subject, resolves people and artifacts by grammatical role,
recognizes complete `take over` and `take (on) the role of` constructions,
checks surplus markers against the other explicit reviewer slot in either
order, binds exclusivity to a reviewer relation or reviewer noun phrase, and
binds postposed absence through the reviewer's own copular predicate.

Fix commit:
`e72a5f8345d238ad30ed4f7d966c18a9c868bc17 fix(skill): bind task routing relation scopes`

### RED Evidence

- Initial focused command after adding all eight Round-12 regression and
  neighboring-control groups, before production edits:
  `node --test --test-name-pattern='round-12' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 8`, `suites 1`, `pass 0`, `fail 8`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The eight aggregated assertions contained 25 classification mismatches
    across passive actor ownership, Task completion, reviewer replacement,
    reviewer surplus, alternative polarity, plural antecedents, review
    exclusivity, and postposed reviewer absence.
- Open-modifier completion neighbor RED:
  `node --test --test-name-pattern='round-12 binds completion|round-12 binds postposed reviewer absence' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 2`, `suites 1`, `pass 1`, `fail 1`; exit 1.
  - `After completion of the currently active Task` was still rejected until
    the Task noun-phrase relation replaced the closed modifier set.
- Same-clause completion and reverse reviewer-order RED:
  `node --test --test-name-pattern='round-12 binds completion|round-12 treats qualified' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 2`, `suites 1`, `pass 0`, `fail 2`; exit 1.
  - Two component-completion controls failed open, and the reverse-order
    complementary reviewer control failed closed before those relations were
    made subject- and order-aware.

### GREEN Evidence

- Focused Round-12 command after final formatting:
  `node --test --test-name-pattern='round-12' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 8`, `suites 1`, `pass 8`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact summary: `tests 246`, `suites 4`, `pass 246`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact output: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check on the two owned validator files:
  - Exact output: `All matched files use Prettier code style!`; exit 0.
- `git diff --check` and `git diff --cached --check`:
  - Exact output: none; both exited 0.
- Committed/base differential pressure matrix loaded both validator libraries
  from Git objects at `94ba94f92b914b1dec4b5eb7833146bea28d1c33`
  and `e72a5f8345d238ad30ed4f7d966c18a9c868bc17`.
  - Exact result: `base 0/23`, `committed 23/23`, `changed 23/23`.
  - The 23 unique probes cover every deduplicated Round-11
    Critical/Important report sentence.

No Rust command was run in Final Fix Round 12. No default `tauri-runtime`
feature was enabled.

### Changed Files And Scope

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended and was not staged or committed.

The commit contains only the two tracked validator files. Design, Plan,
`SKILL.md` prose, progress, prior review reports, Rust, the root checkout, and
unrelated files were not edited.

### Self-Review

- Confirmed all 23 unique report probes change from the wrong base
  classification to the required committed classification.
- Confirmed the Round-12 automated controls cover both accepted and rejected
  neighbors for every root, including arbitrary Task noun-phrase modifiers,
  same-clause component completion, both reviewer orders, positive contrast
  reset, people subjects/recipients, scope-limited quantifiers, and negated
  absence.
- Confirmed all 238 pre-existing tests remain green alongside the eight new
  Round-12 groups.
- Confirmed Simple manifest-free behavior, Grok default selection, Codex-only
  document producers/reviewers, normal/high Task routes, serial Task order,
  boundary-only Agent changes, owning-producer final fixes, and generic
  recovery behavior remain unchanged.
- Confirmed the final commit contains only the two producer-owned tracked
  files and the ignored report remains outside the index.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled route-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 13

### Result And Root Cause

Resolved the deduplicated union of all seven primary and three auxiliary
Round-12 Important regression groups. The auxiliary report was treated only
as a simulated Grok workflow test double, not as a real Grok verdict. The
fix also resolved all six Important regressions found by the independent
read-only review of the first Round-13 GREEN diff.

The failures were bounded relation-binding problems. Sequential passive actor
collection did not recognize `then`/`subsequently` ellipsis and its ordinary
fillers. Carried Task completion required adjacency, while the first repair
mistook arbitrary `-ly` nouns and post-comma review actions for completion
modifiers and Task components. `take (on) the role of` fell back to a prior
required reviewer even for explicit unrelated roles, while initially
bypassing pronominal role objects. Alternative polarity reset only at
`but`/`yet` and let actors cross positive subordinate boundaries. Plural
pronouns used global people precedence instead of recipient, transitive
object, and subject roles. Review exclusivity recognized only immediate scope
links, and postposed absence attached any nearby `missing` term before the
first repair became too restrictive for `now`/`again` modifiers.

The corrected matcher keeps sequential passive links limited to coordinator,
alternative, or known sequential filler spans; carries only known completion
bridges; distinguishes a Task component from a following object-taking review
action; and preserves explicit and pronominal take-role targets. It resets
alternatives and actor spans at positive subordinate contrasts, gives people
recipients precedence while treating transitive plural document objects as
document antecedents, follows temporal complements through bounded modifiers,
and requires postposed absence to be the reviewer's own predicate complement.
Skill prose and structured routing, risk, generation, progress, and lineage
validation were unchanged.

### RED Evidence

All exact Round-12 report probes and their structural neighboring controls
were added before the first Round-13 production edit.

- `node --test --test-name-pattern='round-13' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Initial exact result: `tests 10`, `suites 1`, `pass 0`, `fail 10`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - Every primary group 1-7 and auxiliary group 1-3 failed for its expected
    contradiction-classification mismatch.
- After the first GREEN diff, the six independent read-only review findings
  were added before their production fixes:
  `node --test --test-name-pattern='round-13 review' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 6`, `suites 1`, `pass 0`, `fail 6`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
- The final subordinate-contrast neighbor was also captured before its
  production edit:
  `node --test --test-name-pattern='round-13 primary 4' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 1`, `suites 1`, `pass 0`, `fail 1`; exit 1 for the
    positive `whereas` implementation relation.

### GREEN Evidence

- Final focused command after formatting:
  `node --test --test-name-pattern='round-13' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 16`, `suites 1`, `pass 16`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Final full Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 262`, `suites 4`, `pass 262`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier:
  `pnpm exec prettier --write .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Both files formatted; exit 0.
- Final Prettier check on the same two files:
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `git diff --check`, `git diff --cached --check`, permitted-file inspection,
  and commit-file inspection all passed with no diagnostics.

No Rust command was run in Final Fix Round 13. No default `tauri-runtime`
feature was enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained
  unstaged.

Fix commit:
`698c98bc916e40b3891c17a1515b1e7ac375f3e1 fix(skill): bind round 13 routing relations`

The commit contains only the two permitted tracked validator files. Design,
Plan, Skill prose, progress, prior review reports, Rust, the root checkout,
and unrelated files were not edited.

### Self-Review

- Confirmed every exact sentence from both Round-12 scoped reports is covered,
  including all 17 unique exact probes, with accepted and rejected structural
  neighbors in the ten original Round-13 groups.
- Confirmed the six independent review regressions have their own RED/GREEN
  controls for former-role anaphora, post-comma completion, `supply`,
  `now`/`again` absence, `then also`, non-`list` transitive objects, and
  modified temporal exclusivity.
- Confirmed the final 262-test suite preserves every prior contradiction,
  explicit-prohibition, routing, generation, risk, progress, and lineage
  control.
- Confirmed the auxiliary report remained explicitly a simulated Grok
  workflow test double only and was not treated as a real Grok verdict.
- Confirmed the work started at exact base
  `e72a5f8345d238ad30ed4f7d966c18a9c868bc17`, the index contained only the
  two permitted files at commit time, and this ignored report stayed outside
  the index.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 14

### Result And Root Cause

Resolved the deduplicated union of all four Important groups reported by the
Round-13 primary re-review and the simulated-Grok auxiliary workflow test
double. The auxiliary report was treated only as a simulated workflow test
double, not as a real Grok verdict. The fix also resolved all six Important
regression groups found across the independent read-only review of the first
GREEN diff and its focused follow-up.

The Task-completion heuristic discarded punctuation and therefore confused a
post-comma or post-colon `review`/`test` action with a component of the Task.
Directive windows now retain bounded action punctuation, including through a
carried prior Task, so component nouns and following commands remain distinct.
Take-role target resolution treated generic demonstratives and qualified
possessive/optional objects alike, while a required primary target without a
trailing `reviewer` noun was invisible. Generic demonstrative fallback is now
limited to unqualified targets, and `required primary` is an explicit required
role target.

Plural pronoun resolution previously used any earlier recipient preposition,
even across a later clause, and recognized only `to`/`by`. It now accepts
`for`, `on behalf of`, `with`, and `together with` only when the people target
is the bounded object of that relation. Postposed reviewer absence previously
accepted only isolated modifiers, and the first repair then mistook transitive
`missing` predicates for absence. The final parser consumes the reported
multiword modifiers as phrases, preserves ordinary trailing absence
complements, and treats a following non-complement phrase as the direct object
of transitive `missing`/`lacking`. Carried Task state also preserves an
explicit postposed `active`/`running` override instead of erasing it merely
because completion appears in the same prior clause.

### RED Evidence

All exact Round-13 re-review probes and accepted/rejected structural neighbors
were added before the first Round-14 production edit.

- `node --test --test-name-pattern='round-14' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Initial exact result: `tests 4`, `suites 1`, `pass 0`, `fail 4`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The four failing test groups contained all exact primary and auxiliary
    probes for Task action/component boundaries, take-role target
    qualification, people beneficiary/participant relations, and postposed
    absence complements.
- After the first GREEN diff, the independent read-only review's three
  Important regression groups were added before their production fixes and
  run with the same focused command.
  - Exact result: `tests 4`, `suites 1`, `pass 1`, `fail 3`, `cancelled 0`,
    `skipped 0`, `todo 0`; exit 1.
  - Failures covered colon boundaries and carried punctuation, relation-local
    people links, and transitive `missing evidence`/`missing input` controls.
- The independent focused follow-up found three further Important neighboring
  regressions. Their exact probes were added before the final production edits
  and run with the same focused command.
  - Exact result: `tests 4`, `suites 1`, `pass 1`, `fail 3`, `cancelled 0`,
    `skipped 0`, `todo 0`; exit 1.
  - Failures covered ordinary `as`/`because`/`though` clause boundaries,
    non-whitelisted transitive `missing` objects, and prior Tasks that were
    explicitly both completed and active.

### GREEN Evidence

- Final focused command after formatting:
  `node --test --test-name-pattern='round-14' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 4`, `suites 1`, `pass 4`, `fail 0`, `cancelled 0`,
    `skipped 0`, `todo 0`; exit 0.
- Final full Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 266`, `suites 4`, `pass 266`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier write command:
  `pnpm exec prettier --write .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Both permitted files formatted; exit 0.
- Final Prettier check on the same two files:
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `git diff --check`, `git diff --cached --check`, permitted-file inspection,
  and commit-file inspection all passed with no diagnostics.

No Rust command was run in Final Fix Round 14. No default `tauri-runtime`
feature was enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained
  unstaged.

Fix commit:
`1e885dee4e31ea167444b5bd3f78f21dd278f947 fix(skill): bind round 14 directive relations`

The commit contains only the two permitted tracked validator files. Skill
prose, Design, Plan, progress, prior review reports, Rust, the root checkout,
and unrelated files were not edited.

### Self-Review

- Confirmed every exact sentence from both Round-13 scoped re-review reports
  has a focused classification control, with accepted and rejected neighbors
  for punctuation, role qualification, people relations, and absence scope.
- Confirmed the first independent review's three Important findings and the
  focused follow-up's three Important findings each produced genuine RED
  before their repairs and remain covered in the final GREEN suite.
- Confirmed Task punctuation survives overlapping and carried directive
  windows, qualified take-role objects outrank generic anaphora, people links
  directly govern their target, and transitive `missing` objects do not erase
  a required reviewer.
- Confirmed all 262 pre-existing tests remain green alongside the four new
  Round-14 test groups, and structured routing, risk, generation, progress,
  lineage, and production Skill validation remain unchanged.
- Confirmed work started at exact HEAD
  `698c98bc916e40b3891c17a1515b1e7ac375f3e1`, the index contained only the
  two permitted files at commit time, and this ignored report stayed outside
  the index.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 15

### Result And Root Cause

Resolved all seven Important regression groups reported by the independent
Round-14 review. The Task-component matcher treated the verb forms `review`
and `test` as component nouns even when `open issues` or `running services`
were their action objects. Take-role resolution conflated qualified anaphoric
reviewer targets with explicit unrelated roles such as a required contact
person. People-recipient inference crossed subordinate boundaries such as
`so that`, and reviewer-absence inference treated the ordinary absence
adverbs `entirely` and `completely` as transitive objects.

Active-Task switching recognized an Agent only after the switch predicate,
missing subject-first forms such as `The Task Agent switches immediately`.
Partial-completion detection reused qualifiers from an earlier completion and
therefore let `partially completed` taint a later `fully completed` state.
Task-state inference also treated an adjunct's unrelated pronoun or reflexive
as the Task without checking the adjunct's bounded subject structure.

The final matcher distinguishes component-state complements from action
objects, resolves anaphoric take-role targets separately from explicit role
objects, bounds people relations at subordinate clauses and punctuation,
classifies known absence modifiers before generic `-ly` controls, recognizes
both subject-first and object-first Agent changes, evaluates completion and
reactivation in token order, and limits adjunct Task anaphora to an unshadowed
`it`. Skill prose and the structured routing, risk, generation, progress, and
lineage contracts were unchanged.

### RED And Takeover Evidence

The handoff preserved RED-first regression and neighboring control cases for
all seven findings in five focused Round-15 test groups. The last recorded
failure before takeover was the accepted sequence:
`The active Task is partially completed, but is now fully completed. Then
switch the Task Agent.` The inherited production diff had subsequently been
updated before takeover, so the first independent focused run was already
GREEN. No additional production or test edit was made during takeover; the
existing bounded diff was inspected and verified instead of reconstructing or
claiming unavailable historical RED counts.

### GREEN Evidence

- Focused Round-15 command:
  `node --test --test-name-pattern='round-15' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 5`, `suites 1`, `pass 5`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 271`, `suites 4`, `pass 271`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check on the two owned validator files:
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on both owned validator files: no diagnostics; both exited 0.
- `git diff --check`, `git diff --cached --check`, and
  `git diff HEAD^..HEAD --check`: no diagnostics; all exited 0.
- Pre-commit unstaged and staged scope checks and the committed-file
  inspection each listed exactly the two permitted validator files.

No Rust command was run in Final Fix Round 15. No default `tauri-runtime`
feature was enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained
  unstaged.

Fix commit:
`e7da74d9113511efd163536d2006db6fa7efeed2 fix(skill): bind round 15 directive relations`

The commit contains only the two permitted tracked validator files. Skill
prose, Design, Plan, progress, prior review reports, Rust, the root checkout,
and unrelated files were not edited.

### Self-Review

- Confirmed all seven reported boundaries have direct behavioral controls:
  component/action ambiguity, take-role target identity, `so that` people
  scope, `lacking entirely/completely`, subject-first Agent switches, ordered
  partial/full completion, and adjunct-pronoun Task scope.
- Confirmed the last recorded failing partial-then-full sequence is accepted,
  while partial completion, reactivation, and genuinely active Task switches
  remain rejected.
- Confirmed all 266 pre-existing tests remain green alongside the five new
  Round-15 groups, and the production Skill still passes its authoritative
  contract validation.
- Confirmed the final commit contains only the two producer-owned tracked
  files, the index and tracked worktree are clean, and this ignored report
  remains outside the index.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 16

### Result And Root Causes

Resolved the complete deduplicated union of the six Important groups from the
Round-15 primary and auxiliary re-reviews. All distinct reproducers and
neighboring controls from both reports are represented in seven focused
Round-16 behavioral test groups.

Task-state parsing discarded the punctuation that separates an imperative
`review` or `test` action from its singular or mass object, and did not carry
that boundary across clauses. It also treated any possessive component noun as
belonging to the Task. Directive windows now retain bounded imperative
punctuation, recognize `please`, preserve the metadata through carried Task
antecedents, and require a Task-related component owner.

Reviewer substitution treated a reviewer modifier as the complete target even
when a concrete role head followed it. Reviewer targets now stop at relative
and source adjuncts and preserve explicit trailing role heads such as
`contact person` and `note taker`. People inference previously crossed purpose
clauses and treated `reviewer`/`producer` modifiers on Plan document heads as
people antecedents. It now excludes role modifiers attached to document heads
and bounds direct people relations at purpose constructions while retaining
genuine participant relations.

Postposed reviewer absence handled bare `lacking in ...` but not an adverb
between `lacking` and its `in` complement. It now recognizes the modified
transitive complement without weakening true objectless absence. Subject-first
Task Agent switches previously accepted only adverbs between the Agent and
change predicate; the bridge now includes modal, temporal, and reflexive
tokens while retaining negated and non-Task controls. Completion sequencing
now recognizes `later` and `afterward`, allowing a later full completion to
supersede an earlier partial state while preserving later reactivation.

Finally, Task-state anaphora could absorb an explicit non-Task subject or an
object pronoun inside adjunct, reporting, monitoring, tracking, and restart
coordination. The state resolver now rejects those shadowed subjects and
nested Task objects while retaining direct Task pronouns and explicit later
Task subjects. The exact embedded v2 contract and Skill prose were unchanged.

### RED Evidence

All Round-16 behavioral expectations were added before their production
repairs.

- Initial focused command:
  `node --test --test-name-pattern='round-16' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 7`, `pass 0`, `fail 7`; exit 1. Every Round-16
    classification group failed as expected.
- A singular non-`please` imperative control was added in a subsequent RED
  cycle.
  - Exact result: `tests 1`, `pass 0`, `fail 1`; exit 1.
- The first full-suite repair exposed an imperative-boundary regression for
  `review pending final approval`.
  - Exact result: `tests 278`, `pass 277`, `fail 1`; exit 1.
- Additional neighboring punctuation, role, and state pressure was added
  before its repair.
  - Exact result: `tests 7`, `pass 2`, `fail 5`; exit 1.
- Purpose-clause and explicit non-Task-subject pressure was added before its
  repair.
  - Exact result: `tests 2`, `pass 0`, `fail 2`; exit 1.
- A reviewer-relative `that` boundary was added before its repair.
  - Exact result: `tests 1`, `pass 0`, `fail 1`; exit 1.

No failing expectation was weakened or removed during the RED/GREEN cycles.

### GREEN Evidence

- Final focused Round-16 command:
  `node --test --test-name-pattern='round-16' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 7`, `suites 1`, `pass 7`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Full Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 278`, `suites 4`, `pass 278`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on both owned validator files produced no diagnostics; both
  exited 0.
- Pre-commit `git diff --check` and `git diff --cached --check` produced no
  diagnostics. Unstaged and staged scope inspections listed exactly the two
  permitted validator files, and the index contained no unrelated path.

No Rust command was run in Final Fix Round 16. The default `tauri-runtime`
feature was not enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained outside
  the index.

Fix commit:
`0934287082cccaeb9042418803a1d1af26fc3e0a fix(skill): bind round 16 directive relations`

The commit contains only the two producer-owned tracked validator files.
Skill prose, Design, Plan, progress, prior review reports, Rust, the root
checkout, and unrelated files were not edited.

### Self-Review

- Confirmed every distinct Round-15 primary and auxiliary reproducer has a
  direct Round-16 behavioral expectation plus accepted and rejected neighbors.
- Confirmed punctuation-sensitive imperative objects, possessive ownership,
  explicit role heads, purpose and participant boundaries, document-role
  modifiers, modified `lacking in` complements, Agent subject bridges,
  ordered completion, and explicit non-Task subject scope are covered.
- Confirmed all 271 pre-existing tests remain green alongside the seven new
  Round-16 groups, and production validation still enforces the authoritative
  embedded v2 contract.
- Confirmed the work started at exact base
  `e7da74d9113511efd163536d2006db6fa7efeed2`, the commit contains only the
  two permitted tracked files, and this ignored report remains outside the
  index.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 17

### Result And Root Causes

Resolved the complete 11-group Important union from the Round-16 primary and
auxiliary re-reviews. The auxiliary report was treated only as a simulated
Grok workflow test double, not as a real Grok verdict. Skill prose and the
structured routing, risk, generation, progress, and lineage validators were
not changed.

The regressions came from bounded relation shortcuts introduced in Round 16.
Imperative detection treated any earlier `please` or `test ... running` as an
action without binding it to the component, while possessive ownership treated
every possessor in a state segment as the component owner. Purpose verbs did
not distinguish their people objects from subordinate subjects, and
role/document attachment ignored punctuation. Reviewer target parsing treated
ordinary postmodifiers as concrete role heads. Subject-first Agent switching
did not inspect the change predicate's object, and completion parsing
recognized only a small Task-component object vocabulary.

Task-state anaphora had four related attachment errors: reporting predicates
and evidence participles became owners without an explicit subject; explicit
Task reporting subjects were discarded; any non-Task restart subject shadowed
a transitive Task object; and a preposed gerund before a punctuation boundary
made a later explicit Task look nested.

The corrected parser binds imperative prefixes, possessive chains, purpose
objects, role/document modifiers, Agent-change objects, completion objects,
reporting/participial owners, restart objects, and explicit Task subjects to
their local punctuation-bounded relations. Possessive chains now follow only
modifier-only links to the closest component owner, and restart pronouns count
as Task objects only when directly governed by the restart predicate.

### RED Evidence

All 11 Round-17 behavioral groups were added before the first production edit.
They include every source reproducer and accepted/rejected neighboring controls.

- Focused command:
  `node --test --test-name-pattern='round-17' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact initial result: `tests 11`, `suites 1`, `pass 0`, `fail 11`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The failure output listed every source misclassification: two imperative
    status cases, three possessive cases, two people-object cases, four
    reviewer-postmodifier cases, two punctuation cases, four Agent-object
    cases, two transitive-completion cases, three reporting/participial cases,
    two explicit reporting-subject cases, one transitive-restart case, and two
    preposed-gerund cases.
- Self-review neighbor command:
  `node --test --test-name-pattern='round-17 binds possessive|round-17 binds transitive restart' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact RED result after adding two further controls and before their
    production corrections: `tests 2`, `suites 1`, `pass 0`, `fail 2`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The failing controls proved that a remote Task possessive must not outrank
    the closest server-owned review and that a subordinate pronoun after an
    intransitive server restart is not a direct Task object.

No existing expectation was removed, weakened, or relabeled.

### GREEN Evidence

- Final focused Round-17 command:
  `node --test --test-name-pattern='round-17' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 11`, `suites 1`, `pass 11`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Complete Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 289`, `suites 4`, `pass 289`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check on both permitted validator files:
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on each permitted validator file produced no diagnostics;
  both exited 0.
- `git diff --check` and `git diff --cached --check` produced no diagnostics.
  The unstaged scope listed exactly the two permitted validator files, and the
  index was empty.

No Rust command was run in Final Fix Round 17. The default `tauri-runtime`
feature was not enabled.

### Changed Files

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended and remained outside the index.

Fix commit:
`c2fd394b94494719f0c92af1fdeaff70e592b1a0 fix(skill): bind round 17 directive relations`

### Self-Review

- Confirmed all 25 source reproducers across the 11 deduplicated groups have
  direct behavioral assertions and retain their required classifications.
- Confirmed accepted controls cover imperative actions, unrelated possessors,
  true purpose clauses, explicit trailing role heads, direct document
  modifiers, non-Agent change objects, full completion, explicit server/service
  owners, intransitive restart, and punctuation-bounded adjuncts.
- Confirmed two additional self-review controls fail before and pass after the
  closest-owner and direct-object corrections.
- Confirmed all 278 pre-existing tests remain green alongside the 11 new
  Round-17 groups, and production Skill validation remains authoritative.
- Confirmed Design, Plan, Skill prose, progress, prior review reports, Rust,
  the root checkout, and unrelated tracked files were not edited.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 18

### Result And Root Causes

Resolved the five Important regression groups in the deduplicated Round-17
primary and auxiliary re-review union. The auxiliary report was treated only
as a simulated Grok workflow test double, not as a real Grok verdict. Skill
prose and the structured routing, risk, generation, progress, and lineage
validators were unchanged.

The regressions came from five overly narrow or prematurely terminating
relations in the bounded directive classifier. Possessive component binding
accepted only a small adjective vocabulary, so ordinary `security`, `code`,
and `integration` compounds disconnected a directly possessed review/test
from its server owner. Completion direct-object detection treated unlisted
prepositional and temporal adjunct heads as nouns. Subject-first Agent change
handling treated identity/profile nouns as unrelated objects even though they
are part of the selected route identity.

Restart pronoun handling always treated a directly governed `it` as the Task
object, without checking an explicit separately qualified non-Task antecedent
in the preceding coordinated segment. Finally, Task-object detection reset at
the opening comma of a parenthetical and discarded the governing outer server
subject. The repair adds bounded compound, adjunct, route-identity, explicit
antecedent, and paired-parenthetical relations while preserving the prior
source cases and controls.

### RED Evidence

All five Round-18 behavioral groups and their neighboring controls were added
before the first production edit. No existing expectation was removed,
weakened, or relabeled.

- Focused RED command:
  `node --test --test-name-pattern='round-18' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 5`, `suites 1`, `pass 0`, `fail 5`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The failures named all three qualified server-owned components, all five
    completion adjuncts, all three Agent identity/profile changes, the
    explicit separate-service restart antecedent, and the parenthetical Task
    object.

### GREEN And Full Verification Evidence

- Final focused Round-18 command:
  `node --test --test-name-pattern='round-18' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 5`, `suites 1`, `pass 5`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Complete Node validator suite, run once before commit:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 294`, `suites 4`, `pass 294`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check on both permitted validator files:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on each permitted validator file produced no diagnostics;
  both exited 0.
- Pre-commit `git diff --check` and `git diff --cached --check` produced no
  diagnostics. Unstaged and staged scope checks listed exactly the two
  permitted validator files, and the index contained no unrelated path.
- Post-commit inspection confirmed exactly one commit after fix base
  `c2fd394b94494719f0c92af1fdeaff70e592b1a0`, exactly the two permitted
  validator paths in the range, no `git show --check` diagnostics, an empty
  index, and a clean tracked worktree.

No Rust command was run in Final Fix Round 18. The default `tauri-runtime`
feature was not enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained outside
  the index.

Fix commit:
`a778e592e41c2b45bc7e0489140e4b31a9fac6cd fix(skill): bind round 18 validator relations`

### Self-Review

- Confirmed every Round-18 source reproducer has a direct behavioral
  expectation and each group retains its required accepted and rejected
  controls.
- Confirmed qualified Task-owned components still reject, genuine transitive
  completion and later reactivation still reject, unrelated object changes
  still accept, direct Agent switching still rejects, the no-competitor
  restart source still rejects, and preposed-gerund Task restarts still
  reject.
- Confirmed the production mutations are independently exercised: removing
  the compound, adjunct, route-identity, explicit-antecedent, or
  parenthetical-subject relation makes its corresponding Round-18 test fail.
- Confirmed all 289 pre-existing tests remain green alongside the five new
  Round-18 groups and no prior test expectation was deleted.
- Confirmed the commit contains only the two producer-owned tracked validator
  files. Skill prose, Design, Plan, progress, prior review reports, Rust, the
  root checkout, and unrelated tracked files were not edited.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 19

### Result And Root Causes

Resolved all four Important regression groups in the deduplicated Round-18
primary and simulated-auxiliary re-review union. The auxiliary report was
treated only as an explicitly labeled simulated Grok workflow test double,
not as a real Grok verdict. Skill prose and the structured routing, risk,
generation, progress, and lineage validators were unchanged.

The Round-18 completion-adjunct repair classified only the first lexical head,
so possessive temporal artifact objects such as `today's documentation` were
mistaken for bare temporal adjuncts. Completion direct-object detection now
checks the recorded possessive boundary before applying adjunct-head
exemptions.

Agent-change classification scanned the entire coordinated action segment for
identity/profile nouns. It now treats an identity/profile term as the selected
Agent object only when the tokens between the change predicate and that term
are route-identity determiners or qualifiers; an earlier concrete object or
later adjunct keeps the change unrelated to Agent identity.

The explicit non-Task antecedent heuristic did not account for a closer Task
mention later in the same bounded segment, and the low-level antecedent path
also runs before parsed Tasks are attached to the clause object. It now derives
the Task mentions from the clause tokens and lets a later explicit Task
antecedent defeat the qualified non-Task fallback.

Finally, the parenthetical outer-subject recovery treated every gerundive Task
object alike. It now distinguishes a Task governed by a reactivation predicate
such as `restarting` from a Task that is merely monitored, preserving explicit
Task reactivation without weakening the valid server-parenthetical control.

### RED Evidence

All four Round-19 behavioral groups were added before the first production
edit. They include every exact reproducer and every neighboring acceptance or
rejection control required by the findings file.

- Command:
  `node --test --test-name-pattern='round-19' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 4`, `suites 1`, `pass 0`, `fail 4`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The four failures reported exactly the four possessive temporal objects,
    four unrelated profile-bearing object changes, two closer explicit Task
    antecedents, and one parenthetical explicit Task restart.
  - All neighboring controls embedded in those four groups already classified
    correctly during RED.

No existing expectation was removed, weakened, or relabeled.

### GREEN And Verification Evidence

- Focused Round-19 command after formatting:
  `node --test --test-name-pattern='round-19' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 4`, `suites 1`, `pass 4`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Complete Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 298`, `suites 4`, `pass 298`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on each permitted validator file produced no diagnostics;
  both exited 0.
- Unstaged and staged `git diff --check` produced no diagnostics. Before the
  commit, the unstaged and staged scope inspections each listed exactly the
  two permitted validator paths at their respective gate, and the index
  contained no unrelated path.
- Post-commit inspection confirmed exactly one commit after fix base
  `a778e592e41c2b45bc7e0489140e4b31a9fac6cd`, exactly the two permitted
  validator paths in the range, no `git show --check` diagnostics, an empty
  index, and a clean tracked worktree.

No Rust command was run in Final Fix Round 19. The default `tauri-runtime`
feature was not enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained outside
  the index.

Fix commit:
`ed1cec8b276d8e9dba4911fdbfb07a2bcbbeeed2 fix(skill): bind round 19 validator relations`

The commit contains only the two permitted tracked validator files. Skill
prose, Design, Plan, progress, prior review reports, Rust, the root checkout,
and unrelated files were not edited.

### Self-Review

- Confirmed every Round-19 source reproducer and required neighboring control
  has a direct behavioral expectation, including all bare temporal adjuncts,
  direct selected-profile changes, unrelated profile-bearing changes,
  restart-pronoun controls, and parenthetical/preposed-gerund controls.
- Confirmed the pre-fix RED independently exercises the realistic production
  mutations: dropping the possessive-object boundary, restoring the
  segment-global profile scan, letting a qualified non-Task antecedent outrank
  a closer Task, or collapsing reactivation and monitoring parentheticals
  makes its corresponding Round-19 group fail.
- Confirmed all 294 pre-existing tests remain green alongside the four new
  Round-19 groups and no prior test expectation was deleted.
- Confirmed the corrections remain bounded to existing directive windows and
  reuse existing tokenizer metadata, Task parsing, and reactivation
  vocabulary rather than adding exact-sentence matches.
- Confirmed the report is ignored by the existing `.superpowers` rule and was
  appended only after the focused commit.

### Remaining Concerns

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.

## Final Fix Round 20

### Result And Root Causes

Resolved all three Important groups in the deduplicated Round-19 primary and
simulated-auxiliary re-review union. The auxiliary report was treated only as
an explicitly labeled simulated Grok workflow test double, not as a real Grok
verdict. Skill prose and the structured routing, risk, generation, progress,
and lineage validators were unchanged.

The Round-19 selected-profile relation admitted only a narrow determiner list,
so direct possessive qualifiers `their` and `own` made selected Agent profile
changes look like unrelated object changes. Those qualifiers now remain
inside the direct identity object while an intervening concrete profile kind
still keeps an unrelated object outside the active-route ban.

The closer-Task antecedent check treated every later `Task` token as a direct
restart antecedent. It now requires a terminal direct Task object, excluding
compound heads such as `Task worker` and `Task log` and Task noun phrases
attached through prepositional non-object links. Explicit `monitors the Task`
and `restarts the Task` objects still override the qualified non-Task subject.

Finally, parenthetical Task reactivation checked only the reactivation verb.
It now requires a non-negated predicate whose path to the Task contains only
direct-object qualifiers. Negated `not`/`without` relations and a reactivation
of another object that merely mentions the Task remain attached to the outer
non-Task subject, while affirmative direct `restarting the Task` remains an
active-Task contradiction.

### RED Evidence

All three Round-20 behavioral groups were added before the first production
edit. They included every exact findings reproducer and the required retained
acceptance/rejection controls. No existing expectation was removed, weakened,
or relabeled.

- Command:
  `node --test --test-name-pattern='round-20' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact initial result: `tests 3`, `suites 1`, `pass 0`, `fail 3`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The first failure listed the three direct possessive selected-profile
    changes as expected rejections that were accepted.
  - The second failure listed the `Task worker`, `Task log`, and
    `service for the Task` cases as expected acceptances that were rejected.
  - The third failure listed both negated parentheticals and the nested worker
    reactivation as expected acceptances that were rejected.
  - The retained direct `its selected profile`, explicit closer-Task objects,
    affirmative direct parenthetical reactivation, and unrelated profile
    objects already classified correctly during RED.

### GREEN And Verification Evidence

- Final focused Round-20 command:
  `node --test --test-name-pattern='round-20' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 3`, `suites 1`, `pass 3`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Complete Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 301`, `suites 4`, `pass 301`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier check:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on each permitted validator file produced no diagnostics;
  both exited 0.
- Pre-commit `git diff --check` and `git diff --cached --check` produced no
  diagnostics. The unstaged and staged scope gates each listed exactly the two
  permitted validator files at their respective stage, with no unrelated path.
- Post-commit inspection confirmed exactly one commit after
  `ed1cec8b276d8e9dba4911fdbfb07a2bcbbeeed2`, exactly the two permitted
  validator paths, no `git show --check` diagnostics, and an empty tracked
  worktree and index.

No Rust command was run in Final Fix Round 20. The default `tauri-runtime`
feature was not enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained outside
  the index.

Fix commit:
`1081eda2b0b24a470d0b591c47920b89c38d77b9 fix(skill): bind round 20 validator relations`

The commit contains only the two permitted tracked validator files. Skill
prose, Design, Plan, progress, prior review reports, Rust, the root checkout,
and unrelated files were not edited.

### Self-Review

- Confirmed every Round-20 source reproducer has a direct behavioral
  expectation and all required Round-19 controls retain their classifications.
- Confirmed the direct-object correction distinguishes explicit Task objects,
  compound Task artifacts, and prepositional Task attachments; an additional
  parenthetical `Task worker` control remains accepted.
- Confirmed the parenthetical correction checks both polarity and object
  attachment rather than matching any preceding reactivation predicate.
- Confirmed all 298 pre-existing tests remain green alongside the three new
  Round-20 groups, and no prior expectation was deleted or relabeled.
- Confirmed the work started at exact clean base
  `ed1cec8b276d8e9dba4911fdbfb07a2bcbbeeed2`, the commit contains only the
  two permitted tracked files, and this report remains ignored and unstaged.

### Remaining Concerns And Final Status

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.
- Final Fix Round 20 status: `DONE_WITH_CONCERNS`; there is no new scoped
  concern, and the two retained whole-branch Minors remain recorded above.

## Final Fix Round 21

### Result And Root Causes

Resolved all three Important groups in the deduplicated Round-21 findings
union. The validator's structured routing, risk, generation, progress, and
lineage behavior and the Skill prose were unchanged.

The Round-20 direct-antecedent helper treated every non-modifier token after a
Task mention as proof that `Task` was not the direct object. That correctly
excluded compound heads such as `Task worker` and `Task log`, but also detached
a real direct object when an ordinary prepositional adjunct followed it, as in
`monitors the Task for diagnostics`. The helper now first rejects a relation
whose qualifier path starts at a non-object link, then distinguishes a trailing
compound head from a trailing adjunct by its first significant relation token.
`near` and `on` are now explicit non-object links, so `near the Task` and
`reports on the Task` remain attached to the separate service rather than the
Task.

The direct parenthetical reactivation qualifier set omitted the ordinary
possessive determiners `their`, `our`, and `your` and the demonstrative `that`.
Those tokens now remain on the direct Task-object path, while the existing bare,
compound, indirect, and negated controls retain their prior classifications.

Finally, parenthetical reactivation reused the generic six-token negation
lookback. A negation governing an earlier action could therefore suppress a
later affirmative restart. The generic helper now accepts an optional relation
boundary set, and the Task-reactivation call uses Task clause and carried-adjunct
boundaries. In `not idling before restarting the Task`, `before` ends the
negation scope; directly scoped `not restarting` and `without restarting`
remain negated.

### RED Evidence

All three Round-21 behavioral groups were added before the first production
edit. No existing expectation was removed, weakened, or relabeled.

- Command:
  `node --test --test-name-pattern='round-21' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact initial result: `tests 3`, `suites 1`, `pass 0`, `fail 3`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 1.
  - The direct-object group reported the expected rejection of
    `monitors the Task for diagnostics` as accepted and the expected acceptance
    of `reports on the Task` as rejected. Its `near the Task`, compound Task,
    prepositional `for the Task`, and terminal direct-object controls were
    already classified as expected.
  - The qualifier group reported all four required `their`, `our`, `your`, and
    `that` Task reactivations as expected rejections that were accepted. The
    retained bare `restarting the Task` control already rejected.
  - The polarity group reported `not idling before restarting the Task` as an
    expected rejection that was accepted. The directly negated `not
    restarting` and `without restarting` controls already accepted.

### GREEN And Verification Evidence

- Final focused Round-21 command:
  `node --test --test-name-pattern='round-21' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `tests 3`, `suites 1`, `pass 3`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Complete Node validator suite:
  `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact final result: `tests 304`, `suites 4`, `pass 304`, `fail 0`,
    `cancelled 0`, `skipped 0`, `todo 0`; exit 0.
- Production validator:
  `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - Exact final result: `PASS: brainstorm-to-delivery Simple contract`,
    `SKILL.md line count: 418`, `0 failures, 1 checks completed`; exit 0.
- Prettier initially reported a layout-only warning in the library file after
  GREEN. Repository formatting was applied to both permitted files, and the
  focused test, full suite, and every final gate were rerun against that exact
  formatted state.
- Final Prettier check:
  `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - Exact result: `All matched files use Prettier code style!`; exit 0.
- `node --check` on each permitted validator file produced no diagnostics;
  both exited 0.
- Pre-commit `git diff --check` and staged `git diff --cached --check` produced
  no diagnostics. The unstaged and staged scope checks each listed exactly the
  two permitted validator files, and the staged `.superpowers/sdd/**` check was
  empty.
- Post-commit inspection found exactly one commit after the required base,
  exactly the two permitted paths in the commit, no `git show --check`
  diagnostics, and an empty tracked worktree and index before this ignored
  report append.

No Rust command was run in Final Fix Round 21. The default `tauri-runtime`
feature was not enabled.

### Changed Files And Commit

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- This ignored Task report was appended after the commit and remained outside
  the index.

Fix commit:
`21401f42a993024fefc97b984c11196928e2dd74 fix(skill): bind round 21 task relations`

The commit contains only the two permitted tracked validator files. Skill
prose, Design, Plan, progress, prior review reports, Rust, the root checkout,
and unrelated files were not edited.

### Self-Review

- Confirmed every exact Round-21 reproducer has a direct behavioral expectation.
  The first group repeats the complete Round-20 direct/compound/prepositional
  matrix, the second retains the bare affirmative reactivation, and the third
  retains both directly negated parenthetical controls.
- Confirmed the test-file change is additive (`49` additions, `0` deletions),
  so no existing expectation was deleted, weakened, or relabeled.
- Confirmed the antecedent logic separately evaluates the link before the Task
  object and the first significant token after it. Removing either relation
  check would fail a Round-21 direct or inverse expectation.
- Confirmed reactivation polarity still uses the shared negation semantics but
  stops at the nearest Task-specific clause boundary; removing the boundary
  argument would fail the Round-21 polarity reproducer.
- Confirmed all 301 pre-existing tests remain green alongside the three new
  Round-21 groups.
- Confirmed work started from exact clean base
  `1081eda2b0b24a470d0b591c47920b89c38d77b9`, the fix is exactly one commit,
  and this report remains ignored and unstaged.

### Remaining Concerns And Final Status

- Retained Task 2 Minor: the shared fence detector accepts a backtick opener
  whose info string contains a backtick although CommonMark rejects it.
- Retained Task 4 Minor: failed/canceled projection-locality coverage remains
  combined rather than isolated one condition at a time.
- The validator remains a bounded structural English classifier rather than a
  general semantic parser; the exact embedded v2 contract remains the
  authority.
- Final Fix Round 21 status: `DONE_WITH_CONCERNS`; there is no new scoped
  concern, and the two retained whole-branch Minors remain recorded above.
