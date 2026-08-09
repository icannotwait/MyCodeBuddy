# Plan Author Brief — EUI-NEO Frontend Spike

## Role
You are the **Codex Plan Author** for brainstorm-to-delivery workflow v2.
You **own** the Plan file and all revisions. Parent will not write the Plan.

## Workspace
`/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike`
Branch: `feat/eui-neo-frontend-spike`

## Design (approved — sole product requirements)
- Path: `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`
- Digest: `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`
- Design review r2: `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/design/design-review-codex-report-r2.md`
- Minors to fold in as Plan clarifications (not new scope): shutdown return type, CODEG_HOME relationship, completion draining on shutdown, fixed jank threshold constant.

## Plan target (must create/write this exact path)
`docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md`

## Skills you must follow
1. Invoke and fully follow `writing-plans` (bite-sized tasks, TDD where applicable, file paths, verification commands).
2. Risk policy version: **`b2d_task_risk_v1`** (see below).
3. Task sequence shape:
   implement → automated verify → producer commit/review package readiness → next Task …
   → pre-Final aggregation → Final review → deliver/report.
   **Do NOT** insert human-only UAT / manual QA / “wait for user sign-off” as mid-sequence Task gates.
   Human acceptance belongs only in post-delivery residual work section.

## Task Routing Matrix (required section in Plan)

For every Task include a matrix row with:
- index, title
- files/modules
- hard triggers evidence (or none)
- soft signals evidence + soft total
- final risk level + reason
- implementer agent
- reviewer set
- policy version `b2d_task_risk_v1`

### Hard triggers → always high
`concurrency_lifecycle`, `security_trust_boundary`, `migration_destructive_persistence`,
`public_compatibility`, `unsafe_ffi`, `update_rollback`

### Soft signals (no hard trigger)
`cross_runtime_or_process`=2; each of `broad_production_surface`, `multiple_ownership_modules`,
`shared_interface`, `dependency_or_build`, `multi_layer_without_test_seam` =1.
Soft ≥3 → high; 0–2 → normal.

### Routes
- **normal**: implementer=grok, reviewer=[codex]
- **high**: implementer=codex, reviewers=[codex (≠ implementer thread identity for review), grok]

Note: High implementer is Codex; high reviewer 1 is a **separate** Codex child (≠ Author, ≠ implementer session); reviewer 2 is Grok.

## Expected plan content anchors (from Design)
- Optional binary `codeg-eui` + crate `codeg-eui-core` staticlib
- CMake-led EUI-NEO (submodule), Linux GLFW+OpenGL
- Async FFI request/completion + polled frame ownership
- Snapshot+subscribe resync live path; EventEmitter::WebOnly
- Pinned CODEG_EUI data root isolation
- Deterministic P0 permission decline
- Grok+Codex settings via narrow facade
- Perf anchors: t0, t_first_presented primary, jank p95, shell RSS
- Default builds must not require EUI

Milestones M0–M6 in design should map to Tasks (split further if needed for bite-sized SDD).

## Deliverables
1. Write complete Plan to the plan target path.
2. Commit the Plan file on this branch (local only).
3. Write author report to:
   `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-author-report.md`
4. Emit protocol-v1 card in final message:

```
<!-- codeg-card-summary-v1
{"kind":"author","status":"done","summary":"<≤240 chars>","plan_digest":"sha256:<hex of plan file>","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-author-report.md"}
-->
```

Compute plan_digest with `sha256sum` after the final Plan write.

Return only: status, plan path, plan digest, task count, matrix risk summary, report path.
