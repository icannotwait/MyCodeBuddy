# Task 5 Final Fix Round 18 Findings

Fix base: `c2fd394b94494719f0c92af1fdeaff70e592b1a0`

The Round 17 primary and explicitly labeled simulated auxiliary re-reviews agreed that all 11 source groups were addressed. Their deduplicated union contains five new Important regression groups. Every group below is binding.

## Important 1: Qualified unrelated possessives become Task-owned

These completed-Task statements must accept because the review/test belongs directly to the server:

```text
The active Task is completed but the server's security review is still running. Then switch the Task Agent.
The active Task is completed but the server's code review is still running. Then switch the Task Agent.
The active Task is completed but the server's integration test is still running. Then switch the Task Agent.
```

Retain rejecting controls where the same qualified component belongs to the Task, including `the Task's security review`, `the Task's code review`, and `the Task's integration test`.

## Important 2: Valid completion adjuncts become direct objects

These statements contain genuine later full-Task completion and must accept:

```text
The active Task is partially complete and later completed without issues. Then switch the Task Agent.
The active Task is partially complete and later completed without errors. Then switch the Task Agent.
The active Task is partially complete and later completed yesterday. Then switch the Task Agent.
```

Add neighboring accepted adjunct controls such as `ahead of schedule` and `under budget`. Retain rejecting controls for genuinely transitive completion such as `completed documentation` and `completed the migration`, plus later reactivation.

## Important 3: Active Agent identity/profile changes are exempted incorrectly

These active-Task directives change the selected Task Agent identity/profile and must reject:

```text
The active Task is running. The Task Agent will switch identities immediately.
The active Task is running. The Task Agent will switch profiles immediately.
The active Task is running. The Task Agent will change its selected profile immediately.
```

Retain accepted controls for changes to unrelated objects such as branches, directories, a file, and logging mode. Retain rejection for direct Agent replacement/switching.

## Important 4: Restart pronouns ignore an explicit non-Task antecedent

This statement must accept because both `it` pronouns resolve to the explicit separate service, not to the Task:

```text
The Task is completed but a separate service fails and the server restarts it and it is still running. Then switch the Task Agent.
```

Retain rejection for the source case with no competing non-Task antecedent:

```text
The Task is completed but the server restarts it and it is still running. Then switch the Task Agent.
```

Retain accepted intransitive, reflexive-server, and subordinate-pronoun controls.

## Important 5: Parenthetical Task objects lose the governing server subject

This statement must accept because `the server` governs `restarts`, while `the Task` is only the object of the parenthetical `monitoring` adjunct:

```text
The active Task is completed but the server, monitoring the Task, restarts and it is still running. Then switch the Task Agent.
```

Retain rejection for explicit Task restarts after preposed gerund adjuncts, including `with monitoring complete, the Task restarts` and `with testing complete, the Task restarts`. Retain accepted explicit-server variants.

## Producer Contract

- Read the Task 5 brief and both complete Round 17 re-review reports before editing.
- Follow strict RED-GREEN-REFACTOR. Add focused Round 18 tests for all five groups and neighboring controls, run them, and record the expected RED failures before any production edit.
- Do not remove, weaken, or relabel an existing expectation to obtain GREEN.
- Modify only `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs` and `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`.
- Do not edit Skill prose, Design, Plan, progress, Rust, prior reports, or unrelated files.
- Append a `## Final Fix Round 18` section to the ignored Task 5 report with root causes, RED/GREEN evidence, files, commit, self-review, and concerns.
- Run the focused Round 18 tests, full Node validator suite, production validator, Prettier, `node --check` for both files, diff checks, and scope checks.
- Do not run Rust. Rust verification is deferred until all Tasks and reviews approve.
- Create exactly one focused commit containing only the two permitted validator files. Do not merge, push, or create a PR.
