# Task 5 Final Fix Round 4 Primary Re-review

## Findings

### Critical

None.

### Important

1. **Coordinated producer actions are still attributed to the parent.**
   `actionDelegatesToProducer` rejects delegation as soon as any `and` or
   `but` appears between the producer role and the action
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1029`).
   That preserves the new parent co-editing rejections, but it also rejected
   this ordinary compliant delegation:

   - `The parent directs the Plan Author to revise and update the Plan.`

   The single-action form and the form with two separately named producers
   were both accepted. A coordinated infinitive with one explicit producer
   must retain that producer as its subject without reopening either direct
   parent action covered by the Round-3 negative controls.

2. **Task Agent action, scope, and review-purpose attachment remains
   order-dependent in both directions.** `actionSegment` cuts every predicate
   at `and`, while `actionTaskScope` can recover only a preceding Task through
   the exact token `them` (`validate-contract.lib.mjs:919`, `:929`). Passive
   actor resolution likewise attaches `by the Task Agent` only to the nearest
   action, and the route-target branch decides before considering a following
   review purpose (`validate-contract.lib.mjs:965`, `:1099`). The pressure
   probe therefore accepted all three direct high-Task implementation
   contradictions:

   - `The Task Agent implements and reviews high Tasks.`
   - `High Tasks are implemented and reviewed by the Task Agent.`
   - `High Tasks are reviewed and implemented by the Task Agent.`

   It also rejected both compliant high-Task auxiliary-review routes:

   - `Route high Tasks to the Task Agent for auxiliary review.`
   - `Route high Tasks to the Task Agent for review.`

   The leading-object variant, `Route auxiliary review of high Tasks to the
   Task Agent.`, passes, confirming that word order rather than the route
   relation controls the result. Bind shared subjects/objects and a
   route-to-review purpose before applying the high/universal prohibition.

3. **Completed and inactive Task boundaries are still rejected outside the
   three marker words.** `hasCompletedTaskTiming` recognizes only `after`,
   `following`, and `once`, and `taskHasActivity` does not account for a
   locally negated active state (`validate-contract.lib.mjs:1148`, `:1171`).
   Consequently both non-conflicting boundary statements were rejected:

   - `Change the Task Agent when the current Task is complete.`
   - `Change the Task Agent only while no Task is running.`

   The paired `after ... is complete` form is accepted and the positive
   `while ... is running` contradiction is rejected. Preserve active-timing
   precedence, but recognize completed-state predicates and negated activity
   instead of falling back solely on `current` or `running` tokens.

4. **Reviewer replacement still loses pronoun and passive-predicate targets.**
   `reviewTargetForBypass` searches only reviewers inside the current
   coordinated segment and has no antecedent handling
   (`validate-contract.lib.mjs:1218`). It therefore accepted all three clear
   replacements of the mandatory Codex reviewer:

   - `The Codex reviewer remains required, but user-named Design reviewers may replace it.`
   - `The Codex reviewer is required and user-named Design reviewers replace it.`
   - `The primary Codex reviewer is required but can be replaced by user-named Design reviewers.`

   The opposite direction also remains unsafe: `User-named Design reviewers,
   although optional, cannot replace the Codex reviewer.` was rejected, while
   the equivalent copular form using `are optional` was accepted. The four new
   exact reviewer-attachment tests pass, but they do not prove affirmative
   pronoun replacement or passive continuation. Attach `it` and coordinated
   passive predicates to the preceding Codex reviewer while retaining the
   document-reviewer exception only for the document reviewer itself.

### Minor

None newly found in `7173d031..20f19035`.

## Round-3 Finding Disposition

All 24 tests selected by the `round-3` pattern now pass, including the four
new exact reviewer-attachment controls. The exact parent co-editing, Task
Agent exclusion/route, active-versus-completed timing, and reviewer-subject
fixtures from Round 3 are addressed.

The four Important findings above are neighboring controls in those same four
relation classes. They show that the replacement relation model still changes
classification based on predicate order, coordination, or a pronoun rather
than the required ownership and routing relation.

## Retained Prior Minors

These remain separate, non-blocking branch debt and are not included in the
scoped Minor count:

1. The shared JavaScript/Rust fence detector accepts a backtick opener whose
   info string contains a backtick although CommonMark rejects it.
2. Failed/canceled Simple route-locality coverage combines both states rather
   than proving each sibling-isolation case independently.

The resolved Task 5 `tasks: [null]` issue remains closed.

## Verification

- Reviewed range `7173d031..20f19035` at HEAD
  `20f190352a639b06a684016e36c880a8d7cf0af8`.
- Reviewed commits:
  - `bf41df667e69e0d8d75cc548281b9d7c1fed977b refactor(skill): attach contradiction grammar relations`
  - `20f190352a639b06a684016e36c880a8d7cf0af8 fix(skill): attach review bypasses to reviewer subjects`
- Supplied review package SHA-256:
  `1ae66e2b8399b300982295db58153d4580afdba9b4b6bf90a4302beb7d6068e7`.
- `node --test --test-name-pattern='round-3' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  24 passed, 0 failed.
- `node --test --test-name-pattern='round-3 reviewer attachment' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  4 passed, 0 failed.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  82 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  passed with 0 failures and reported 418 split lines.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  passed.
- `git diff --check 7173d031..20f19035`: passed.
- Direct read-only relation pressure probe: 19 cases, 7 correct
  classifications and 12 misclassifications. The 12 failures are enumerated
  in the four Important findings above.
- No Rust command was run. This follows the controller restriction that Rust
  verification must wait until all Tasks finish and then use only
  `--no-default-features --features server,test-utils`.

## Counts And Verdict

Scoped counts: **Critical 0, Important 4, Minor 0**. Retained non-blocking
branch debt: **2 Minors**.

**NOT APPROVED.** The exact Round-3 controls and the complete existing suite
pass, but direct neighboring forms still permit high-Task implementation by
the Task Agent and replacement of the required Codex reviewer, while rejecting
valid parent delegation, auxiliary-review routing, completed boundaries, and
optional document-reviewer wording.
