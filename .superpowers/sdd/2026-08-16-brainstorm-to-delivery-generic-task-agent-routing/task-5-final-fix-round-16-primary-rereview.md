# Task 5 Final Fix Round 16 Primary Re-review

## Finding Verdicts

No Round-15 report contains a Critical finding. The verdicts below preserve
the reports' order. Overlapping auxiliary items are mapped explicitly, and
every distinct reproducer is retained.

### Prior primary carried-forward Group 1: Component/action ambiguity - ADDRESSED

The exact `review open issues` and `test running services` actions remain
accepted at `validate-contract.test.mjs:3017` and
`validate-contract.test.mjs:3022`, while the unfinished component controls in
the same Round-15 group remain rejecting. The current object classifier is at
`validate-contract.lib.mjs:3958`. The focused Round-15 group passed at HEAD.

### Prior primary carried-forward Group 2: Take-role target identity - ADDRESSED

The `aforementioned`, `previously assigned`, and `previously designated`
reviewer objects still reject at `validate-contract.test.mjs:3067`,
`validate-contract.test.mjs:3072`, and `validate-contract.test.mjs:3077`.
Explicit contact-person objects remain accepted at
`validate-contract.test.mjs:3177` and `validate-contract.test.mjs:3182`.
Target resolution remains in `validate-contract.lib.mjs:4643`.

### Prior primary carried-forward Group 3: People-relation boundaries - ADDRESSED

The scoped possessive/nested relations, genuine `after consulting`
participant, and `so that` subordinate-clause cases remain classified by the
Round-15 group beginning at `validate-contract.test.mjs:3268`, including the
participant at line 3297 and subordinate control at line 3357. The current
relation check is at `validate-contract.lib.mjs:1748`.

### Prior primary carried-forward Group 4: Absence transitivity - ADDRESSED

Bare `lacking entirely` and `lacking completely` still reject at
`validate-contract.test.mjs:3712` and `validate-contract.test.mjs:3717`, while
the transitive `missing all context and evidence` and `missing Friday's
deadline` controls accept at lines 3747 and 3752. The current direct-object
decision is at `validate-contract.lib.mjs:1709`.

### Prior primary carried-forward Group 5: Subject-first switching - ADDRESSED

`The active Task is running. The Task Agent switches immediately.` remains a
rejecting control at `validate-contract.test.mjs:3782`. The current
subject-before-predicate path is at `validate-contract.lib.mjs:4478`.

### Prior primary carried-forward Group 6: Ordered completion - ADDRESSED

The later full completion remains accepted at
`validate-contract.test.mjs:3787`; its completion-local qualifier scan remains
at `validate-contract.lib.mjs:4148`. The focused Round-15 group passed at HEAD.

### Prior primary carried-forward Group 7: Carried-state adjunct pronouns - ADDRESSED

The direct Task-anaphor and separate-service controls in the Round-15 group
beginning at `validate-contract.test.mjs:3763` retain their required results,
including the `integration service itself` control at line 3817. Current Task
subject resolution starts at `validate-contract.lib.mjs:3811`.

### Primary Important 1: Mass action objects and unrelated possessives - ADDRESSED

Both distinct primary reproducers are covered. `test running software` accepts
at `validate-contract.test.mjs:4170`, and `the server's review is still
running` accepts at line 4195. The action-object path is at
`validate-contract.lib.mjs:3958`; the current possessive-owner decision is at
line 3902.

### Primary Important 2: Reviewer modifier versus explicit role head - ADDRESSED

The exact `previously assigned reviewer contact person` target accepts at
`validate-contract.test.mjs:4230`; `note taker` accepts at line 4235, while the
bare anaphoric reviewer still rejects at line 4240. The trailing-head check is
at `validate-contract.lib.mjs:4618` and gates anaphoric fallback at line 4700.

### Primary Important 3: Role modifiers on Plan documents - ADDRESSED

The exact `reviewer and producer Plans` reproducer rejects at
`validate-contract.test.mjs:4270`, while the genuine people participant
control accepts at line 4275. Role/document disambiguation is implemented at
`validate-contract.lib.mjs:2084`.

### Primary Important 4: Modified `lacking in` complement - ADDRESSED

The exact `lacking completely in experience` reproducer accepts at
`validate-contract.test.mjs:4315`; the reported `critical context`, `entirely`,
and `severely` variants accept at lines 4320-4335. The modified-complement
branch is at `validate-contract.lib.mjs:1726`.

### Primary Important 5: Modal, temporal, and reflexive subject-first switches - ADDRESSED

The `will`, `must`, `should`, `can`, and `may` forms reject through the matrix
at `validate-contract.test.mjs:4353`; `then switches` and `itself switches`
reject at lines 4359 and 4364. The bridge vocabulary is defined at
`validate-contract.lib.mjs:897` and applied at line 4478.

