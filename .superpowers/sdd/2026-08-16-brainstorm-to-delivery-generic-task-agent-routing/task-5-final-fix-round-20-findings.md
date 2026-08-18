# Task 5 Final Fix Round 20 Findings

Fix the following deduplicated union from the independent Round 19 scoped
re-reviews. Treat every group as Important and binding.

## 1. Direct selected-profile possessive qualifiers fail open

The Round 19 identity-prefix allowlist does not recognize ordinary direct
possessive qualifiers such as `their` and `own`. These active-Task route
profile changes must reject:

```text
The active Task is running. The Task Agent will change their selected profile immediately.
The active Task is running. The Task Agent will change their current profile immediately.
The active Task is running. The Task Agent will change its own profile immediately.
```

Retain the Round 19 accepted controls where `profile` belongs to an unrelated
changed object, and retain the direct `its selected profile` rejection.

## 2. A later Task mention is not necessarily the restart antecedent

The Round 19 closer-Task check is relation-blind. These completed-Task
statements refer to a non-Task subject or compound artifact and must accept:

```text
The Task is completed but a separate service starts a Task worker and the server restarts it and it is still running. Then switch the Task Agent.
The Task is completed but a separate service monitors the Task log and the server restarts it and it is still running. Then switch the Task Agent.
The Task is completed but a separate service for the Task fails and the server restarts it and it is still running. Then switch the Task Agent.
```

Retain rejection for the Round 19 explicit closer-Task controls where the
service monitors or restarts the Task and `it` therefore resolves to the Task.

## 3. Parenthetical Task reactivation must be affirmative and direct

The Round 19 parenthetical check treats any preceding reactivation predicate
as direct affirmative Task reactivation. These completed-Task statements must
accept:

```text
The active Task is completed but the server, not restarting the Task, restarts and it is still running. Then switch the Task Agent.
The active Task is completed but the server, without restarting the Task, restarts and it is still running. Then switch the Task Agent.
The active Task is completed but the server, restarting a worker that monitors the Task, restarts and it is still running. Then switch the Task Agent.
```

Retain rejection for the affirmative direct control:

```text
The active Task is completed but the server, restarting the Task, restarts and it is still running. Then switch the Task Agent.
```

## Process and scope

- Follow strict TDD: add focused tests first, run them and record the expected
  RED failures before editing production code, then implement and rerun GREEN.
- Do not delete, weaken, or relabel existing expectations.
- Modify only:
  - `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  - `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Run the focused Round 20 cases, the full Node validator suite, production
  validator, Prettier check, syntax checks, `git diff --check`, and scope check.
- Do not run Rust commands. The orchestrator will run the explicitly permitted
  no-default-feature Rust verification after all review gates pass.
- Append a complete Round 20 section to `task-5-report.md` and commit the two
  permitted tracked files in one commit. Do not commit scratch reports.
