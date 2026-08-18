# Task 5 Final Fix Round 20 Primary Re-review

## Finding Verdicts

### 1. Direct selected-profile possessive qualifiers fail open: ADDRESSED

The production prefix allowlist now includes both `own` and `their`, so the
three required possessive selected-profile mutations remain attached to the
Task Agent identity object
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:914`).
The additive expectations cover all three source cases, retain rejection of
`its selected profile`, and retain acceptance of the unrelated compiler and
logging profile objects
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5045`).
A focused in-memory classification matrix matched all 6/6 expectations.

### 2. A later Task mention is not necessarily the restart antecedent: NOT ADDRESSED

The three enumerated acceptances and two retained explicit-Task rejections do
classify as required (5/5 in the focused in-memory matrix), and the additive
test group records those expectations
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5066`).
However, the new relation is not a correct direct-object test. It requires
every token after `Task` through the segment boundary to be a Task-state
modifier or an `-ly` adverb
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3899`).
Consequently, a directly governed Task followed by an ordinary prepositional
adjunct is treated as a non-object:

`The Task is completed but a separate service monitors the Task for diagnostics and the server restarts it and it is still running. Then switch the Task Agent.`

The validator accepts that statement, even though `the Task` remains the
direct object of `monitors` and the Agent switch must reject. This is a new
fail-open regression from the fix range; the prior closer-Task behavior
rejected the same construction.

The inverse relation is also incomplete: the non-object-link allowlist is
limited to nine prepositions
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:980`).
Clear attachments such as `a separate service near the Task fails` and
`a separate service reports on the Task` still reject because `near` and `on`
are mistaken for direct-object paths. The exact new tests do not exercise
either side of this adjacent prepositional boundary.

### 3. Parenthetical Task reactivation must be affirmative and direct: NOT ADDRESSED

The three enumerated acceptances and the bare affirmative control classify as
required (4/4 in the focused in-memory matrix), with additive coverage at
`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5083`.
Polarity is checked through `actionIsNegated`, but directness is restricted to
the closed `TASK_DIRECT_OBJECT_QUALIFIERS` set
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:972`)
and requires every intervening token to belong to it
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4020`).
That set omits ordinary possessive and demonstrative direct-object qualifiers,
including `their`, `our`, `your`, and `that`.

Both of these affirmative direct reactivations are therefore accepted:

- `the server, restarting their Task, restarts ... Then switch the Task Agent.`
- `the server, restarting that Task, restarts ... Then switch the Task Agent.`

Before this fix, any parenthetical `restarting` predicate governing the Task
preserved the active-Task contradiction, so this is a new fail-open regression
introduced by the narrowed direct-object condition. The additive test retains
only bare `restarting the Task` as the affirmative control
(`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5094`).

## New Breakage in the Fix Diff

1. **Important:** A direct Task object followed by a prepositional adjunct is
   misclassified as non-direct, allowing an active-route Agent change. This
   weakens the Round-19 explicit closer-Task rejection.
2. **Important:** Affirmative parenthetical reactivation of a possessive or
   demonstrative Task object is misclassified as indirect, allowing an
   active-route Agent change. This weakens the Round-19 parenthetical Task
   reactivation rejection.

No Critical breakage was found.

## Out-of-Scope Observations

The previously retained Task 2 CommonMark fence Minor and Task 4 combined
failed/canceled projection-locality Minor are unchanged by this scoped diff.
They are not included in the scoped severity count.

## Verification Performed

- Read the Round 20 finding union, Task 5 brief, complete Task 5 producer
  report through the Round 20 section, and supplied review package.
- Reviewed the supplied diff once. It changes only the two permitted validator
  files. The test-file hunk is purely additive: no prior expectation is
  deleted, weakened, or relabeled.
- Ran a read-only in-memory matrix against `validateSkillMarkdown` for all
  enumerated Round 20 cases and retained controls: group 1 was 6/6, group 2
  was 5/5, and group 3 was 4/4.
- Ran focused read-only in-memory probes only for the concrete direct-object,
  possessive-qualifier, and prepositional-attachment doubts described above.
- Confirmed HEAD is
  `1081eda2b0b24a470d0b591c47920b89c38d77b9` and the tracked worktree/index
  are clean.
- Did not rerun the producer's focused or full Node suites. Ran no Rust
  command and did not enable default `tauri-runtime`.

## Severity Counts

- Critical: 0
- Important: 2
- Minor: 0 scoped new findings

## Final Verdict

**CHANGES REQUIRED.** Group 1 is addressed. Groups 2 and 3 pass their exact
enumerated examples but are not addressed at the required relation boundary,
and the fix introduces two Important fail-open regressions. Approval is not
available until direct-object attachment remains correct with trailing
adjuncts and with ordinary possessive/demonstrative Task qualifiers, without
weakening the required compound and prepositional non-object acceptances.
