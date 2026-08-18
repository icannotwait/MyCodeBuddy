# Task 5 Latest Contradiction-Grammar Primary Re-review

## Findings

### Critical

None.

### Important

1. **The widened grammar rejects direct prose that expresses the approved
   workflow, so the scoped fix replaces the false-negative gap with false
   positives.** `hasScopedTask` treats `every` or `all` as contradictory
   without excluding an explicit `normal` scope; the Task Agent route check
   does not distinguish implementation from auxiliary review; the review check
   does not distinguish the valid normal route from a bypass; and the parent
   check attributes a delegated Plan Author action to the parent
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:766`,
   `:838`, `:921`, `:804`). A fresh read-only probe appended these compliant
   directives to the production Skill, and every one incorrectly returned
   `B2D-SKILL-005`:

   - `The Task Agent implements every normal Task.`
   - `Route every high Task auxiliary review to the Task Agent.`
   - `For normal Tasks, use primary review instead of auxiliary review.`
   - `The parent asks the Plan Author to revise the Plan.`

   All four statements restate required v2 behavior: the Task Agent implements
   normal Tasks, supplies high-Task auxiliary review, normal Tasks have only
   the primary reviewer, and the parent coordinates revision through the Plan
   Author. The new positive controls cover only explicit negation of forbidden
   behavior, so they do not catch this regression
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:455`).
   Add compliant active/passive routing and delegated-producer controls, then
   bind scope and action target before reporting a contradiction.

### Minor

None newly introduced in `5ddf75b0..ef10695a`.

## Open-Finding Disposition

**The prior false-negative examples are addressed.** The latest implementation
returns `B2D-SKILL-005` for all six directives from the previous primary
re-review, including passive parent ownership, passive high-Task implementation,
running-Task switching, optional/omitted auxiliary review, and skipped normal
primary review. It also preserves the tested explicit prohibitions.

The result is not yet acceptable because the new scoped false positives above
reject equally direct contract-compliant prose. This is a new validator
behavior defect in the latest fix, not retained historical debt.

## Other Prior Findings

The other final-review findings remain closed:

1. Authoritative document validation still requires routing while lower-level
   markerless parsing remains legacy-compatible.
2. A new generation still requires the complete pending suffix to have empty
   runs, while admitted historical generations remain valid.
3. The installed Skill remains self-contained with the complete policy,
   document shapes, and byte bounds and remains below 500 lines.
4. Malformed `tasks: [null]` still returns deterministic failures rather than
   throwing.

The latest commit changes only the two Task 5 producer-owned JavaScript files;
no ownership breach or unrelated compatibility change was found.

## Retained Prior Minors

These remain separate, non-blocking branch debt:

1. The shared JavaScript/Rust fence detector accepts a backtick opener whose
   info string contains a backtick although CommonMark rejects it.
2. Failed/canceled Simple route-locality coverage combines both states rather
   than proving each sibling-isolation case independently.

The resolved Task 5 `tasks: [null]` Minor is not retained.

## Verification

- Reviewed only `5ddf75b0..ef10695a` against the previous primary blocker and
  confirmed the other prior finding closures against current code.
- Focused `rereview` Node tests: 15 passed, 0 failed.
- Full Node validator suite: 51 passed, 0 failed.
- Production Skill validator: passed with 0 failures and reported 418 split
  lines.
- Prettier check for both validator files: passed.
- `git diff --check 5ddf75b0..ef10695a`: passed.
- No Rust command was run.

## Verdict

**Not approved.** Latest scoped counts: **Critical 0, Important 1, Minor 0**.
Retained non-blocking prior debt: **2 Minors**. The exact prior negative probes
are fixed, but the validator must stop rejecting the approved routes and
producer delegation before Task 5 can pass primary review.
