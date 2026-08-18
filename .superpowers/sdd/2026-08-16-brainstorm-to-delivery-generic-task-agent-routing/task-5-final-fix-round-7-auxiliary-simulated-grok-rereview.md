SIMULATED GROK WORKFLOW TEST DOUBLE ONLY

# SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: Task 5 Final Fix Round 7 Auxiliary Re-review

This is a simulated workflow test double, not a real Grok model verdict.

## Scope

Reviewed `7173d031..6c2e0ca4a688dacd04b87826ddb89e3dd6fa92a4` at HEAD
`6c2e0ca4a688dacd04b87826ddb89e3dd6fa92a4` in the assigned worktree. The
tracked range changes only the validator library and its Node tests. This
review did not edit production files, tests, Design, Plan, progress, or other
reports. Only this ignored report was created.

The exact embedded v2 contract, routing/progress agreement, markerless parser
compatibility, pending-generation boundary checks, timing controls, producer
delegation controls, and reviewer-antecedent controls were re-read and
rechecked. No Rust command was run.

## Findings

### Critical

None.

### Important

1. **Coordinated passive and direct-route actor lists bind only the first
   actor, allowing extra implementers/reviewers and route targets.**

   `actorAfterLink` returns a single actor after a `by`/`to` link, and
   `directPassiveActorsForAction` and `routeTargetsForAction` do not bind
   actors coordinated by a later `and` or comma
   ([`validate-contract.lib.mjs`](../../../.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1141),
   [`validate-contract.lib.mjs`](../../../.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1147),
   [`validate-contract.lib.mjs`](../../../.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1300)).
   `conflictsWithTaskAgentRoute` therefore sees only the first, often valid,
   actor/target. These clear contradictions all return `ACCEPT` rather than
   `B2D-SKILL-005`:

   ```text
   Normal Tasks are reviewed by Codex and Grok.
   Normal Tasks are reviewed by Codex and the Task Agent.
   Normal Tasks are implemented by Grok and Codex.
   High Tasks are implemented by Codex and Grok.
   High Tasks are implemented by Codex and the Task Agent.
   Route high Tasks to Codex and Grok.
   Route high Tasks to Codex and the Task Agent.
   Delegate high Tasks to Codex and Grok.
   Delegate normal Tasks to Grok and Codex.
   ```

   The order dependence is observable: the corresponding sentences beginning
   with `Grok`/the `Task Agent` reject because the first bound actor exposes the
   contradiction, while the valid actor first followed by an extra actor is
   accepted. High implementation has one Codex producer, normal implementation
   has one selected Task Agent producer, normal review has one Codex primary,
   and direct high/normal implementation routes cannot add a second target.
   Bind the complete coordinated actor/target relation before applying the
   route table while preserving the existing valid high Codex plus Task Agent
   review controls.

2. **Reviewer slot and cardinality contradictions remain accepted.**

   `routeReviewPurpose` identifies `primary`/`auxiliary`, but the route check
   only rejects a normal Task Agent reviewer and a high auxiliary Codex or
   primary Task Agent; it does not reject an auxiliary slot on a normal Codex
   route or enforce the exact reviewer set
   ([`validate-contract.lib.mjs`](../../../.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1272),
   [`validate-contract.lib.mjs`](../../../.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1484)).
   The required-review matcher likewise has no bounded structural handling for
   `only`/`no` reviewer cardinality statements
   ([`validate-contract.lib.mjs`](../../../.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1777)).
   All of these direct contradictions return `ACCEPT`:

   ```text
   Route normal Tasks to Codex for auxiliary review.
   Normal Tasks are reviewed by Codex in the auxiliary slot.
   Normal Tasks route auxiliary review to Codex.
   Normal Tasks have an auxiliary reviewer.
   Normal Tasks have primary and auxiliary reviewers.
   High Tasks are reviewed only by Codex.
   High Tasks are reviewed by Codex and no other reviewer.
   High Tasks have no auxiliary reviewer.
   High Tasks have Codex primary and no auxiliary reviewer.
   High Tasks use Codex as the only reviewer.
   ```

   The exact route requires normal Tasks to have only the Codex primary
   reviewer and high Tasks to have both Codex primary and selected Task Agent
   auxiliary review. Add slot/cardinality binding without turning the existing
   explicit prohibitions (`Never skip...`, `Do not omit...`) into false
   positives.

### Minor

None newly found in this range.

## Prior Finding Disposition

The four Round-5 Important classes remain closed in their exact and nearby
controls: finite parent predicates after delegated producer infinitives,
high/normal Task scope and actor order for the tested forms, active/pre-
completion timing, and reviewer antecedents across conjunction, contrast,
semicolon, pronoun, passive, and replacement wording. The Round-3, Round-4,
Round-6, and passive-actor regression controls also pass.

The authoritative document validator still fails closed on markerless routing
while lower-level parsing remains compatible. New generation adoption still
requires an empty pending suffix, malformed nested Tasks/runs produce failures
without throwing, and the production Skill retains its complete policy and
document shapes.

Retained non-blocking branch debt, not included in this scoped Minor count:

1. The shared JavaScript/Rust fence detector accepts a backtick opener whose
   info string contains a backtick although CommonMark rejects it.
2. Failed/canceled Simple route-locality coverage combines both states rather
   than proving each sibling-isolation case independently.

## Verification

- Full Node validator suite: `146` passed, `0` failed.
- Production validator: PASS, `0` failures; production `SKILL.md` line count
  `418`.
- Prettier check for both validator files: PASS.
- `git diff --check 7173d031..6c2e0ca4a688dacd04b87826ddb89e3dd6fa92a4`: PASS.
- Independent bounded pressure matrix: `10/29` expected classifications
  passed. All 10 legal controls passed; all 9 coordinated actor/target
  contradiction probes and all 10 reviewer-slot/cardinality contradiction
  probes were false negatives.
- Malformed routing/progress and randomized nested null/malformed input probes
  returned deterministic failures without throws.
- No Rust command was run.

## Verdict

**SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: NOT APPROVED.** Critical: 0;
Important: 2; Minor: 0 newly found. Retain the two prior non-blocking Minors
listed above. This is not a real Grok verdict.
