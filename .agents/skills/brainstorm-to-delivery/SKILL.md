---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm to Delivery

Treat the completed Brainstorm named by the user as the requirements baseline.
Do not repeat brainstorming. Continue through planning, implementation,
verification, independent final review, and local delivery unless a decision
changes requirements, scope, architecture, or user data handling.

**Core contract:** The Plan defines the work. One structured progress document
records orchestration. Generic delegation runs execute it. The parent keeps
those sources reconciled and adjudicates reports against repository evidence.

**REQUIRED SUB-SKILL:** Use `writing-plans` to create and revise the
Implementation Plan.

**REQUIRED SUB-SKILL:** Use `subagent-driven-development` for Task briefs,
implementation, independent review, fix loops, reports, and final review. This
Skill supplies the Codeg routing and Simple progress contract below.

## 1. Establish current truth

1. Read repository instructions, the Brainstorm, relevant code and tests,
   recent commits, and existing user changes.
2. Inspect the live MCP schemas before constructing calls. Refresh tool
   discovery before declaring a required agent or tool unavailable.
3. Use this workflow allowlist even when an older client displays additional
   workflow surfaces: `register_simple_workflow`, `delegate_to_agent`,
   `continue_delegation`, `get_delegation_status`, `get_session_info`, and
   `request_recovery_authorization` when a typed generic recovery response
   requires it.
4. Re-read the Plan, progress document, reports, Git state, and live generic
   run status after compaction, interruption, or resumed work. Treat earlier
   reasoning as provisional until disk and tool evidence confirm it.

The parent coordinates and adjudicates. Routed children implement and review
Task code. The parent may update the Plan and progress document but does not
implement Task code.

## 2. Produce the Plan and establish progress

1. Choose workspace-relative Plan and progress paths. Before any reviewer
   dispatch, create the progress document with the block from Section 3, an
   empty `tasks` array, `active_task_index: null`, and a Markdown thread ledger.
2. Self-check the Brainstorm for completeness and implementability. When it
   spans modules, migration, concurrency, security, persistence, public
   compatibility, or material ambiguity, record review intent in the ledger
   and request independent document review. Update the ledger after every
   observed state change; return revisions to the same reviewer thread.
3. Use `writing-plans` to write a Plan no larger than 2 MiB. Its
   `## Task N: Title` or `### Task N: Title` headings are contiguous and ordered
   from Task 1. Give every Task exact file ownership, interfaces, verification
   commands, report location, and commit boundary. Express dependencies through
   prior Task outputs so execution remains serial.
4. After the Plan exists on disk, call the root-only
   `register_simple_workflow` tool:

```json
{
  "plan_rel_path": "docs/superpowers/plans/example.md",
  "progress_rel_path": ".superpowers/sdd/<root-id>/progress.md"
}
```

Both values are normalized workspace-relative paths. When no progress path was
established, omit `progress_rel_path` and create the document at the returned
path. Conversation identity is token-bound and is not an argument. Treat
registration as locator metadata: an unavailable descriptor leaves the Plan,
progress, and generic delegation contracts in force.

5. Immediately refresh the block from the registered Plan: include every Task
   as `pending`, keep `active_task_index: null`, and preserve the document-review
   ledger. Refresh it again whenever a Plan revision changes Task headings.
6. Record Plan-review intent in the Markdown ledger, then request an independent
   Codex Plan review. Add user-named document reviewers only to Brainstorm/Plan
   review. The parent adjudicates findings from documents and repository facts,
   revises the Plan, and continues the same reviewer threads until Critical and
   Important findings are resolved.
7. Pause when a valid finding requires a user-owned change to requirements,
   scope, architecture, or user data handling.

## 3. Maintain the progress contract

Keep the progress document at or below 512 KiB, with exactly one structured
block at or below 64 KiB:

```text
<!-- codeg-simple-progress-v1
{
  "schema_version": 1,
  "plan_rel_path": "docs/superpowers/plans/example.md",
  "active_task_index": null,
  "tasks": [
    {
      "index": 1,
      "status": "pending",
      "runs": []
    },
    {
      "index": 2,
      "status": "pending",
      "runs": []
    }
  ],
  "final_review_status": "pending",
  "updated_at": "2026-08-11T00:00:00Z"
}
-->
```

Use Task statuses `pending`, `in_progress`, `completed`, or `blocked`. Mirror
observed generic run states as `reserving`, `running`, `completed`, `failed`,
`canceled`, `cancelled`, `stalled`, or `unknown`. Keep commands, findings,
recovery history, and the final-review thread ledger as normal Markdown after
the block.

Replace the whole block before each Task delegation mutation and after every
observed Task state change. Before admission, set the current Task and
`active_task_index`, then record intended role, agent, profile, action, and
stable `work_unit_key` with state `reserving`. After admission, fill the
returned task and child IDs. After status changes, record the observed state.
For document and final-review runs, make the same before/after updates in the
Markdown thread ledger. Mark a Task completed only after parent adjudication
confirms implementation, review, repository evidence, and covering
verification.

## 4. Apply the workspace gate

Immediately before Task execution, and again after a material Plan revision or
recovery, inspect branch, HEAD, `git status`, staged diff, unstaged diff, and
Plan touchpoints.

- Preserve every pre-existing or user-owned change.
- Continue with a recorded warning only when changes are few, attributable,
  non-overlapping, and can be excluded from Task commits and review.