### Primary Important 6: Explicit non-Task reporting and restart subjects - ADDRESSED

The exact separate-server reporting and restart reproducers accept at
`validate-contract.test.mjs:4419` and `validate-contract.test.mjs:4424`.
Direct Task-pronoun controls still reject at lines 4444-4455. The new subject
guards are at `validate-contract.lib.mjs:3734` and line 3748.

### Auxiliary Important 1: Singular and mass imperative objects - ADDRESSED

This overlaps Primary Important 1, but its distinct reproducers are retained:
`please review pending work`, `please test running code`, and the singular
`pending issue` forms accept at `validate-contract.test.mjs:4175`, line 4180,
and lines 4185-4190. The exact auxiliary cases are therefore addressed.

### Auxiliary Important 2: Purpose clauses becoming people recipients - ADDRESSED

The `in order that`, `in order for`, and participial `allowing` reproducers
reject at `validate-contract.test.mjs:4280`, line 4285, and line 4295. A genuine
`with both reviewers` participant remains accepted at line 4305. Purpose
bounding is at `validate-contract.lib.mjs:1807`.

### Auxiliary Important 3: Modified `lacking in` complement - ADDRESSED

This overlaps Primary Important 4. Its distinct `completely/entirely in
critical context` and `in experience` forms are all accepted at
`validate-contract.test.mjs:4315` through line 4330, while objectless `lacking
completely` rejects at line 4340.

### Auxiliary Important 4: Modal subject-first Agent changes - ADDRESSED

This overlaps Primary Important 5. The auxiliary `will switch` and `must
switch` reproducers are covered by the modal matrix at
`validate-contract.test.mjs:4353`; the negated modal and completed-Task
controls remain accepted at lines 4374 and 4379.

### Auxiliary Important 5: `later` and `afterward` full completion - ADDRESSED

Both distinct completion connectors accept at
`validate-contract.test.mjs:4394` and `validate-contract.test.mjs:4399`.
Partial-only and later-reactivation controls still reject at lines 4404 and
4409. The connectors are admitted at `validate-contract.lib.mjs:943` and used
by completion binding at line 4261.

### Auxiliary Important 6: Object pronouns inside service adjuncts - ADDRESSED

This overlaps Primary Important 6 but has distinct monitoring/tracking
reproducers. `integration service monitoring it`, `server tracking it`, and
`monitoring only it` accept at `validate-contract.test.mjs:4429`, line 4434,
and line 4439; direct Task pronouns still reject at lines 4444-4455.

### Auxiliary required-group verdict mapping

- Group 1 - ADDRESSED. Deduplicated with carried-forward Group 1, Primary
  Important 1, and Auxiliary Important 1; all distinct mass, singular,
  possessive, and plural action reproducers above were retained.
- Group 2 - ADDRESSED. Deduplicated with carried-forward Group 2 and Primary
  Important 2.
- Group 3 - ADDRESSED. Deduplicated with carried-forward Group 3, Primary
  Important 3, and Auxiliary Important 2.
- Group 4 - ADDRESSED. Deduplicated with carried-forward Group 4, Primary
  Important 4, and Auxiliary Important 3.
- Group 5 - ADDRESSED. Deduplicated with carried-forward Group 5, Primary
  Important 5, and Auxiliary Important 4.
- Group 6 - ADDRESSED. Deduplicated with carried-forward Group 6 and Auxiliary
  Important 5.
- Group 7 - ADDRESSED. Deduplicated with carried-forward Group 7, Primary
  Important 6, and Auxiliary Important 6.

## New Breakage in the Fix Diff

### Important 1: Imperative heuristics erase explicit unfinished Task status

```text
After completion of the active Task (please note its review pending), switch the Task Agent.
After completion of the active Task: the Task's test running overnight, switch the Task Agent.
```

Both statements explicitly retain an unfinished Task component, so both Agent
switches must reject. The fix base rejects both; HEAD accepts both.

`componentHasActionModifier` now treats any preceding `please` as proof that
the component is an action, even in the reporting phrase `please note its
review pending` (`validate-contract.lib.mjs:4031`). Separately,
`testRunningObject` treats any post-boundary `test ... running` form as an
imperative object without checking the explicit `Task's test` subject
(`validate-contract.lib.mjs:3983`). The punctuation-only imperative paths at
lines 3969-4007 make the same status/action distinction overly broad.

### Important 2: A nested Task owner is treated as an unrelated possessor

```text
The active Task is completed but the Task's primary reviewer's mandatory review is still running. Then switch the Task Agent.
```

The mandatory review belongs to the Task's primary reviewer and is therefore
an unfinished Task review. The fix base rejects the switch; HEAD accepts it.

