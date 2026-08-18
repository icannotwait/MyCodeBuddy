# Task 5 Final Fix Round 5 Primary Re-review

## Findings

### Critical

None.

### Important

1. **The coordinated-action fix reopens parent co-editing when the delegated
   infinitive is followed by a finite parent action.**
   `coordinatedActionGroup` groups actions separated by `and` without
   distinguishing a delegated bare infinitive from a later finite predicate,
   while `actionDelegatesToProducer` can also reuse any earlier `to` for a
   later ungrouped action
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:955`,
   `:1194`). The validator incorrectly accepted all three direct violations of
   `parent_edits: false`:

   - `The parent asks the Plan Author to revise the Plan and revises it too.`
   - `The parent directs the Plan Author to revise the Plan, then updates the Design.`
   - `The parent tells the Plan Author to update the Plan and then edits the Design itself.`

   The compliant `... Plan Author to revise and update the Plan` form remains
   accepted, and an explicitly repeated second `parent` is rejected. The
   relation must preserve coordinated infinitives owned by one producer
   without extending that ownership through a later finite matrix predicate.

2. **Task route classification remains order-dependent across contrast,
   mixed scopes, and trailing reviewer slots.** Actions share Task scope and a
   passive actor only across `and`, so a shared object or actor after `but` is
   lost (`validate-contract.lib.mjs:955`, `:1007`, `:1045`). `scopeForTasks`
   collapses a mixed normal/high object to `high`, which hides the normal-route
   violation (`validate-contract.lib.mjs:1000`). `routeHasReviewPurpose` checks
   `primary` only before the first `review` token, so a trailing primary slot
   turns a forbidden primary route into an apparent auxiliary route
   (`validate-contract.lib.mjs:1158`). The validator incorrectly accepted:

   - `The Task Agent implements but does not review high Tasks.`
   - `High Tasks are implemented but not reviewed by the Task Agent.`
   - `Codex implements normal and high Tasks.`
   - `The Task Agent reviews normal and high Tasks.`
   - `Route a high Task to Grok for review in the primary slot.`
   - `Route a high Task to Grok for review as the primary reviewer.`

   The paired legal controls with negated implementation, auxiliary slots, or
   separated normal implementation/high review were accepted. Preserve scope
   per Task and bind shared predicate relations and the complete review-purpose
   phrase before deciding the route.

3. **Preposed active-Task timing is ignored.** Every timing path in
   `conflictsWithActiveTaskSwitch` requires the Task to appear after the
   `change`/`switch` token (`validate-contract.lib.mjs:1362`, `:1407`). These
   explicit active-Task switches therefore passed:

   - `While the current Task is running, change the Task Agent.`
   - `During an active Task, switch the Task Agent.`
   - `The current Task is active when you switch the Task Agent.`

   The equivalent postposed active form is rejected, while the preposed
   completed and negated-running controls are accepted. Attach timing on both
   sides of the switch action while retaining active-state precedence.

4. **Reviewer replacement antecedents still depend on exact conjunction and
   pronoun spelling.** `reviewTargetForBypass` recognizes only singular `it`
   and only recovers an antecedent outside a segment created by `and` or `but`;
   the `in place of` check also requires those three tokens to be adjacent
   (`validate-contract.lib.mjs:1453`, `:1515`). The validator incorrectly
   accepted every one of these replacements of the required Codex reviewer:

   - `The Codex reviewer is required, yet user-named Design reviewers may replace it.`
   - `The primary Codex reviewers are required, but user-named Design reviewers may replace them.`
   - `The Codex reviewer is required, but user-named Design reviewers may replace that reviewer.`
   - `Use optional user-named Design reviewers in the place of the Codex reviewer.`
   - `The Codex reviewer is required, while user-named Design reviewers may replace it.`

   The corresponding explicit-object, singular-`it` after `and`/`but`, exact
   `in place of`, and negated-replacement controls all classify correctly.
   Resolve replacement objects and antecedents independently of this narrow
   token order.

### Minor

None newly found in `7173d031..899e961c`.

## Round-4 Finding Disposition

All 25 automated `round-4` controls pass, including every exact sentence
enumerated in the authoritative Round-4 report and the producer's additional
primary-slot control. The full existing suite also remains green.

The four Important findings above are nearby positive/negative controls in the
same ownership, Task-route, boundary-timing, and reviewer-target classes. In
particular, the first finding regresses the previously correct neighboring
parent co-editing control when the producer fix carries its subject through a
finite parent predicate.

## Retained Prior Minors

These remain separate, non-blocking branch debt and are not included in the
scoped Minor count:

1. The shared JavaScript/Rust fence detector accepts a backtick opener whose
   info string contains a backtick although CommonMark rejects it.
2. Failed/canceled Simple route-locality coverage combines both states rather
   than proving each sibling-isolation case independently.

The resolved Task 5 `tasks: [null]` issue remains closed.

## Verification

- Reviewed the supplied `7173d031..899e961c` package at HEAD
  `899e961cd16803f17a7caee4b84d2a336a5d2607`.
- Supplied review package SHA-256 verified as
  `260246cb8e1a6245a59fc78b773bd166f122278a510197ab023f9ebca1166215`.
- Reviewed commits:
  - `bf41df667e69e0d8d75cc548281b9d7c1fed977b refactor(skill): attach contradiction grammar relations`
  - `20f190352a639b06a684016e36c880a8d7cf0af8 fix(skill): attach review bypasses to reviewer subjects`
  - `899e961cd16803f17a7caee4b84d2a336a5d2607 fix(skill): bind coordinated directive relations`
- `node --test --test-name-pattern='round-4' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  25 passed, 0 failed.
- `node --test --test-name-pattern='round-3' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  24 passed, 0 failed.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  107 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  passed with 0 failures and reported 418 split lines.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  passed.
- `git diff --check 7173d031..899e961c`: passed.
- Main direct read-only pressure probe: 33 cases, 18 correct classifications
  and 15 misclassifications. A focused parent probe reproduced three misses in
  five cases, adding two unique parent co-editing failures beyond the main
  probe. All failures are enumerated in the four Important findings above.
- No Rust command was run.

## Counts And Verdict

Scoped counts: **Critical 0, Important 4, Minor 0**. Retained non-blocking
branch debt: **2 Minors**.

**NOT APPROVED.** The exact Round-4 controls and all 107 existing tests pass,
but neighboring forms still allow parent co-editing, invalid normal/high Task
routes, active-Task Agent switching, and replacement of the mandatory Codex
reviewer.
