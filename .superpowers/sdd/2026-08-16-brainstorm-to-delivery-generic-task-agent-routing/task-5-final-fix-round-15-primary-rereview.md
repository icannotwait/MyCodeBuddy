# Task 5 Final Fix Round 15 Primary Re-review

## Finding Verdicts

### 1. Component/action ambiguity - ADDRESSED

The scoped cases now classify in both directions. The regression tests accept
the completed-Task imperatives `review open issues` and `test running
services` at
[validate-contract.test.mjs:3017](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3017)
and
[validate-contract.test.mjs:3022](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3022),
while `review is almost complete`, `barely complete`, and `remains nowhere
near complete` remain rejecting controls at lines 2937, 2942, and 2947. The
production path separates action objects in `taskComponentStateHasActionObject`
and retains explicit unfinished component states in
`taskComponentHasUnfinishedState` at
[validate-contract.lib.mjs:3717](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3717)
and line 3739.

### 2. Take-role target identity - ADDRESSED

The requested anaphors are covered as rejecting reviewer replacements at
[validate-contract.test.mjs:3067](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3067),
lines 3072, and 3077. The explicit `required primary contact person` and
`required contact person` controls are accepted at lines 3177 and 3182.
Production now resolves qualified reviewer anaphora at
[validate-contract.lib.mjs:4393](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4393)
and distinguishes a generic-person role with a later role qualifier at lines
4474-4487.

### 3. People-relation boundaries - ADDRESSED

The possessive and nested-object controls reject at
[validate-contract.test.mjs:3272](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3272)
and line 3277, `after consulting both reviewers` accepts at line 3297, and the
`so that` subordinate clause rejects at line 3357. The relation matcher checks
possessive indexes, nested `of` heads, clause boundaries, and the explicit
`after consulting` participant form at
[validate-contract.lib.mjs:1663](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1663).

### 4. Absence transitivity - ADDRESSED

The exact `lacking entirely` and `lacking completely` absences reject at
[validate-contract.test.mjs:3712](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3712)
and line 3717. The object-bearing `missing all context and evidence` and
`missing Friday's deadline` controls accept at lines 3747 and 3752. The
production classifier recognizes bounded modifier/time tails before deciding
whether `missing` or `lacking` has a direct object at
[validate-contract.lib.mjs:1505](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1505)
and line 1632.

### 5. Subject-first switching - ADDRESSED

`The active Task is running. The Task Agent switches immediately.` is an
explicit rejecting regression at
[validate-contract.test.mjs:3782](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3782).
The implementation now looks for a Task-Agent/Codex subject before the change
predicate at
[validate-contract.lib.mjs:4202](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4202).

### 6. Ordered completion - ADDRESSED

The later-full sequence is accepted at
[validate-contract.test.mjs:3787](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3787),
while partial-only forms reject at lines 3767-3777. For each later completion,
the qualifier scan starts after the preceding completion token at
[validate-contract.lib.mjs:3878](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3878),
so an earlier `partially` no longer contaminates a later `fully completed`.

### 7. Carried-state adjunct pronouns - ADDRESSED

The telemetry and `in fact` Task-anaphor cases reject at
[validate-contract.test.mjs:3807](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:3807)
and line 3822. The `integration service itself` and direct separate-service
controls accept at lines 3817 and 3851-3866. The adjunct branch requires the
bounded prefix and trailing `it`, and excludes a preceding `itself`, at
[validate-contract.lib.mjs:3616](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3616).

## New Breakage in Fix Diff

### Important 1: Task-component disambiguation regresses mass-noun actions and unrelated possessives

```text
After completion of the active Task: test running software, then switch the Task Agent.
The active Task is completed but the server's review is still running. Then switch the Task Agent.
```

Both are valid completed-Task switches: the first is an imperative whose
object is the mass noun `software`; the second describes a separate server's
review. The fix base accepts both, but HEAD rejects both.

The new action-object check recognizes only a head ending in `s` or a head
preceded by a determiner
([validate-contract.lib.mjs:3730](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3730)),
so ordinary bare mass nouns remain component states. In the other direction,
`stateHasTaskSubject` attaches any nearby component suffix without checking
that it is possessed by an unrelated subject
([validate-contract.lib.mjs:3668](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3668)).

### Important 2: Anaphoric matching consumes a reviewer modifier and ignores the explicit role head

```text
The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer contact person.
```