The new possessive scan returns non-Task as soon as it sees any possessive
owner other than the literal token `task` (`validate-contract.lib.mjs:3902`).
It sees the nested `reviewer's` owner and discards the outer `Task's`
relationship instead of resolving the possessive chain.

### Important 3: A reviewer postmodifier is mistaken for a new role head

```text
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer on duty.
```

`on duty` modifies the same previously assigned reviewer, so this replaces the
mandatory reviewer and must reject. The fix base rejects it; HEAD accepts it.

`reviewerModifierHasTrailingRoleHead` treats any unlisted token after
`reviewer` as a concrete unrelated role (`validate-contract.lib.mjs:4618`).
`on` is not an adjunct link, so the helper returns true and suppresses the
required-review antecedent at `validate-contract.lib.mjs:4700`. The same
regression reproduces with `reviewer still responsible`.

### Important 4: Role/document disambiguation crosses punctuation

```text
The developers revise the Design after consulting both reviewer and producer (Plan work begins later). The parent updates both of them on progress.
```

`updates both of them on progress` unambiguously refers to the consulted people,
so this is legal parent coordination and must accept. The fix base accepts it;
HEAD rejects it.

`peopleRoleModifiesDocument` scans up to four later tokens and accepts only
role/coordinator tokens between the role and a document head, but it receives
no punctuation metadata (`validate-contract.lib.mjs:2084`). It therefore
attaches the parenthetical `Plan` to both earlier people roles, removes the
people antecedent at line 2112, and lets the document antecedent win.

### Important 5: Reporting and participial adjuncts suppress a Task anaphor

```text
The active Task is completed but, according to its own telemetry, it is reported that it is still running. Then switch the Task Agent.
The active Task is completed but, according to its own telemetry, after restarting, it is still running. Then switch the Task Agent.
```

In both statements `its own telemetry` fixes the antecedent as the Task, and
the final `it` says that Task is running. Both switches must reject. The fix
base rejects both; HEAD accepts both.

`adjunctFinalItHasNonTaskOwner` declares the final pronoun non-Task when any
reporting predicate appears earlier, or when any earlier `-ing` token has only
allowed trailing modifiers (`validate-contract.lib.mjs:3734`). It does not
distinguish passive reporting or a comma-delimited `after restarting` adjunct
from a non-Task subject. That broad result suppresses the Task anaphor at
`validate-contract.lib.mjs:3839`.

### Important 6: Restart shadowing ignores a transitive Task object

```text
The Task is completed but the server restarts it and it is still running. Then switch the Task Agent.
```

The first `it` is the Task object of `restarts`, and the second `it` continues
that Task antecedent. The Task is active, so the switch must reject. The fix
base rejects it; HEAD accepts it.

`stateSegmentHasExplicitNonTaskSubject` inspects only the tokens before the
restart predicate and records `server` as the owner
(`validate-contract.lib.mjs:3748`). It never checks the predicate's explicit
Task object, so the later Task pronoun is discarded at lines 3890-3899. The
new exact control covers intransitive `the server restarts and it ...`, but not
this transitive boundary.

## Out-of-Scope Observations

The retained Task 2 CommonMark fence Minor and Task 4 failed/canceled
projection-locality Minor are outside this Round-16 diff and were not
reassessed. Candidate statements whose classification did not change from the
fix base were excluded from the new-breakage counts.

## Verification

- Confirmed HEAD `0934287082cccaeb9042418803a1d1af26fc3e0a` and fix base
  `e7da74d9113511efd163536d2006db6fa7efeed2`.
- Read the complete Task brief, producer report through Final Fix Round 16,
  both complete Round-15 re-reviews, and the complete 1,205-line scoped diff
  package.
- Confirmed the scoped range changes only
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  and `validate-contract.test.mjs`.
- Ran the focused Round-15 tests: 5 tests, 5 pass, 0 fail, exit 0.
- Ran the focused Round-16 tests: 7 tests, 7 pass, 0 fail, exit 0.
- Ran an in-memory Git-object differential probe for the concrete doubts raised
  by code reading. All 8 decisive probes across the six new groups were
  base-correct and HEAD-wrong; no test-output file was created.
- `git diff --check e7da74d9..09342870` produced no diagnostics.
- The producer's full 278-test suite, production validator, syntax checks, and
  formatting results were treated as claims and were not rerun.
- No Rust command was run, and default `tauri-runtime` was never enabled.
- The tracked worktree and index were clean before this ignored report was
  written. No tracked file, index entry, commit, branch, or HEAD state was
  changed.

## Severity Counts

Prior findings under verification: **Critical 0 / Important 0 / Minor 0**
outstanding; every prior scoped finding/group is addressed.

New fix-diff breakage: **Critical 0 / Important 6 / Minor 0**.

Final severity counts: **Critical 0 / Important 6 / Minor 0**.

## Final Verdict

**NOT APPROVED**
