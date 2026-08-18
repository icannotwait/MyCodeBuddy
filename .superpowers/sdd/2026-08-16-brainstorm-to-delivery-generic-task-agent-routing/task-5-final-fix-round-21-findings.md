# Task 5 Final Fix Round 21 Findings

Fix the following deduplicated union from the independent Round 20 scoped
re-reviews. Treat every group as Important and binding.

## 1. Direct Task-object attachment must survive trailing adjuncts

The Round 20 terminal-object check treats a directly governed Task followed
by an ordinary adjunct as non-direct. This active-Task switch must reject:

```text
The Task is completed but a separate service monitors the Task for diagnostics and the server restarts it and it is still running. Then switch the Task Agent.
```

The inverse relation must remain non-direct and accept:

```text
The Task is completed but a separate service near the Task fails and the server restarts it and it is still running. Then switch the Task Agent.
The Task is completed but a separate service reports on the Task and the server restarts it and it is still running. Then switch the Task Agent.
```

Retain the complete Round 20 matrix: direct `monitors the Task` and `restarts
the Task` reject; `Task worker`, `Task log`, and `service for the Task` accept.

## 2. Direct parenthetical Task objects allow ordinary qualifiers

The narrowed direct-object qualifier set omits ordinary possessive and
demonstrative forms. These affirmative direct Task reactivations must reject:

```text
The active Task is completed but the server, restarting their Task, restarts and it is still running. Then switch the Task Agent.
The active Task is completed but the server, restarting our Task, restarts and it is still running. Then switch the Task Agent.
The active Task is completed but the server, restarting your Task, restarts and it is still running. Then switch the Task Agent.
The active Task is completed but the server, restarting that Task, restarts and it is still running. Then switch the Task Agent.
```

Retain the bare affirmative `restarting the Task` rejection and all Round 20
indirect/negated acceptance controls.

## 3. Parenthetical negation must bind the reactivation predicate locally

The generic lookback lets negation of a prior action suppress a later
affirmative Task restart. This statement must reject:

```text
The active Task is completed but the server, not idling before restarting the Task, restarts and it is still running. Then switch the Task Agent.
```

Retain acceptance for `not restarting the Task` and `without restarting the
Task`, where negation directly scopes over the reactivation predicate.

## Process and scope

- Follow strict TDD: add focused tests first, run them and record the expected
  RED failures before editing production code, then implement and rerun GREEN.
- Do not delete, weaken, or relabel existing expectations.
- Modify only:
  - `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
  - `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Run the focused Round 21 cases, full Node validator suite, production
  validator, Prettier check, syntax checks, `git diff --check`, and scope check.
- Do not run Rust commands. The orchestrator will run the explicitly permitted
  no-default-feature Rust verification only after all review gates pass.
- Append a complete Round 21 section to `task-5-report.md` and commit the two
  permitted tracked files in one commit. Do not commit scratch reports.
