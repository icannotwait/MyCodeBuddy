# Task 5 Final Fix Round 17 Primary Re-review

## Finding Verdicts

The Round 16 source reports contain Critical 0 and Important 13 source
findings, deduplicated by the controller into the following 11 groups. The
groups remain in the required order and retain every source reproducer.

### Important 1: Imperative heuristics erase explicit unfinished Task status - ADDRESSED

```text
After completion of the active Task (please note its review pending), switch the Task Agent.
After completion of the active Task: the Task's test running overnight, switch the Task Agent.
```

Both exact reproducers reject at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4475`.
The required accepted imperative controls remain at lines 4487-4500. The fix
binds the component to its local punctuation segment and owner at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3775`
and limits the action interpretation at lines 4064-4116 and 4142-4169. The
focused Round 17 run passed this group.

### Important 2: Possessive scope hides Task-owned components - ADDRESSED

```text
The active Task is completed but the Task's primary reviewer's mandatory review is still running. Then switch the Task Agent.
The active Task is completed but, despite the server's warning, the review is still running. Then switch the Task Agent.
The active Task is completed but, following the server's report, the validation is still running. Then switch the Task Agent.
```

The exact reproducers reject at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4505`.
The direct unrelated-owner control and two neighboring controls remain at
lines 4522-4535. `taskComponentOwner` now follows modifier-only possessive
chains at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3784`,
and its result is applied at lines 4012-4016 and 4119-4124. The focused run
passed this group.

### Important 3: Purpose verbs suppress genuine people objects - ADDRESSED

```text
The developers revise the Plan, Design, and code to enable both reviewers. The parent updates both of them.
The developers revise the Plan, Design, and code by allowing both reviewers to participate. The parent updates both of them.
```

Both exact reproducers accept at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4540`.
The three required true-purpose controls still reject at lines 4552-4565.
The direct-object distinction is implemented at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1827`.
The focused run passed this group.

### Important 4: Reviewer postmodifiers are mistaken for explicit role heads - ADDRESSED

```text
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer on duty.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer with long tenure.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer assigned earlier.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer still responsible.
```

All four exact reproducers reject in the generated matrix at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4570`.
The real `contact person` and `note taker` role-head controls still accept at
lines 4581-4589. The target-tail boundaries and modifier guard are at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4823`.
The focused run passed this group.

### Important 5: Role/document disambiguation crosses punctuation - ADDRESSED

```text
The developers revise the Design after consulting both reviewer and producer (Plan work begins later). The parent updates both of them on progress.
```

The exact reproducer accepts at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4594`.
The direct `reviewer and producer Plans` control still rejects at lines
4601-4604. Punctuation now bounds the document-head lookup at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2122`,
and the metadata is propagated to people antecedent resolution at lines
2144-2153. The focused run passed this group.

### Important 6: Subject-first changes to unrelated objects look like Agent replacement - ADDRESSED

```text
The active Task is running. The Task Agent will switch branches immediately.
The active Task is running. The Task Agent can change directories.
The active Task is running. The Task Agent should replace a file.
The active Task is running. The Task Agent may switch the logging mode.
```

All four exact reproducers accept in the matrix at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4614`.
The modal, reflexive, and temporal actual-switch controls still reject at lines
4625-4638. The post-predicate object check is at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4614`
and gates the subject-first result at line 4695. The focused run passed this
group.

### Important 7: A later transitive completed action becomes full Task completion - ADDRESSED

```text
The active Task is partially complete and later completed documentation. Then switch the Task Agent.
The active Task is partially complete and afterward completed the migration. Then switch the Task Agent.
```

Both exact reproducers reject at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4643`.
The full-completion, partial-completion, and reactivation controls remain at
lines 4655-4673. The new direct-object check is at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4329`
and is applied by `completionBelongsToTask` at lines 4376-4378. The focused
run passed this group.

### Important 8: Reporting and participial adjuncts suppress a Task anaphor - ADDRESSED

```text
The active Task is completed but, according to its own telemetry, it is reported that it is still running. Then switch the Task Agent.
The active Task is completed but, according to its own telemetry, after restarting, it is still running. Then switch the Task Agent.
The active Task is completed but according to ongoing monitoring it is still running. Then switch the Task Agent.
```

All three exact reproducers reject at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4678`.
The explicit server and tracking-service controls remain accepted at lines
4695-4703. The reporting/participial owner lookup is at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3816`.
The focused run passed this group.

### Important 9: An explicit Task reporting subject is discarded - ADDRESSED

```text
The active Task is completed but, according to telemetry, the Task says that it is still running. Then switch the Task Agent.
```

The exact reproducer rejects at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4708`.
The explicit server subject remains accepted and the direct Task pronoun still
rejects at lines 4715-4723. The owner lookup at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3818`
retains an explicit `task` subject. The focused run passed this group.

### Important 10: Restart shadowing ignores a transitive Task object - ADDRESSED

```text
The Task is completed but the server restarts it and it is still running. Then switch the Task Agent.
```

