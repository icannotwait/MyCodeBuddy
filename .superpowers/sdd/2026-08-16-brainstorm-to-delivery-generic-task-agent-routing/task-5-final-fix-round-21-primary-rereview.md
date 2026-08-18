# Final Fix Round 21 Primary Re-review

## Finding Verdicts

### 1. Direct Task-object attachment must survive trailing adjuncts: ADDRESSED

The binding group requires the direct `monitors the Task for diagnostics`
case to reject while the inverse `near the Task` and `reports on the Task`
relations accept, with the complete Round-20 direct/compound/prepositional
matrix retained
(`task-5-final-fix-round-21-findings.md:6-23`).

The fix separates the relation immediately before the Task object from the
first significant trailing relation. A preceding non-object link still makes
the Task indirect, while an absent trailing head or a trailing prepositional
link preserves a direct Task object
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3907`,
`:3924`, `:3927`). The non-object link set now explicitly includes `near` and
`on`, in addition to inherited `for`
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:988`).
The focused expectations cover the required rejecting reproducer, both named
inverse relations, `Task worker`, `Task log`, `for the Task`, and the two
terminal direct controls
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5102`).

### 2. Direct parenthetical Task objects allow ordinary qualifiers: ADDRESSED

The binding group requires `their`, `our`, `your`, and `that` to remain on the
direct Task-object path while retaining the bare affirmative and prior
indirect/negated controls
(`task-5-final-fix-round-21-findings.md:25-38`).

All four qualifiers were added to the existing direct-object qualifier set
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:976`).
The reactivation-object helper continues to require an affirmative recognized
predicate and permits only those qualifiers between the predicate and Task
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4024`).
The new expectation matrix covers all four required forms and repeats the bare
`the Task` rejection
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5126`).
The immediately preceding unchanged Round-20 controls remain present, including
indirect/compound acceptances and the bare affirmative rejection
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5088`,
`:5094`).

### 3. Parenthetical negation must bind locally: ADDRESSED

The binding group requires `not idling before restarting the Task` to reject,
while directly scoped `not restarting the Task` and `without restarting the
Task` continue to accept
(`task-5-final-fix-round-21-findings.md:40-50`).

`actionIsNegated` now accepts a caller-selected boundary set while preserving
its previous default
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1959`).
The Task-reactivation caller supplies the union of Task clause and carried
adjunct boundaries, including `before`
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:956`,
`:964`, `:4031`). Thus the earlier `not idling` is outside the restart
predicate's local lookback, while direct `not`/`without` remain inside it. The
focused expectations exercise the required rejection and both acceptance
controls
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5135`).

## New Breakage in the Fix Diff

No new Critical or Important breakage found in the scoped diff.

The relation change remains limited to direct Task-antecedent classification;
the focused caller check shows that a preceding `near`/`on`/`for` link remains
indirect, while a trailing such link is treated as an adjunct. The negation
change preserves the shared helper's default boundary behavior and narrows only
the Task-reactivation call. No conflicting active-Task directive was found that
the changed paths would newly accept, and no material compliant Skill form was
found that they would newly reject.

Test-expectation integrity is intact. The scoped package reports 49 additions
and no deletions in the test file, and its complete test hunk is additive; it
does not remove, weaken, or relabel an existing expectation
(`review-1081eda2..21401f42.diff:6`, `:183`, `:187`). The new expectations use
the existing `reject: true`/`reject: false` meanings consistently
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5102`,
`:5126`, `:5135`).

## Out-of-Scope Observations

The producer report retains two pre-existing whole-branch Minor observations:
the CommonMark backtick-info-string detector behavior and combined
failed/canceled projection-locality coverage (`task-5-report.md:2624`). Neither
is introduced or modified by this two-file scoped diff, so neither affects this
gate.

## Verification Performed

- Read the binding Round-21 findings completely.
- Read the complete `## Final Fix Round 21` producer-report section. Its test,
  formatter, syntax, diff, and scope results were treated as producer claims;
  none were rerun.
- Read the supplied commit/stat/complete scoped diff package once. It contains
  one commit and only the two permitted validator files
  (`review-1081eda2..21401f42.diff:1`, `:3`, `:6`).
- Performed focused unchanged-code checks only for two named risks: whether the
  inherited relation links include `for`, and whether the new Task-specific
  negation boundary set could consume direct negation. `for` is inherited from
  `PEOPLE_ANTECEDENT_LINKS`
  (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:449`),
  while neither `not` nor `without` is in the Task boundary sets
  (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:881`,
  `:956`).
- Ran no classification probe, no test suite, no Rust command, and did not
  enable default `tauri-runtime`.
- Made no tracked-state, index, HEAD, or branch changes. This report is the only
  written artifact and is at the requested ignored path.

## Severity Counts

- Critical: 0
- Important: 0
- Minor: 0 scoped (2 retained out-of-scope observations)

## Final Verdict

APPROVE. All three binding finding groups are ADDRESSED, test-expectation
integrity is preserved, and no scoped Critical or Important breakage remains.
