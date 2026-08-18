# Task 5 Final Fix Round 11 Primary Re-review

## Finding Verdicts

### 1. Concrete and qualified generic Task Agent routing: ADDRESSED

`actorsAfterLink` now admits a bounded actor prefix by structural boundaries
instead of the previous finite modifier whitelist
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1422`,
`:1437`). The three reported high-Task contradictions using `chosen`,
`resolved`, and `invocation-selected Task Agent` now return
`B2D-SKILL-005`; the corresponding normal `chosen Task Agent` control remains
accepted. The committed regression group is at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:1617`.

### 2. Carried active Task with completion timing: ADDRESSED

Completion timing now recognizes `this`/`that` Task anaphors and an implicit
completion subject after a boundary marker
(`validate-contract.lib.mjs:2653`, `validate-contract.lib.mjs:2660`), and the
active-switch check evaluates completion before falling back to the carried
active Task (`validate-contract.lib.mjs:2701`). Both reported legal forms,
`after this completes` and `When complete`, are accepted, while the
pre-completion and active controls reject. The regression group is at
`validate-contract.test.mjs:1644`.

### 3. Reviewer replacement antecedents for `the role` / `same role`: ADDRESSED

`reviewTargetForBypass` now recognizes qualified `reviewer`/`role` noun
phrases and resolves them through the required-reviewer antecedent
(`validate-contract.lib.mjs:2845`, `validate-contract.lib.mjs:2903`). Both
reported replacement contradictions reject, and both explicit negations
accept. The regression group is at `validate-contract.test.mjs:1669`.

### 4. Surplus reviewer cardinality: ADDRESSED

`explicitReviewerCardinality` now recognizes `further` and treats a count
followed by `more` as surplus (`validate-contract.lib.mjs:2147`,
`validate-contract.lib.mjs:2163`). `two more reviewers` and `a further
reviewer` reject, their negated controls accept, and exact `two reviewers`
still accepts for high Tasks. The regression group is at
`validate-contract.test.mjs:1694`.

### 5. Actor alternatives using `rather than` / `instead of`: ADDRESSED

Alternative polarity now starts at the first bounded `rather than` or
`instead of` phrase and excludes every actor later in that alternative list
(`validate-contract.lib.mjs:1449`, `validate-contract.lib.mjs:1472`). The
reported legal `Codex rather than Grok or Gemini` form accepts and the inverse
Grok route rejects. The regression group is at
`validate-contract.test.mjs:1711`.

### 6. Delegated producer predicates across `afterward`: ADDRESSED

This previously addressed finding remains addressed. Delegated bare producer
actions are still kept with the named Plan Author or Design Fixer, while a
repeated/finite parent predicate is rejected by
`actionDelegatesToProducer` (`validate-contract.lib.mjs:1822`). The six-case
accepted/rejected matrix at `validate-contract.test.mjs:1449` remains green,
and the independent matrix reproduced all six classifications.

### 7. Document targets versus role recipients and people pronouns: ADDRESSED

The reported cases are fixed. The new antecedent pass distinguishes plural
document targets from plural people (`validate-contract.lib.mjs:1153`,
`validate-contract.lib.mjs:1198`), and `actionHasDocumentTarget` consumes that
typed result for `them` (`validate-contract.lib.mjs:1650`). `developers
discuss the Plan; parent updates them` now accepts, while `Plan Author lists
the Plan and Design; parent updates them` rejects. The regression group is at
`validate-contract.test.mjs:1739`.

### 8. Bare `Codex Agent` forced into the Task Agent role: ADDRESSED

`directiveActors` now maps only the explicit three-token `Codex Task Agent`
form to `task_agent`; bare `Codex` and `Codex Agent` remain `codex`
(`validate-contract.lib.mjs:1036`). All three reported legal bare-Codex-Agent
sentences accept, while high implementation by `Codex Task Agent` rejects.
The regression group is at `validate-contract.test.mjs:1820`.

### 9. Generic `take*` tokens reject ordinary reviewer actions: ADDRESSED

Bare `take*` forms are no longer review-bypass actions. They enter bypass
validation only when `reviewActionIsReplacement` finds nearby `over`
(`validate-contract.lib.mjs:2750`, `validate-contract.lib.mjs:2763`). Both
reported `takes notes` sentences accept, while `take over for the required
Codex reviewer` rejects. The regression group is at
`validate-contract.test.mjs:1859`.

## New Breakage In The Fix Diff

### Important: broadened actor prefixes attach actors from unrelated `by` relations

`actorRelationPrefixIsValid` now permits every token except a short boundary
denylist (`validate-contract.lib.mjs:1422`). `actorsAfterLink` consequently
attaches the Task Agent in this approval relation to the earlier
implementation predicate (`validate-contract.lib.mjs:1437`):

```text
High Tasks are implemented by Codex after approval by the independently selected Task Agent.
```

This is a compliant Codex implementation with separate Task Agent approval,
but HEAD returns `B2D-SKILL-005`. The fix base accepted it. The change repairs
qualified actor names by admitting arbitrary intervening phrases, but it does
not prove that the actor belongs to the production relation.

### Important: implicit completion binds review/test completion as Task completion

Any completion token within two positions of a boundary marker is treated as
an implicit completion of the carried Task
(`validate-contract.lib.mjs:2660`). These active-Task switches now fail open:

```text
The current Task is running. After review completes, change the Task Agent.
The current Task is running. When testing completes, change the Task Agent.
The current Task is running. Once validation finishes, change the Task Agent.
```

