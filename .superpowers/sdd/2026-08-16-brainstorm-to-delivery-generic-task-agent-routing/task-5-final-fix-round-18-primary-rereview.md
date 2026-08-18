# Task 5 Final Fix Round 18 Primary Re-review

Fix base: `c2fd394b94494719f0c92af1fdeaff70e592b1a0`

Reviewed head: `a778e592e41c2b45bc7e0489140e4b31a9fac6cd`

## 1. Finding Verdicts

1. **Important 1 - ADDRESSED.** The fix recognizes `code`, `integration`,
   and `security` as compound modifiers at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:820`
   and admits them in the direct-owner chain at the same file's line 3823.
   The focused owner controls at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4778`
   accept the three server-owned components and reject the corresponding
   Task-owned components.

2. **Important 2 - ADDRESSED.** The completion-adjunct heads are defined at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:988`
   and excluded from direct-object classification at line 4454. The controls
   at `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4795`
   accept all five requested adjunct forms while retaining rejection for
   `completed documentation`, `completed the migration`, and reactivation.

3. **Important 3 - ADDRESSED.** Agent identity/profile nouns are defined at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:908`
   and prevent the non-Agent-object exemption at lines 4703-4713. The focused
   controls at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4825`
   reject all three requested identity/profile switches while accepting the
   requested unrelated-object changes and retaining direct-switch rejection.

4. **Important 4 - ADDRESSED.** The new preceding-segment antecedent check at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3867`
   is applied to directly governed restart pronouns at lines 3920-3927. The
   controls at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4856`
   accept the separate-service case, reject the no-competing-antecedent case,
   and retain the requested intransitive, reflexive, and subordinate-pronoun
   controls.

5. **Important 5 - ADDRESSED.** The parenthetical fallback at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3978`
   recovers the outer subject across a paired punctuation boundary. The
   controls at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:4886`
   accept the requested server-governed parenthetical and explicit-server
   variants while rejecting both preposed-gerund Task restarts.

## 2. New Breakage in the Fix Diff

### Important 1 - Possessive direct objects headed by a day are treated as adjuncts

`TASK_COMPLETION_ADJUNCT_HEADS` includes `today` and `tomorrow` at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:988`,
and `completionHasDirectObject` returns `false` solely from that first token at
line 4454 without considering the recorded possessive boundary. Consequently:

```text
The active Task is partially complete and later completed tomorrow's migration. Then switch the Task Agent.
```

is rejected at the fix base but accepted at the reviewed head. `tomorrow's
migration` is a genuine direct object, so the new acceptance can authorize an
Agent switch while the Task is still only partially complete.

### Important 2 - Any later `profile` token erases an explicit unrelated object

`changeHasExplicitNonAgentObject` scans the entire action segment for an
identity/profile noun before it evaluates the actual object at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4703`.
This makes an unrelated later noun override the direct `branches` object:

```text
The active Task is running. The Task Agent switches branches after checking the browser profile.
```

The fix base accepts this compliant directive; the reviewed head rejects it.
The identity/profile relation needs to bind to the Agent-change predicate and
object, rather than to any token later in the segment.

### Important 3 - A qualified non-Task subject hides a later explicit Task object

`previousSegmentHasExplicitNonTaskAntecedent` looks only for a qualifier and a
non-Task subject head at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3867`.
It does not check whether that same segment introduces an explicit Task object,
yet its result suppresses the directly governed `it` Task-object path at line
3925. Consequently:

```text
The Task is completed but a separate service monitors the Task and the server restarts it and it is still running. Then switch the Task Agent.
```

is rejected at the fix base but accepted at the reviewed head. Here the
explicit and nearest antecedent of the restart object is `the Task`; accepting
the switch loses the completed-boundary safety invariant.

## 3. Out-of-Scope Observations

The focused pressure probe also found that both the fix base and reviewed head
accept this causative restart form:

```text
The active Task is completed but the server, monitoring the Task, lets it restart and it is still running. Then switch the Task Agent.
```

The object-pronoun scan at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3914`
only looks after the restart predicate, whereas `it` precedes `restart` in this
construction. Because the behavior is unchanged from the fix base, this is a
non-blocking untouched-code observation and is excluded from severity counts.
The previously recorded Task 2 and Task 4 Minors were also outside this scoped
range and were not re-reviewed.

## 4. Verification Performed

- Read the Task brief, Round 18 findings, complete producer report through
  Round 18, and the complete scoped diff package.
- Confirmed `HEAD` is exactly
  `a778e592e41c2b45bc7e0489140e4b31a9fac6cd` and the range contains exactly
  one commit.
- Confirmed the range changes only the two permitted validator files.
- Confirmed `git diff --numstat` reports `132` insertions and `0` deletions in
  the test file. Therefore no existing test expectation was removed, replaced,
  or weakened in the scoped diff.
- Confirmed `git diff --check` passes for the exact base-to-head range.
- Ran the focused Round 18 filter: 5 tests passed, 0 failed.
- Ran a focused in-memory base-versus-head classification probe for the three
  new Important cases. Each was correct at the fix base and regressed at the
  reviewed head.
- Ran one additional base-versus-head causative-restart probe to distinguish
  the pre-existing out-of-scope behavior from fix-diff breakage.
- Did not run the full Node suite or any Rust command, as required.

## 5. Severity Counts

- Critical: 0
- Important: 3
- Minor: 0
- Out-of-scope observations, not counted: 1

## 6. Final Verdict

**REJECT - NOT APPROVED.** All five Round 18 source finding groups are
addressed, but the scoped fix introduces three Important regressions. The
approval condition requiring no new Critical or Important breakage is not met.
