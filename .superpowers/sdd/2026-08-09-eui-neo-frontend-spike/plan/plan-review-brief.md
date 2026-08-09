# Plan Review Brief — EUI-NEO Frontend Spike

## Role
Independent Plan Reviewer for brainstorm-to-delivery (schema v2, protocol v1 settlement cards).

## Workspace
`/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike`

## Materials
1. Design (approved): `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`
   digest `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`
2. Plan: `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md`
   digest `sha256:4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db`
3. Author report: `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-author-report.md`

## Review focus
- Spec coverage vs design M0–M6 and acceptance checklist
- Task Routing Matrix consistency with `b2d_task_risk_v1` (hard/soft → level → route)
- No mid-sequence human UAT gates
- Bite-sized TDD tasks, exact files, verification commands
- High-risk FFI/concurrency tasks correctly routed high + dual reviewers
- Author is Codex; high implementers Codex; normal Task 10 Grok

## Verdict
`approve` | `approve_with_minors` | `request_changes` | `block`

## Deliverables
Write full review to the path given in your dispatch.
End with VERDICT line and **must emit** protocol-v1 card:

```
<!-- codeg-card-summary-v1
{"kind":"review","verdict":"<approve|approve_with_minors|request_changes|block>","critical":0,"important":0,"minor":0,"summary":"≤240 chars","report_file":"<your report path>"}
-->
```

Return only: verdict, bullets, report path.
