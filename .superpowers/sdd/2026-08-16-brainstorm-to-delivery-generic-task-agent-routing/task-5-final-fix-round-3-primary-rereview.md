# Task 5 Final Fix Round 3 Primary Re-review

## Findings

### Critical

None.

### Important

1. **The producer exemption hides explicit parent co-ownership.**
   `conflictsWithParentOwnership` skips a production action whenever a `Plan
   Author` or `Design Fixer` phrase appears between `parent` and that action
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:889`).
   This correctly accepts the new orchestration fixture, but it also accepted
   both of these direct violations of `parent_edits: false`:

   - `The parent and the Plan Author revise the Plan.`
   - `The parent directs the Plan Author and then revises the Plan.`

   An intervening producer name does not prove that the parent is absent from
   the action's subject. Distinguish delegation (`parent` directs producer to
   act) from a coordinated subject or a later parent-owned action, and add both
   co-ownership cases as negative controls.

2. **Task Agent actor/action attachment remains wrong in both directions.**
   The actor-last branch ignores a candidate whenever any earlier `by`/`to`
   link occurs between the action and the Task Agent, even when that link is an
   explicit Codex exclusion; the scope matcher also still treats universal
   normal Tasks as forbidden and cannot identify an auxiliary-review target
   (`validate-contract.lib.mjs:924`, `:776`). Fresh probes incorrectly accepted
   both contradictions:

   - `Route high Tasks not to Codex but to the Task Agent.`
   - `High Tasks are not implemented by Codex but by the Task Agent.`

   The same validator incorrectly rejected both approved routes:

   - `The Task Agent implements every normal Task.`
   - `Route every high Task auxiliary review to the Task Agent.`

   The new exact Codex/Task-Agent positive controls pass, but these probes show
   that attachment still depends on the presence of another link rather than
   which actor owns which production or review action. Bind the action target
   and explicit exclusion before applying high/universal scope.

3. **Completed-boundary recognition is keyword-specific and can override an
   explicit active-Task switch.** `hasCompletedTaskBoundary` recognizes only an
   `after ... Task ... completion-term` sequence, while
   `conflictsWithActiveTaskSwitch` treats any recognized completed-boundary
   phrase as an unconditional exemption (`validate-contract.lib.mjs:830`,
   `:987`). It rejected two ordinary legal boundary directives:

   - `Switch the Task Agent once the current Task completes.`
   - `Switch the Task Agent following completion of the current Task.`

   It also accepted this explicit in-Task switch because a later completion
   phrase canceled the earlier active timing:

   - `Switch the Task Agent during the current Task and after the current Task completes.`

   Preserve the exact new `after ... completes` positive control, but attach
   each timing clause to the switch and never let a later boundary erase an
   explicit `during`/`while` active-Task directive.

4. **Required/optional review attachment still rejects requirements and can
   accept document-reviewer replacement.** Review bypass validation recognizes
   a narrow preceding-token negation and requires `Design` to follow `Codex`
   locally (`validate-contract.lib.mjs:1013`, `:871`). It incorrectly rejected
   both compliant requirements:

   - `Primary review is mandatory rather than optional.`
   - `Primary review is required and cannot be omitted.`

   It incorrectly accepted this forbidden replacement, despite `Design` and
   `Codex reviewer` having an unambiguous shared subject in the same clause:

   - `Optional user-named Design reviewers replace the Codex reviewer.`

   The previously reported semicolon cases now classify correctly, but the
   parser still needs to bind `optional`/`omitted`/`replace` to the relevant
   reviewer role and recognize direct exclusion forms such as `rather than`
   and `cannot`.

### Minor

None newly found in `ef10695a..7173d031`.

## Prior Finding Disposition

All seven exact compliant probes from the authoritative Round 2 review now
return no failures:

1. Parent-to-Plan-Author orchestration: addressed for the exact fixture.
2. Codex implementation plus Task Agent review and explicit Task Agent
   exclusion: addressed for the three exact fixtures.
3. `after the current Task completes` boundary switch: addressed for the exact
   fixture.
4. Optional user-named document reviewers and required primary review across a
   semicolon: addressed for the two exact fixtures.

The four Important findings above are new pressure-test failures within the
same ownership, attachment, boundary, and reviewer-role classes. They show the
fix is not yet safe in either direction.

## Regression Controls

- All nine prior contradiction probes still return `B2D-SKILL-005`.
- All six prior explicit-prohibition controls still return no failures.
- Semicolon separation works for both directions tested independently:
  `Never skip primary review; skip auxiliary review after a high Task fix.` is
  rejected, while `Primary review is required; user-named Design reviewers are
  optional.` is accepted.
- Straight actor attachment works for the additional controls: Task Agent
  review of Codex production is accepted, while Task Agent review plus high
  implementation is rejected.

## Retained Prior Minors

These remain separate, non-blocking branch debt:

1. The shared JavaScript/Rust fence detector accepts a backtick opener whose
   info string contains a backtick although CommonMark rejects it.
2. Failed/canceled Simple route-locality coverage combines both states rather
   than proving each sibling-isolation case independently.

The resolved Task 5 `tasks: [null]` issue remains closed.

## Verification

- Reviewed only commit `7173d031` through the supplied
  `ef10695a..7173d031` package, Task 5 brief/report, and authoritative Round 2
  primary report.
- Focused Round 2 positive controls: 7 passed, 0 failed.
- Prior `rereview` contradiction/prohibition controls: 15 passed, 0 failed.
- Full Node validator suite: 58 passed, 0 failed.
- Production Skill validator: passed with 0 failures and reported 418 split
  lines.
- Prettier check for both validator files: passed.
- `git diff --check ef10695a..7173d031`: passed.
- Direct read-only pressure probe: 16 cases, 6 correct classifications and 10
  misclassifications; the misclassifications and two additional retained
  route false positives are documented above.
- No Rust command was run. No production file was edited by this review.

## Counts And Verdict

Scoped counts: **Critical 0, Important 4, Minor 0**. Retained non-blocking
branch debt: **2 Minors**.

**Not approved.** Commit `7173d031` closes every exact Round 2 fixture and
preserves the existing contradiction/prohibition suite, but direct neighboring
forms still permit parent co-ownership, misattach Task Agent production,
misclassify completed boundaries, and conflate required and optional reviewer
roles.
