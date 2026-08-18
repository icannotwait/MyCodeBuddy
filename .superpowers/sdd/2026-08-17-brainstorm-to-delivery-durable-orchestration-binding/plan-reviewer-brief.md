# Plan Reviewer Brief — Durable Orchestration Binding Increment

You are the independent Codex Plan Reviewer. You cannot see the parent
conversation. You did not author this Plan. Review the complete latest Plan
against the approved Design. Do not implement code and do not edit the Plan.

## Read first

1. Approved Design:
   `docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md`
2. Complete latest Plan:
   `docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md`
3. Plan Author report:
   `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/plan-author-report.md`
4. Current Skill:
   `.agents/skills/brainstorm-to-delivery/SKILL.md`
5. Current repository sources named in the Plan File Map. Confirm file
   ownership and interfaces match the code that actually exists at HEAD
   `f13c0c79` plus the already-landed 2026-08-16 routing increment.

Working directory:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

## Context

This Plan is an increment. Tasks 1-5 of
`docs/superpowers/plans/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing.md`
are already implemented. The Design revision at `f13c0c79` is independently
approved. Task Agent generation 1 is grok with a null profile.

Current static validator result against this Plan and synced progress:

```text
PASS: brainstorm-to-delivery Simple contract
  SKILL.md line count: 420
  Plan Tasks parsed: 7
  Progress Tasks parsed: 7
0 failures, 3 checks completed
```

`--derive-plan-routing` does not exist yet; that is planned work.

## Review the complete latest Plan

Verdict both:

1. Spec compliance against the approved Design
2. Plan quality: implementability, TDD, no placeholders, exact files,
   interfaces, verification commands, serial dependencies, and risk/route
   correctness

Check at least:

- Every Design Testing, Compatibility, and Success Criteria item has a Task.
- `b2d_task_risk_v1` arithmetic, evidence, and derived routes are correct.
- High route is Codex implementer + Codex primary + Grok auxiliary.
- Routing JSON indices match contiguous Task headings.
- File ownership is exact and non-overlapping except where later Tasks
  consume earlier committed interfaces.
- Every Rust command uses `--no-default-features --features server,test-utils`
  and none enables `tauri-runtime`.
- Grok `tools/list` 7680-byte / `7_680` budget is retained, not weakened.
- No workflow manifest, Gate, completion Card, or platform-owned completion
  decision is reintroduced.
- ACP `route_fingerprint` is not reused for orchestration identity.
- Parent remains coordinator-only.
- Lost-acknowledgement adoption, actual Agent/profile proof, cross-namespace
  keyed discovery, and the coordinated Plan/progress rewrite regression are
  planned.
- SKILL.md stays under 500 lines in the Task that edits it.
- No TBD/TODO/placeholder steps.

Do not pre-clear issues. If a finding is plan-mandated and you still consider
it a defect, report both the finding and the Plan text.

## Report

Write:
`.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/plan-review.md`

Include Critical / Important / Minor findings, counts, and a verdict of
`APPROVED` or `CHANGES REQUIRED`. Return only the verdict, counts, and
finding one-liners.
