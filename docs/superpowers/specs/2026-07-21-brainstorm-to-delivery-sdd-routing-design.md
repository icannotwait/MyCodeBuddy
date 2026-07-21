# Brainstorm-to-Delivery SDD Routing Design

## Context

`brainstorm-to-delivery` currently permits either direct implementation or
`subagent-driven-development` (SDD). That choice allowed a delivery session to
implement a reviewed multi-task plan in the parent Grok session while using
Codex only for reviews. The workflow therefore skipped SDD task briefs,
per-task implementers, per-task independent reviews, and its progress ledger.

This change makes SDD mandatory and assigns explicit Codeg agent types to each
implementation role.

## Goals

- Execute every reviewed implementation plan through the complete SDD workflow.
- Use a fresh Grok child for each Task's implementation work.
- Use an independent Codex child for every Task review and the final branch
  review.
- Preserve optional parallel reviewers for Brainstorm and plan documents while
  keeping Codex mandatory in that document review group.
- Fail closed instead of silently falling back to parent-session implementation.

## Non-goals

- Do not modify the shared `subagent-driven-development` skill.
- Do not change Codeg delegation routing, profiles, or MCP schemas.
- Do not allow optional document reviewers to review implementation code.
- Do not prescribe a concrete Grok or Codex model beyond the Codeg agent type or
  an explicitly supplied delegation profile.

## Required Workflow

1. Treat the referenced Brainstorm as the requirements baseline.
2. Review the Brainstorm when the existing risk conditions require it. The
   document review group always contains Codex and may contain user-selected
   optional reviewers.
3. Use `writing-plans` to create the implementation plan and have the same
   complete document review group approve it.
4. Run the existing workspace gate immediately before execution.
5. Invoke and completely follow `subagent-driven-development`. There is no
   direct implementation mode.
6. Execute Tasks sequentially through the SDD implement-review-fix loop and run
   the SDD final whole-branch review.
7. Run final verification, create only the requested local commits, and report
   the results.

## Agent Role Contract

| Phase | Required agent | Rules |
| --- | --- | --- |
| Brainstorm review | Codex plus optional document reviewers | All reviewers are read-only; Codex is never optional. |
| Plan review | Codex plus optional document reviewers | All reviewers are read-only; resolve Critical and Important findings before execution. |
| Task implementation | Grok | Dispatch a fresh `agent_type: "grok"` child for each Task using the SDD task brief and report contract. |
| Task fixes | Grok | Every implementation or fix dispatch uses `agent_type: "grok"`; rerun covering tests and update the Task report. |
| Task review | Codex | Dispatch an independent read-only `agent_type: "codex"` child with the SDD brief, report, and review package. |
| Final branch review | Codex | Dispatch an independent read-only `agent_type: "codex"` child with the final review package. |

The parent session is a controller. It prepares handoff artifacts, answers
questions, adjudicates findings, maintains the SDD ledger, and coordinates
verification. It must not implement or fix Task code itself.

## Failure Handling

- If a plan is not decomposed into dispatchable Tasks, revise and re-review the
  plan before execution.
- If the SDD workspace prerequisites cannot be satisfied, stop and report the
  blocker. Do not switch to direct implementation.
- If Codeg delegation is unavailable or a required Grok/Codex child cannot be
  launched, stop and report the blocker. Do not substitute another agent type.
- If an implementer reports `NEEDS_CONTEXT` or `BLOCKED`, follow SDD escalation
  rules while preserving the Grok implementation role.
- Critical and Important review findings return to a Grok fixer and then to an
  independent Codex re-review until both SDD verdicts pass.

## Skill Structure Changes

- Replace the current mode-selection section with an early mandatory SDD and
  role-contract section.
- Remove every direct-implementation branch, risk matrix, example, and
  rationalization that permits parent-session implementation.
- Retain the existing document review group and workspace gate.
- Make the implementation and code-review sections defer process details to SDD
  while enforcing the Codeg agent-type bindings above.
- Add a concise quick-reference table and explicit counters for common bypasses,
  including project size, expensive builds, tightly coupled Tasks, urgency, and
  partial imitation of SDD.
- Keep `agents/openai.yaml` aligned with the Skill's purpose without embedding
  the workflow in discovery metadata.

## Validation

Use the existing failure as the RED baseline and forward-test the revised Skill
with a fresh agent under combined pressure:

- a large repository with expensive dependencies and builds;
- a reviewed eight-Task plan with cross-module dependencies;
- an existing isolated worktree;
- deadline pressure and prior sunk effort.

The revised Skill passes only if the agent:

1. invokes SDD rather than choosing direct implementation;
2. assigns every Task implementation and fix to Grok;
3. assigns every Task review and final review to independent Codex children;
4. retains optional reviewers only for Brainstorm and plan documents;
5. stops instead of substituting roles or implementing in the parent when a
   required child cannot run.

Also run the Skill validator, inspect the final diff, and scan for remaining
language that authorizes direct implementation.

## Acceptance Criteria

- The Skill contains one implementation path: complete SDD execution.
- The role contract uses explicit Codeg wire values `grok` and `codex`.
- Optional document reviewers remain supported and cannot enter code review.
- No project-size, build-cost, coupling, or urgency exception permits direct
  implementation.
- Validation demonstrates the revised instructions close the observed bypass.