- Request a user decision when ownership is unclear, changes overlap Task
  files, or the Task cannot be committed and reviewed independently.
- Tell every writing child that it is not alone in the worktree and must work
  with current files without reverting other changes.

## 5. Execute Tasks serially

Execute implementation Tasks serially with these routes:

| Work | First run | Later work on the same unit |
| --- | --- | --- |
| Task implementation/fix | Grok via `delegate_to_agent` | Same Grok via `continue_delegation` |
| Task independent review | Fresh Codex via `delegate_to_agent` | Same Codex via `continue_delegation` |
| Final whole-branch review | Fresh Codex via `delegate_to_agent` | Same Codex for interrupted recovery and fix re-review |

For a first run, call `delegate_to_agent` with `agent_type`, a self-contained
`task`, a fresh `correlation_id`, `profile_id` when selected, `working_dir` when
needed, and the stable `work_unit_key`. Include the Brainstorm/Plan references,
exact Task scope, constraints, current repository state, required checks,
report path, and the instruction to preserve unrelated work.

Use stable keys of at most 200 characters:

| Work unit | Key |
| --- | --- |
| Design reviewer | `design|{design_rel_path}|reviewer|{agent}|{profile_or_none}` |
| Plan reviewer | `plan|{plan_rel_path}|reviewer|{agent}|{profile_or_none}` |
| Task implementer | `task|{index}|implementer|{agent}|{profile_or_none}` |
| Task reviewer | `task|{index}|reviewer|{agent}|{profile_or_none}` |
| Final reviewer | `final_review|reviewer|codex|{profile_or_none}` |

Normalize path material, keep role/agent/profile fixed for the work unit, and
store the key plus latest task ID, child ID, recovery count, and replacement
metadata in progress.

Complete each Task's implementation, targeted checks, Task-owned commit,
independent review, fixes, and re-review before starting the next Task. The
parent reads every report, checks it against the current diff/commit and test
evidence, resolves Critical and Important findings, and records retained Minor
findings with reasons.

Join required runs with `get_delegation_status` using `task_ids`,
`return_when: "all_terminal_or_attention"`, and `wait_ms: 0`. Re-join only
required runs that remain active.

## 6. Continue and replace from generic run state

Prefer `continue_delegation` when the progress ledger and fresh status show a
recoverable established work unit. Supply its latest terminal `task_id`, a
self-contained new-turn `task`, a fresh `correlation_id`, and the same
`work_unit_key`. A continuation prompt must require the child to:

1. re-inspect Git, current files, diffs, reports, and relevant test evidence;
2. treat pre-interruption reasoning as provisional;
3. audit partial filesystem changes;
4. recreate any report that is not durably present; and
5. rerun covering checks before claiming completion.

When a generic call returns `recovery_confirmation_required`, call
`request_recovery_authorization` with `subject_kind: "delegation_task"`, the
returned task ID as `subject_id`, and a fresh `correlation_id`. Replay the exact
rejected call with its `recovery_authorization_id`, unchanged action, key,
agent, and profile. Keep that authorization ID transport-only; do not write it
to progress or reports.

When typed generic state selects `fresh_dispatch` for a first run that failed
before admission and never established child or resume identity, call
`delegate_to_agent` again with the same agent, profile, and key and without
`replaces_task_id`. When a replacement itself fails before reaching `running`
and typed state selects a pre-admission retry, repeat the same replacement key,
`replaces_task_id`, and `replacement_reason`. These pre-running retries preserve
the existing lineage and do not consume its one-replacement rail.

Use a fresh `delegate_to_agent` replacement only when generic state selects a
supported reason and budget remains. Supply the original agent/profile/key,
`replaces_task_id` for the latest terminal source, and the exact
`replacement_reason`: `unresumable`, `budget_exhausted_continue`,
`not_supported`, `admission_failed`, or `admission_unknown`. Record replacement
intent before the call and the new task/child IDs after admission.

Preserve inherited consumption across each established work-unit lineage:

| Generic recovery rail | Limit |
| --- | --- |
| Unexpected continuations | 2 |
| Logical replacement | 1 |

When either rail is exhausted or the required agent remains unavailable after
discovery, record the typed blocker and surface it to the user.

## 7. Final review and delivery

After every implementation Task is complete:

1. Re-read the Brainstorm, Plan, progress, Task reports, retained findings,
   commits, full diff, and worktree state.
2. Run scope-appropriate tests, lint, build, and project checks; rerun checks
   invalidated by fixes.
3. Set `final_review_status` to `in_progress`, record final-review intent in the
   Markdown ledger, and dispatch a fresh independent Codex final reviewer.
4. Adjudicate final findings from its report and current repository facts.
   Route fixes through the owning implementer work unit, rerun covering checks,
   and continue the same final-review Codex thread to review the changed
   delivery state.
5. Set final review to `completed` only when the reviewed repository state is
   locally deliverable. A later code or report mutation reopens verification
   and final review.
6. Commit only owned changes locally. Do not merge, push, or create a PR unless
   the user separately requests it.

Report the delivered result, files and commits, exact commands and outcomes,
review conclusions, retained Minors/risks, worktree state, and blockers.
Automated evidence plus final review establish local delivery. Put manual UAT
or product sign-off that does not change requirements in post-delivery
follow-up rather than between implementation Tasks.
