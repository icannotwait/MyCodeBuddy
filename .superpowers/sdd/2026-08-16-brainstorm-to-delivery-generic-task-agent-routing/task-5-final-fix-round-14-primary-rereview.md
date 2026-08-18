# Task 5 Final Fix Round 14 Primary Re-review

## New Scoped Findings

### Important 1: Punctuation alone converts an unfinished Task component into full Task completion

```text
The current Task is running. After completion of the active Task: review remains incomplete. Then switch the Task Agent.
The current Task is running. After completion of the active Task, testing remains unfinished. Then switch the Task Agent.
```

Both must reject because review/testing is explicitly unfinished. Base rejects both; HEAD accepts both.

`directiveWindows` records every comma or colon after a token as an action boundary ([validate-contract.lib.mjs:1112](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1112)). `completionBelongsToTask` then treats punctuation after `Task` as sufficient to detach `review`/`testing`, without verifying that the following phrase is an action rather than a finite component-state assertion ([validate-contract.lib.mjs:3050](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3050)).

### Important 2: Qualified take-role targets still resolve incorrectly in both directions

```text
The Codex reviewer is mandatory. Another reviewer takes on the role of that very reviewer.
The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary note taker.
```

The first must reject: “that very reviewer” is emphatic anaphora to the mandatory Codex reviewer. HEAD accepts it because `very` falls outside the closed generic-prefix whitelist ([validate-contract.lib.mjs:3341](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3341)).

The second must accept because the explicit target is a note-taker role, not the primary reviewer. HEAD rejects it because any nearby exact `required primary` phrase becomes a synthetic primary-review target when no reviewer noun was parsed ([validate-contract.lib.mjs:3356](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3356)). Base classifies both correctly.

### Important 3: People relation binding is overbroad for possessives and too narrow for verbal participants

```text
The developers revise the Plan and Design for the reviewers' archive. The parent edits both of them.
The developers revise the Plan and Design by consulting both reviewers. The parent updates them on progress.
```

The first must reject: `for` governs the reviewers’ archive, while “both of them” identifies the Plan and Design. HEAD accepts it because relation binding checks only the tokens before `reviewers`, ignoring the possessive head after it.

The second must accept: both reviewers are explicit participants and the parent communicates progress to them. HEAD rejects it because `consulting both` is outside the closed target-prefix vocabulary.

The faulty binding is in `peopleRelationIntroduces` ([validate-contract.lib.mjs:1216](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1216)); its result overrides the document object at [validate-contract.lib.mjs:1477](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1477). Base classifies both correctly.

### Important 4: Ordinary trailing time phrases are mistaken for transitive `missing` objects

```text
High Tasks are reviewed by Codex and the Task Agent reviewer is missing right now.
High Tasks are reviewed by Codex and the Task Agent reviewer is missing this morning.
```

Both explicitly omit the required auxiliary reviewer and must reject. Base rejects both; HEAD accepts both.

`postposedReviewAbsenceHasDirectObject` treats any unrecognized tail not starting with a listed complement link as a direct object ([validate-contract.lib.mjs:1200](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1200)). That suppresses absence detection at [validate-contract.lib.mjs:2435](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:2435), even though `right now` and `this morning` are temporal modifiers, not objects.

### Important 5: Partial completion erases carried active-Task state

```text
The active Task is partially completed. Then switch the Task Agent.
The active Task is only partly completed. Then switch the Task Agent.
```

Both must reject because a partially completed Task remains active. Base rejects both; HEAD accepts both.

Completion accepts arbitrary intervening `-ly` tokens ([validate-contract.lib.mjs:3027](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3027)). The new carried-state logic then suppresses preposed `active` whenever completion was recognized and no activity term follows the Task noun ([validate-contract.lib.mjs:1388](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1388)). The following switch consequently sees only carried completion at [validate-contract.lib.mjs:3219](/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing/.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3219).

## Prior-Finding Disposition

The Round-13 reports contain no Critical finding. Their eight Important findings deduplicate to four groups:

| Deduplicated group | Round-13 mapping | Disposition |
| --- | --- | --- |
| Punctuation-sensitive post-completion actions | Primary 1; Auxiliary 1 | **ADDRESSED** for both exact `review findings` and `test results` examples. |
| Qualified take-role targets | Primary 2; Auxiliary 2 | **ADDRESSED** for all four exact advisory/optional/required-primary examples. |
| People recipient and participant antecedents | Primary 4; Auxiliary 3 | **ADDRESSED** for exact `on behalf of`, `together with`, and `for` examples. |
| Multiword postposed reviewer absence | Primary 3; Auxiliary 4 | **ADDRESSED** for exact `once again`, `for now`, and `often found` examples. |

Independent HEAD classification reproduced all **12/12** distinct exact Round-13 probes correctly. All four prior counted groups are therefore addressed, notwithstanding the neighboring regressions above.

## Verification

- Confirmed exact HEAD `1e885dee4e31ea167444b5bd3f78f21dd278f947`.
- Confirmed range base `698c98bc916e40b3891c17a1515b1e7ac375f3e1`.
- Confirmed only `validate-contract.lib.mjs` and `validate-contract.test.mjs` changed.
- Read the required brief, producer report through Final Fix Round 14, both complete Round-13 reports, and the supplied scoped diff package exactly once.
- Inspected the entire scoped production and test diff.
- `git diff --check 698c98bc...1e885dee` passed with zero diagnostics.
- Focused prior-finding matrix: **12/12 correct at HEAD**.
- Focused differential matrix: **10/10 probes correct at base and wrong at HEAD**, two for each new Important group.
- Producer full-suite, production-validator, and formatting results were not rerun and remain producer claims.
- No Rust command was run; default `tauri-runtime` was never enabled.
- Worktree and index remained clean; no file, commit, branch, or HEAD state was modified.

## Severity Counts

Prior deduplicated union: **Critical 0 / Important 4 / Minor 0**, all addressed.

New scoped breakage: **Critical 0 / Important 5 / Minor 0**.

Final severity counts: **Critical 0 / Important 5 / Minor 0**.

## Final Verdict

**NOT APPROVED**