The exact reproducer rejects at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4728`.
The intransitive, reflexive-server, and subordinate-pronoun controls remain
accepted at lines 4735-4748. The directly governed Task-object check is at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3841`.
The focused run passed this group.

### Important 11: A preposed gerund adjunct hides an explicit Task restart - ADDRESSED

```text
The active Task is completed but, with monitoring complete, the Task restarts. Then switch the Task Agent.
The active Task is completed but, with testing complete, the Task restarts. Then switch the Task Agent.
```

Both exact reproducers reject at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4753`.
The corresponding explicit server controls remain accepted at lines
4765-4773. `taskMentionIsNestedInNonTaskSubject` now restarts its prefix after
the local punctuation boundary at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3879`.
The focused run passed this group.

## New Breakage in the Fix Diff

### Important 1: Qualified unrelated possessives are rebound to the Task

These legal completed-boundary statements were accepted at the fix base but
are rejected at HEAD:

```text
The active Task is completed but the server's code review is still running. Then switch the Task Agent.
The active Task is completed but the server's integration test is still running. Then switch the Task Agent.
```

Both components are directly owned by `server`, so they must behave like the
retained legal control `the server's review is still running`. At
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3791`,
`attachmentIsModifierOnly` accepts only a narrow modifier vocabulary. The
ordinary compound modifiers `code` and `integration` make it return false, and
line 3804 then returns `implicit-task` instead of `other`. Callers at lines
4012-4016 and 4119-4124 consequently treat the server-owned component as an
unfinished Task component. The focused Git-object differential probe was
base-correct and HEAD-wrong for both statements.

### Important 2: Agent profile or identity changes are treated as unrelated objects

These active-Task directives were rejected at the fix base but are accepted at
HEAD:

```text
The active Task is running. The Task Agent will switch profiles immediately.
The active Task is running. The Task Agent will change its selected profile immediately.
```

A profile is part of the selected Task Agent identity and canonical route key,
as shown by `profile_id` in route derivation at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:5552`
and the keys at lines 5579-5587. It therefore cannot change inside an active
Task. The new generic object scan at lines 4614-4648 treats `profiles` or
`profile` as proof that the change is unrelated to the Agent. The subject-first
guard at line 4695 then suppresses the contradiction. This fixes branches and
files by making every noun an exemption, but fails open for the identity field
the boundary protects. The focused differential probe was base-correct and
HEAD-wrong for both statements.

### Important 3: Completion adjuncts are mistaken for transitive objects

These genuine full-completion boundaries were accepted at the fix base but are
rejected at HEAD:

```text
The active Task is partially complete and later completed without errors. Then switch the Task Agent.
The active Task is partially complete and later completed yesterday. Then switch the Task Agent.
```

`completionHasDirectObject` at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4329`
classifies the first unrecognized post-completion token as an object. `without`
and the temporal adverb `yesterday` are outside its small preposition/modifier
sets, so line 4378 discards the later full completion. The earlier partial state
then remains effective and the otherwise legal boundary is rejected. The
focused differential probe was base-correct and HEAD-wrong for both statements.

## Out-of-Scope Observations

- The retained Task 2 CommonMark fence Minor and Task 4 failed/canceled
  projection-locality Minor are outside this Round 17 diff and were not
  reassessed or included in the counts.
- The scoped changes remain confined to the bounded contradiction parser and
  its tests; structured routing, risk, generation, progress, lineage, Skill
  prose, and Rust surfaces were not changed.

## Verification Performed

- Read the complete Task brief, Round 17 findings, producer report through
  Round 17, scoped 983-line review package, and both complete Round 16 source
  re-reviews.
- Confirmed HEAD is
  `c2fd394b94494719f0c92af1fdeaff70e592b1a0` and the fix base is
  `0934287082cccaeb9042418803a1d1af26fc3e0a`.
- Confirmed the range changes only
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  and `validate-contract.test.mjs`.
- Ran only the focused Round 17 test pattern: 11 tests, 11 passed, 0 failed.
- Ran one focused in-memory base/HEAD Git-object differential matrix for the
  code-reading doubts above: 6 of 6 probes were base-correct and HEAD-wrong.
- Confirmed `git diff --check` reports no diagnostics.
- Confirmed the test diff has 303 additions and 0 deletions. No existing test
  expectation was removed, weakened, or relabeled to obtain GREEN.
- Confirmed the tracked worktree and index were clean before writing this
  ignored report.
- Did not rerun the full Node suite, production validator, formatter, or any
  Rust command. Their reported outcomes remain implementer claims. The default
  `tauri-runtime` feature was never enabled.

## Severity Counts

Findings under verification: **Critical 0 / Important 0 / Minor 0**
outstanding; all 11 required groups are addressed for every source reproducer.

New scoped fix-diff breakage: **Critical 0 / Important 3 / Minor 0**.

Final counts: **Critical 0 / Important 3 / Minor 0**.

## Final Verdict

**NOT APPROVED**

All 11 reported finding groups are addressed, but the scoped fix introduces
three new Important contradiction-classification regressions.
