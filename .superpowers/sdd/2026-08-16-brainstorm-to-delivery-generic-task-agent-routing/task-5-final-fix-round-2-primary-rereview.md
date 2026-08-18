# Task 5 Final Fix Round 2 Primary Re-review

## Findings

### Critical

None.

### Important

1. **The parent-ownership matcher rejects valid parent-to-producer
   orchestration.** `conflictsWithParentOwnership` treats any production verb
   within eleven tokens after `parent` as an action owned by the parent, even
   when an intervening role is the grammatical actor
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:804`).
   A fresh production-validator probe returned `B2D-SKILL-005` for:

   > The parent directs the Plan Author to update the Plan.

   This is compliant with the approved Design: the parent owns coordination
   and sends revision briefs, while the Plan Author alone edits the Plan
   (`docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:104`,
   `:154`, `:159`). The newly added `update` inflections make this ordinary
   compliant wording a regression. Bind the production action to the parent
   actor, and add a positive control in which the parent dispatches or directs
   the named document producer.

2. **The Task Agent route matcher misattributes Codex production and explicit
   Task Agent exclusions to the Task Agent.** The actor-first branch scans any
   route/production action after `Task Agent`, and the actor-last branch scans
   any action before a nearby `by` or `to`; neither branch establishes which
   actor owns that action (`validate-contract.lib.mjs:838`). Fresh probes all
   returned `B2D-SKILL-005`:

   > High Tasks are implemented by Codex and reviewed by the Task Agent.

   > The Task Agent reviews work implemented by Codex for high Tasks.

   > Route high Tasks to Codex, not to the Task Agent.

   The first two state the canonical high route, and the third explicitly
   prohibits the invalid high Task Agent producer route. They are required by
   the Design's Codex implementer plus Task Agent auxiliary-reviewer contract
   (`generic-task-agent-design.md:169`). The last case also shows that checking
   negation only before the action cannot recognize `not to <actor>`. Refine
   actor/action attachment and exclusion handling, with all three strings as
   positive controls.

3. **The active-Task switch matcher rejects the legal completed-Task boundary
   switch.** `conflictsWithActiveTaskSwitch` considers `current` near `Task`
   sufficient proof that a switch occurs inside an active Task and does not
   recognize completion/boundary qualifiers (`validate-contract.lib.mjs:899`).
   The following valid directive returned `B2D-SKILL-005`:

   > Switch the Task Agent after the current Task completes.

   The approved Design expressly permits Task Agent changes after every prior
   Task completes and defers an in-Task request until that boundary
   (`generic-task-agent-design.md:116`, `:221`, `:238`). Preserve rejection of
   the requested `while ... running` and `during the current Task` cases while
   accepting the corresponding `after ... completes` boundary case.

4. **The review-bypass matcher conflates optional document reviewers with
   required Task/Design review and can ignore an explicit prohibition.**
   `conflictsWithRequiredReview` selects the first bypass-like token anywhere
   in a 64-token window, checks negation only around that token, and then looks
   for `primary`/`auxiliary` or `Codex` plus `Design` anywhere else in the
   window (`validate-contract.lib.mjs:921`). Both compliant probes returned
   `B2D-SKILL-005`:

   > Optional user-named Design reviewers do not replace the Codex Design Reviewer.

   > Primary review remains required; optional user-named Design reviewers are document-only.

   The Design explicitly makes user-named Design/Plan reviewers optional while
   keeping Codex mandatory (`generic-task-agent-design.md:152`). In the first
   probe, the matcher chooses unnegated `optional` and never applies `not` to
   `replace`; in the second, it merges two different reviewer roles across a
   semicolon. Bind each bypass term to its review target and evaluate each
   action's own negation. Add both positive controls.

### Minor

None newly found in `5ddf75b0..ef10695a`.

## Blocking Report Disposition

The prior blocking report's concrete false negatives are addressed. A fresh
probe against the production `validateSkillMarkdown` returned exactly
`B2D-SKILL-005` for all of these:

- passive parent ownership: `The Plan is written and updated by the parent.`
- passive high route: `High Tasks are implemented by the Task Agent.`
- running Task switch: `Switch the Task Agent while a Task is running.`
- optional auxiliary review: `After fixing a high Task, auxiliary review is optional.`
- normal primary bypass: `Skip primary review on normal Tasks.`
- omitted auxiliary reviewer: `The auxiliary reviewer may be omitted after a high Task fix.`
- direct route: `Route high Tasks to the Task Agent.`
- direct delegation: `Delegate all high-risk Tasks to Grok.`
- current Task switch: `Switch the Task Agent during the current Task.`

The six producer-added explicit-prohibition controls also remain valid with no
validator failures:

- `The Plan must not be written or updated by the parent.`
- `Never route high Tasks to the Task Agent.`
- `Do not delegate any high-risk Task to Grok.`
- `Do not switch the Task Agent while the current Task is running.`
- `Auxiliary review is not optional and may not be omitted after a high Task fix.`
- `Never skip primary review on normal Tasks.`

Those focused results close the exact prior false-negative list, but the four
Important false-positive classes above make the broadened validator unsafe for
valid contract prose.

## Retained Prior Minors

These remain branch-level debt and are not caused by this fix range:

1. The shared JavaScript/Rust fence detector accepts a backtick opener whose
   info string contains a backtick although CommonMark rejects it.
2. Failed/canceled Simple route-locality coverage still combines both states
   instead of proving each sibling-isolation case independently.

## Verification

- Reviewed `5ddf75b0..ef10695a` against the approved Design, Task 5
  brief/report, the prior blocking re-review, current production Skill, and
  current validator behavior.
- The range contains one commit, `ef10695a fix(skill): reject direct routing
  contradictions`, and changes only
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  and `validate-contract.test.mjs`.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  51 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  production validation passed with 0 failures and reported 418 lines.
- Direct production-validator probe: all 9 contradiction cases returned
  `B2D-SKILL-005`; all 6 producer prohibition controls returned no failures;
  all 7 compliant false-positive probes above returned `B2D-SKILL-005`.
- Prettier check for both changed validator files: passed.
- `git diff --check 5ddf75b0..ef10695a`: passed.
- No Rust command was run. No production file was edited by this review.

## Counts And Verdict

Scoped counts: **Critical 0, Important 4, Minor 0**. Retained non-blocking
branch debt: **2 Minors**.

**Not approved.** The requested contradiction probes now fail and the exact
prohibition controls remain valid, but the replacement grammar rejects
multiple direct statements of the approved ownership, high-route,
boundary-switch, and optional-document-reviewer behavior. All four Important
false-positive classes must be resolved before Task 5 can be approved.