Review, testing, or validation may complete before the Task. The fix base
rejected all three through the carried active state; HEAD accepts all three.
This bypasses the required completed-Task boundary.

### Important: reviewer replacement binding regresses in both directions

`reviewActionIsReplacement` recognizes `take*` only when `over` occurs within
two following tokens (`validate-contract.lib.mjs:2750`), so these direct
replacement statements changed from rejected at the fix base to accepted at
HEAD:

```text
Optional Design reviewers take the role of the required Codex reviewer.
Optional Design reviewers take on the role of the required Codex reviewer.
```

Conversely, `roleReference` treats `the`/`their` anywhere in the two-token
prefix of a later `role` as an anaphor for the prior mandatory reviewer
(`validate-contract.lib.mjs:2845`). This unrelated-role statement changed
from accepted to rejected:

```text
The Codex reviewer is mandatory. User-named Design reviewers replace the advisory role.
```

The change removes the ordinary-action false positive but does not bind the
replacement construction or its target structurally.

### Important: qualified `another` / `additional` primary reviewers fail open

An extra marker is discarded whenever any primary/auxiliary label and actor
follow it (`validate-contract.lib.mjs:2147`,
`validate-contract.lib.mjs:2153`). These explicit duplicate-primary
assertions changed from rejected at the fix base to accepted at HEAD:

```text
High Tasks have another primary Codex reviewer.
Normal Tasks have an additional primary Codex reviewer.
```

Qualifying the surplus reviewer with an expected role does not make it part
of the exact set; `another primary` still asserts a second primary reviewer.

### Important: alternative exclusion leaks past a positive contrast

`alternativeExclusionStart` finds one start for the entire action range, and
every later actor is marked excluded solely by position
(`validate-contract.lib.mjs:1449`, `validate-contract.lib.mjs:1472`). This
illegal unavailable-Agent fallback changed from rejected at the fix base to
accepted at HEAD:

```text
High Tasks are implemented by Codex rather than by Grok, but by Gemini when Codex is unavailable.
```

The positive `but by Gemini` relation is incorrectly swallowed by the earlier
`rather than` polarity. The Skill requires the recorded route to block or use
its recovery rails, not substitute Gemini.

### Important: plural documents override an explicit people recipient

`directivePronounAntecedent` returns `document` before considering a plural
people candidate (`validate-contract.lib.mjs:1216`), without distinguishing a
direct object from a later recipient. This compliant coordination changed
from accepted at the fix base to rejected at HEAD:

```text
The Plan Author sends the Plan and Design to the developers. The parent updates them with review findings.
```

Here `them` refers to the developers, not the sent documents. The fix replaces
the prior actor-presence heuristic with another precedence heuristic and
still misclassifies the parent coordination boundary.

### Important: `alone` / `solely` are treated as global reviewer-set quantifiers

`reviewStatementIsExhaustive` now treats `alone`, `only`, or `solely`
anywhere in the action segment as exhausting the reviewer set
(`validate-contract.lib.mjs:2243`). This complete legal high-review route
changed from accepted at the fix base to rejected at HEAD:

```text
High Tasks are reviewed by Codex for findings solely about correctness. The Task Agent provides the auxiliary review.
```

`solely` qualifies the finding scope, not the reviewer set. The first clause
is incorrectly read as an exhaustive Codex-only review despite the explicit
auxiliary review in the next sentence.

### Important: ordinary modifiers hide postposed required-reviewer absence

Postposed absence is recognized only when every intervening token belongs to
`POSTPOSED_REVIEW_SUBJECT_MODIFIERS`
(`validate-contract.lib.mjs:2084`). This direct incomplete high-review route
changed from rejected at the fix base to accepted at HEAD:

```text
High Tasks are reviewed by Codex and the Task Agent reviewer is definitely omitted.
```

The ordinary adverb `definitely` breaks the finite modifier check, so the
missing required auxiliary reviewer is ignored.

No new Critical or Minor breakage was found in the scoped fix diff.

## Out-of-Scope Observations

- The retained CommonMark backtick-info-string Minor is unchanged and is not
  counted.
- The retained combined rather than isolated failed/canceled route-locality
  coverage Minor is unchanged and is not counted.
- `Optional Design reviewers assume the role of the required Codex reviewer.`
  is accepted by both the fix base and HEAD. This is a pre-existing
  contradiction-classifier gap, so it is recorded here as non-blocking and
  not counted against this fix diff.
- The fix changes only the validator library and Node tests. Structured
  routing, risk, generation, progress, and lineage code was not changed.
- No Rust command was run, and no default `tauri-runtime` feature was enabled.

## Verification

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: 238 tests, 4 suites, 238 passed, 0 failed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
  - PASS: 0 failures, 1 check; Skill line count 418.
- `pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
  - PASS: both files use Prettier style.
- `git diff --check 943bfc291e7fa30d49c94b845e3528ba415a85a3..94ba94f92b914b1dec4b5eb7833146bea28d1c33`
  - PASS: exit 0, no output.
- Independent previous-finding matrix
  - PASS: 30/30 classifications correct across all nine verdict groups.
- Independent committed base-to-HEAD differential matrix
  - FAIL: 13/13 scoped regression probes reproduced the eight new breakage
    classes above at `943bfc29` and `94ba94f9`.
- No Rust command was run, as required.

## Severity Counts

**Critical 0 / Important 8 / Minor 0**

All nine previous findings are addressed, but the fix introduces eight new
Important contradiction-classifier regressions.

## Final Verdict

**NOT APPROVED**
