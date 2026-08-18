### Spec Compliance

- `FAIL`: Risk-policy validation is missing at `src-tauri/src/acp/delegation/workflow/project.rs:2215`, and durable child independence is not reconciled at `src-tauri/src/acp/delegation/workflow/project.rs:2491`.
- `Cannot verify`: The reported 3 route and 3 warning test results were not rerun, per instruction.

### Strengths

- Normal/high routes use canonical, explicitly slotted keys and preserve distinct implementer, primary, and auxiliary identities.
- Progress and durable-run matching compares complete keys rather than generic roles.
- Warnings are deduplicated and capped at 64.
- Invalid route derivation falls back with `simple_plan_routing_invalid`; it does not create admission or Gate authority.

### Issues

#### Critical

None.

#### Important

- `src-tauri/src/acp/delegation/workflow/project.rs:2215`: Route derivation trusts `risk.level` without validating the recorded hard triggers, unique-evidence soft score, or `risk.score` against `b2d_task_risk_v1`. The parser only checks schema/policy identifiers before accepting the snapshot (`src-tauri/src/acp/delegation/workflow/simple_parse.rs:571`), so no earlier Rust validation closes this gap. Consequently, a task with a hard trigger or soft score `>= 3` can claim `normal` and receive the weaker route. The tests reinforce the defect by treating `high` with no evidence and score zero as valid at `src-tauri/src/acp/delegation/workflow/project.rs:3330`. Recompute the canonical risk classification from deduplicated evidence, reject inconsistent level/score data, and add boundary, hard-trigger, and duplicate-evidence cases.

- `src-tauri/src/acp/delegation/workflow/project.rs:2491`: Durable runs are checked only for keys outside the route; child-conversation independence is checked solely against progress-file runs at `src-tauri/src/acp/delegation/workflow/project.rs:2328`. The durable model exposes `child_conversation_id` at `src-tauri/src/db/entities/delegation_task_run.rs:57`, so two admitted durable work units with different expected keys can share one child without producing `simple_progress_route_child_not_independent` when progress is absent or stale. Reconcile admitted durable runs, ideally together with progress references, by mapping each non-null child ID to its complete expected key and warn once on cross-key reuse; add a database-backed projection test.

#### Minor

None.

### Assessment

- Task quality: `Needs fixes`
- Reasoning: The route shapes and warning mechanics are sound, but the missing canonical risk validation permits route downgrades, and durable child reuse can evade the required independence warning.
