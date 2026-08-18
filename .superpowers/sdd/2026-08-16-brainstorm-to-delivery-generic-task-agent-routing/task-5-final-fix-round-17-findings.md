# Task 5 Final Fix Round 17 Findings

Round 16 base: `e7da74d9113511efd163536d2006db6fa7efeed2`

Round 16 reviewed HEAD / Round 17 fix base:
`0934287082cccaeb9042418803a1d1af26fc3e0a`

Source reports:

- `task-5-final-fix-round-16-primary-rereview.md`
- `task-5-final-fix-round-16-auxiliary-simulated-grok-rereview.md`

The source reports contain Critical 0 and Important 13 source findings. The
complete deduplicated union below has 11 Important groups. Preserve every
source reproducer and add neighboring controls before changing production
code.

## Important 1: Imperative heuristics erase explicit unfinished Task status

These must reject:

```text
After completion of the active Task (please note its review pending), switch the Task Agent.
After completion of the active Task: the Task's test running overnight, switch the Task Agent.
```

Do not regress the accepted imperative-action controls introduced in Round 16,
including `please review pending work`, `test running software`, and unrelated
work after a completed Task.

## Important 2: Possessive scope hides Task-owned components

These must reject:

```text
The active Task is completed but the Task's primary reviewer's mandatory review is still running. Then switch the Task Agent.
The active Task is completed but, despite the server's warning, the review is still running. Then switch the Task Agent.
The active Task is completed but, following the server's report, the validation is still running. Then switch the Task Agent.
```

An unrelated possessive in an adjunct must not own the later component, and a
nested possessor must not discard the outer Task ownership. Preserve the
accepted direct unrelated-owner control `the server's review is still running`.

## Important 3: Purpose verbs suppress genuine people objects

These must accept:

```text
The developers revise the Plan, Design, and code to enable both reviewers. The parent updates both of them.
The developers revise the Plan, Design, and code by allowing both reviewers to participate. The parent updates both of them.
```

Preserve rejection for true purpose-clause people such as `in order that three
reviewers can respond`, `in order for three reviewers to respond`, and
`, allowing three reviewers to respond`.

## Important 4: Reviewer postmodifiers are mistaken for explicit role heads

These must reject:

```text
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer on duty.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer with long tenure.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer assigned earlier.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer still responsible.
```

Preserve acceptance for real trailing role heads such as `reviewer contact
person` and `reviewer note taker`.

## Important 5: Role/document disambiguation crosses punctuation

This must accept:

```text
The developers revise the Design after consulting both reviewer and producer (Plan work begins later). The parent updates both of them on progress.
```

Parenthetical or later document tokens must not retroactively convert genuine
people participants into document modifiers. Preserve rejection when the role
tokens directly modify a document head, such as `reviewer and producer Plans`.

## Important 6: Subject-first matching treats changes to unrelated objects as Task Agent replacement

These must accept:

```text
The active Task is running. The Task Agent will switch branches immediately.
The active Task is running. The Task Agent can change directories.
The active Task is running. The Task Agent should replace a file.
The active Task is running. The Task Agent may switch the logging mode.
```

Preserve rejection for actual Task Agent replacement while the Task is active,
including modal, temporal, and reflexive forms.

## Important 7: A later transitive completed action becomes full Task completion

These must reject:

```text
The active Task is partially complete and later completed documentation. Then switch the Task Agent.
The active Task is partially complete and afterward completed the migration. Then switch the Task Agent.
```

Preserve acceptance when `later` or `afterward` is followed by genuine full
Task completion and preserve rejection for later partial completion or
reactivation.

## Important 8: Reporting and participial adjuncts suppress a Task anaphor

These must reject:

```text
The active Task is completed but, according to its own telemetry, it is reported that it is still running. Then switch the Task Agent.
The active Task is completed but, according to its own telemetry, after restarting, it is still running. Then switch the Task Agent.
The active Task is completed but according to ongoing monitoring it is still running. Then switch the Task Agent.
```

Preserve acceptance when an explicit separate service or server is the state
owner or merely monitors/tracks the Task.

## Important 9: An explicit Task reporting subject is discarded

This must reject:

```text
The active Task is completed but, according to telemetry, the Task says that it is still running. Then switch the Task Agent.
```

An explicit Task subject overrides generic reporting-verb shortcuts.

## Important 10: Restart shadowing ignores a transitive Task object

This must reject:

```text
The Task is completed but the server restarts it and it is still running. Then switch the Task Agent.
```

Preserve acceptance for the intransitive separate-server control `the server
restarts and it is still running`.

## Important 11: A preposed gerund adjunct hides an explicit Task restart

These must reject:

```text
The active Task is completed but, with monitoring complete, the Task restarts. Then switch the Task Agent.
The active Task is completed but, with testing complete, the Task restarts. Then switch the Task Agent.
```

An explicit later Task subject must not be treated as nested in a preceding
gerund adjunct.

## Scope And Required Process

- Read the complete Task 5 brief, Task 5 report, and both Round 16 reports.
- Modify only:
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  and
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`.
- Add focused Round 17 behavioral tests first. Run them and record expected
  RED output before editing the production validator.
- Fix the root causes with bounded structural parsing. Do not weaken, remove,
  or relabel existing expectations to make the suite pass.
- Run the focused Round 17 tests, the complete Node validator suite, the
  production validator, Prettier check, both Node syntax checks, and diff/scope
  checks.
- Append `## Final Fix Round 17` to `task-5-report.md`, including RED/GREEN
  evidence, changed files, self-review, and retained concerns. The report is
  ignored and must not be staged.
- Create one focused commit containing exactly the two permitted validator
  files. Do not merge, push, or open a PR.
- Do not run any Rust command. The controller will run only
  `--no-default-features --features server,test-utils` Rust checks after all
  Task and review work is complete.
