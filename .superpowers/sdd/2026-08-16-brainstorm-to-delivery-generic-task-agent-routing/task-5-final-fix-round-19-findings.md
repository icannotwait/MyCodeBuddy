# Task 5 Final Fix Round 19 Findings

Fix base: `a778e592e41c2b45bc7e0489140e4b31a9fac6cd`

Both Round 18 scoped re-reviews found all five source groups addressed. Their new-breakage union deduplicates to the following four Important groups. Every group below is binding.

## Important 1: Possessive temporal direct objects are treated as completion adjuncts

These are transitive artifact completions, so the Task remains only partially complete and the Agent switch must reject:

```text
The active Task is partially complete and later completed today's documentation. Then switch the Task Agent.
The active Task is partially complete and later completed yesterday's migration. Then switch the Task Agent.
The active Task is partially complete and later completed tomorrow's checklist. Then switch the Task Agent.
The active Task is partially complete and later completed tomorrow's migration. Then switch the Task Agent.
```

Retain acceptance for genuine adjuncts `completed yesterday`, `completed today`, and `completed tomorrow`, as well as `without issues`, `without errors`, `ahead of schedule`, and `under budget`. Retain rejection for the existing transitive completion and later-reactivation controls.

## Important 2: A later profile token overrides the direct unrelated object

These directives change an unrelated object, so they must accept even though a profile term occurs later or qualifies that unrelated object:

```text
The active Task is running. The Task Agent switches branches after checking the browser profile.
The active Task is running. The Task Agent will switch branches after comparing profiles.
The active Task is running. The Task Agent will change the compiler profile immediately.
The active Task is running. The Task Agent will change its logging profile immediately.
```

Identity/profile terms must block the unrelated-object exemption only when they are the direct selected-Agent identity/profile object. Retain rejection for `switch identities`, `switch profiles`, and `change its selected profile`. Retain acceptance for branches, directories, file, and logging-mode changes.

## Important 3: A qualified non-Task antecedent hides a closer explicit Task object

All of these contain a later or closer explicit Task object/antecedent. The final restart pronoun therefore refers to the Task and the switch must reject:

```text
The Task is completed but a separate service monitors the Task and the server restarts it and it is still running. Then switch the Task Agent.
The Task is completed but a separate service restarts the Task and the server restarts it and it is still running. Then switch the Task Agent.
```

Retain acceptance for `a separate service fails and the server restarts it`, where there is no intervening explicit Task object. Retain rejection for the no-competing-antecedent source and acceptance for intransitive, reflexive-server, and subordinate-pronoun controls.

## Important 4: Parenthetical recovery hides explicit Task reactivation

This must reject because the parenthetical explicitly restarts the Task, even though the outer subject is the server:

```text
The active Task is completed but the server, restarting the Task, restarts and it is still running. Then switch the Task Agent.
```

Retain acceptance for `the server, monitoring the Task, restarts and it is still running`, where the Task is merely the monitoring object. Retain rejection for explicit Task restarts after preposed gerund adjuncts and acceptance for explicit-server variants.

## Producer Contract

- Read the Task 5 brief, both complete Round 18 re-review reports, and this findings file before editing.
- Follow strict RED-GREEN-REFACTOR. Add focused Round 19 tests for all four groups and neighboring controls, run them, and record the expected RED failures before any production edit.
- Do not remove, weaken, or relabel an existing expectation to obtain GREEN.
- Modify only `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs` and `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`.
- Do not include the two deferred whole-branch observations in this scoped fix; they are unchanged from the Round 18 fix base and belong to the later broad review.
- Do not edit Skill prose, Design, Plan, progress, Rust, prior reports, or unrelated files.
- Append a `## Final Fix Round 19` section to the ignored Task 5 report with root causes, RED/GREEN evidence, files, commit, self-review, and concerns.
- Run focused Round 19 tests, full Node validator suite, production validator, Prettier, `node --check` for both files, diff checks, and scope checks.
- Do not run Rust. Rust verification is deferred until every Task and review approves.
- Create exactly one focused commit containing only the two permitted validator files. Do not merge, push, or create a PR.