`contact person` is the explicit target head, so this does not replace the
mandatory reviewer and must be accepted. The fix base accepts it; HEAD rejects
it. Once the new code finds the modifier token `reviewer`, it resolves the
anaphoric prefix to the mandatory antecedent and returns immediately, without
examining the trailing `contact person`
([validate-contract.lib.mjs:4388](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4388),
line 4411). This recreates the explicit-role false positive for a normal noun
compound.

### Important 3: Participant inference treats role modifiers on Plans as people

```text
The developers revise the Plan and Design after consulting both reviewer and producer Plans. The parent edits both of them.
```

Here `reviewer` and `producer` modify `Plans`; the parent is explicitly editing
documents and the prose must reject. The fix base rejects it, but HEAD accepts
it. `directivePeopleAntecedents` creates people mentions from either role token
without checking the following document head
([validate-contract.lib.mjs:1963](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1963)).
The new `after consulting` participant path then binds those mentions at line
1725, and plural people recipients take precedence over the actual plural
document objects at
[validate-contract.lib.mjs:2042](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2042).

### Important 4: An adverb before an `in` complement turns transitive `lacking` into absence

```text
High Tasks are reviewed by Codex and the Task Agent reviewer is lacking completely in experience.
```

The reviewer is present but lacks experience, so this must be accepted. The
fix base accepts it; HEAD rejects it as if the required reviewer were absent.
The direct-object special case recognizes `lacking in ...` only when `in` is
the first tail token
([validate-contract.lib.mjs:1648](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1648)).
The new modifier-plus-complement path then classifies `completely in
experience` as intransitive absence at lines 1649-1659. `lacking entirely in
critical context` and `lacking severely in experience` reproduce the same
regression.

### Important 5: Subject-first switching now misses modals, temporal adverbs, and reflexives

```text
The active Task is running. The Task Agent will switch immediately.
```

This is the same forbidden active-Task switch as the addressed exact case and
must reject. The fix base rejects it; HEAD accepts it. The new subject-first
path permits only intervening tokens whose spelling ends in `ly`
([validate-contract.lib.mjs:4202](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:4202),
line 4207), so it loses the subject across `will`. `must switch`, `then
switches`, and `itself switches` fail the same way. Because the fallback looks
only for an Agent target after the change predicate, all four active-Task
switches are accepted at HEAD even though the base rejected them.

### Important 6: Unrelated service subjects still leak into Task activity

```text
The active Task is completed but, according to telemetry, a separate server says that it is still running. Then switch the Task Agent.
The Task is completed but the server restarts and it is still running. Then switch the Task Agent.
```

In both sentences, `it` is the separate server, so the completed Task permits
the switch. The fix base accepts both; HEAD rejects both. The adjunct shortcut
checks only that the normalized prefix begins with `according to`, ends in
`it`, and lacks `itself`; it does not reject an intervening explicit subject
such as `a separate server`
([validate-contract.lib.mjs:3616](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3616)).
Across coordination, shadowing is inferred only from a previous copular/state
link at lines 3652-3666, so the lexical predicate `the server restarts` also
fails to claim its following pronoun.

## Out-of-Scope Observations

No additional issue is counted. The retained Task 2 fence-detector Minor and
Task 4 coverage Minor are outside this Round-15 fix and were not reassessed.
Focused probes that behaved identically at the fix base and HEAD were excluded
from the blocking counts.

## Verification

- Confirmed HEAD `e7da74d9113511efd163536d2006db6fa7efeed2` and fix base
  `1e885dee4e31ea167444b5bd3f78f21dd278f947`.
- Read the supplied scoped diff package once and inspected only its two changed
  validator files for new breakage.
- Ran the focused Round-15 suite: **5 tests, 5 pass, 0 fail**, exit 0.
- Ran focused in-memory base/HEAD classifications for the concrete doubts
  raised by code reading. Eight decisive probes across the six groups above
  reproduced the stated base-correct/HEAD-wrong differentials; no repository
  file was used for test output.
- `git diff --check` for the supplied fix range produced no diagnostics.
- The producer's full 271-test, production-validator, and formatting results
  were treated as claims and were not rerun.
- No Rust command was run, and default `tauri-runtime` was never enabled.
- The tracked worktree and index were clean before this ignored report was
  written. No tracked file, index entry, commit, branch, or HEAD state was
  changed.

## Severity Counts

Findings under verification: **Critical 0 / Important 0 / Minor 0**
outstanding; all seven are addressed for their scoped cases.

New fix-diff breakage: **Critical 0 / Important 6 / Minor 0**.

Final severity counts: **Critical 0 / Important 6 / Minor 0**.

## Final Verdict

**NOT APPROVED**
