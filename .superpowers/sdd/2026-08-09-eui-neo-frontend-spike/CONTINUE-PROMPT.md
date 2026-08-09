# CONTINUE: EUI-NEO frontend spike (brainstorm-to-delivery absolute v2)

Paste this as the user message on the **new machine** after the branch is checked out.

<codeg_terminal_context version="1">
Selected shell: bash
Dialect: posix
Generate shell command lines using POSIX syntax.
ACP command+args requests may still execute directly.
This context is authoritative for the current connection and supersedes
earlier terminal context records.
</codeg_terminal_context>

## Goal
Resume absolute **brainstorm-to-delivery workflow v2** for design
`docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md` and approved plan
`docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md`. Parent orchestrates only
(no Plan rewrite, no parent Task code). Use Codeg MCP `delegate_to_agent` /
`continue_delegation` / `get_workflow_state` / recovery tools. Skill:
`.agents/skills/brainstorm-to-delivery/SKILL.md` + `subagent-driven-development`.

## Checkout / worktree
```bash
# after git fetch of feat/eui-neo-frontend-spike
cd /path/to/MyCodeBuddy
git fetch origin feat/eui-neo-frontend-spike
git worktree add .worktrees/feat/eui-neo-frontend-spike feat/eui-neo-frontend-spike
cd .worktrees/feat/eui-neo-frontend-spike
git status --porcelain   # must be clean before producer admission
git rev-parse HEAD       # expect 89e44a2302247f447691cc4df069008d8773b474 (or newer handoff)
```
- Branch: `feat/eui-neo-frontend-spike`
- Handoff commit: `89e44a2302247f447691cc4df069008d8773b474`
  (`chore(eui): handoff Task 6 WIP and full SDD workflow artifacts`)
- SDD root (force-tracked): `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/`
- Ledger: `.../progress.md` (includes machine handoff section)
- This continue prompt: `.../CONTINUE-PROMPT.md`
- DB exports: `.../plan/db-manifest-active.json`, `db-manifest-r6.json`,
  `db-workflow-meta.json`, `db-run-bindings.json`

## Parent policies (non-negotiable)
1. **SKIP all full cargo test** for remaining Tasks + Final. Do not run
   `cargo test --lib --features test-utils`, workspace-wide cargo test, or other
   broad shared-codeg suites that OOM ~4GiB hosts. Allowed: focused probes,
   contracts-only CTest, `cargo fmt --check`, `git diff --check`.
2. Reviewers must **not** treat missing full cargo as Critical/Important.
3. Protocol **v1 cards** required on terminal producer/review messages
   (`completion_protocol.version=1`, mode=`v1`).
4. High Tasks: Codex implementer + Codex reviewer + Grok reviewer (strict AND).
5. Same-key `continue_delegation` when lineage exists; cancellation-family is
   never `unresumable`; use typed recovery authorization when required.

## Durable workflow identity (from codeg.db on source host)
| Field | Value |
| --- | --- |
| workflow_id | `19646b57-f773-4f48-9ee0-ae228cb0d00d` |
| publication_token | `db8314a8-c587-4471-84e2-13397ae45f99` |
| workflow_kind | `brainstorm_to_delivery` |
| schema_version | `2` |
| capability_version | `workflow_manifest_v2` |
| workflow_state | `approved` |
| active_manifest_revision | `6` |
| graph_revision (live) | `196` (manifest r6 document graph was 42; runtime advanced) |
| structural_revision | `5` |
| completion_protocol_version | `1` |
| completion_protocol_mode | `v1` |
| parent_conversation_id (source host) | `3` |
| design_fingerprint | `c81283a68262e4a2b75ff05e0a1f3b3fa219de3a274dcd46048f3d5f3b142fc3` |
| plan_fingerprint | `386c9d36cb672202db329db507df3745dfa45b7cb8a2dc473a1c22ae20cceb0b` |
| design digest | `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| plan digest | `sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f` |
| design rel | `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md` |
| plan rel | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| risk_policy_version | `b2d_task_risk_v1` |
| document_digest (manifest r6) | `3c47b7a8e30ed66aa50a77fb930efc185a81f85783255ba5dd187a603f0ccbfa` |

### Gate settlements (source DB)
- **design** cycle 1 → `approved` (reviewer codex `90b56d6b…`, minors)
- **plan** cycle 1 → `approved` covering author `0a62f6bf…` digest `76a829be…`
  (reviewers codex `970a4eb0…` approve, grok `f52ff6c4…` approve)

### Task policies (manifest)
Tasks **1–9,11 = high** (codex impl + codex/grok reviewers); Task **10 = normal**
(grok impl + codex reviewer).

### Cohort frozen (source DB node bindings)
Frozen/observed through Task 6 cohort: tasks 1–6 implementer+reviewers.
Task 6 reviewers not yet observed. Tasks 7–11 + Final not started.

## Task progress (disk + platform)
| Task | Status |
| --- | --- |
| 1–4 | complete (dual high approve) |
| 5 | complete — producer fix `07f57b49…` digest `1b471206…`; rev codex `6cf2fab0…` + grok `5686f59e…` **approve_with_minors** |
| 6 | **IN PROGRESS / incomplete producer** |
| 7–11 + Final | not started |

### Task 6 critical resume facts
- Admitted work_unit_key: `task|6|implementer|codex|none` (cohort_frozen)
- Latest task_id: `962b7d60-203e-4e4b-b6e7-8009b55eeed4`
- Platform status: **`canceled`** (`error_code=parent_canceled`), child_conversation_id `33`
- run_binding: artifact_digest `48e083d714d6b9e3326e209833841c2c67e8d539`, **summary_validated=0**
- On-disk commits after Task 5:
  - `9cf90829` feat(eui): add recoverable live stream projection
  - `90372cf5` fix(eui): harden live turn recovery boundaries
  - `48e083d7` fix(eui): preserve terminal live recovery evidence
  - `89e44a23` handoff: SDD force-add + extra live recovery WIP in live/model/runtime/live_recovery
- Artifacts: `task-6-brief.md`, `task-6-report.md`, `task-6-review-package.md`
  (package may predate final WIP — refresh after producer finish)
- Reviewers not started: `task|6|reviewer|codex|none`, `task|6|reviewer|grok|none`

### Immediate parent actions on resume
1. `get_workflow_capabilities` then `get_workflow_state` for
   `19646b57-f773-4f48-9ee0-ae228cb0d00d`. If workflow missing on new host DB,
   recover/import or re-bind per platform tools using exported manifests below;
   **do not invent a second workflow_id**. Prefer platform recovery over
   republish unless state is absent.
2. Reconcile disk HEAD vs Task 6 producer. Finish Task 6 implementer:
   - Prefer **same-key continue** on lineage if platform still allows continue
     after cancel (cancellation ≠ unresumable). If continue rejected, follow
     typed recovery recipe; only replace with allowed `replacement_reason`.
   - Ensure porcelain empty; producer commit(s) only Task-6-owned files +
     force-add reports; refresh `task-6-report.md` + review package BASE..HEAD.
   - Emit protocol-v1 implementation card; require `summary_validated`.
3. Dual high review Task 6 (codex + grok) on **same** `reviewed_task_id` +
   non-empty `artifact_digest` (= clean HEAD). Fix loop until both
   approve/approve_with_minors.
4. Continue serial Tasks 7→11 under risk matrix, then Final Codex review +
   delivery report. Keep **SKIP full cargo**.

## Work unit keys (A1)
- Task N implementer: `task|{n}|implementer|{agent}|none`
- Task N reviewer: `task|{n}|reviewer|{agent}|none`
- Final: `final_review|reviewer|codex|none`, fixer `final_review|fixer|grok|none`

## Active approved manifest (document_json, revision 6)
Source table: `delegation_workflow_manifest_revisions` where
`workflow_id=19646b57-…` AND `manifest_revision=6` AND `manifest_state=approved`.
Also on disk: `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/db-manifest-active.json`

```json
{"schema_version":2,"workflow_kind":"brainstorm_to_delivery","plan_target_rel_path":"docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md","risk_policy_version":"b2d_task_risk_v1","workflow_id":"19646b57-f773-4f48-9ee0-ae228cb0d00d","expected_manifest_revision":4,"publication_token":"db8314a8-c587-4471-84e2-13397ae45f99","workflow_state":"approved","design":{"rel_path":"docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md","digest":"sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef"},"plan":{"rel_path":"docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md","digest":"sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f"},"phases":[{"id":"design","kind":"design"},{"id":"plan","kind":"plan"},{"id":"tasks","kind":"tasks"},{"id":"final","kind":"final"}],"nodes":[{"id":"design-reviewer-codex","kind":"work_unit","phase_id":"design","role":"reviewer","agent_type":"codex","work_unit_key":"design|docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md|reviewer|codex|none","deps":[],"required":true,"title":"Design reviewer (Codex)"},{"id":"plan-author","kind":"work_unit","phase_id":"plan","role":"author","agent_type":"codex","work_unit_key":"plan|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md|author|codex|none","deps":[],"required":true,"title":"Plan Author (Codex)"},{"id":"plan-reviewer-codex","kind":"work_unit","phase_id":"plan","role":"reviewer","agent_type":"codex","work_unit_key":"plan|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md|reviewer|codex|none","deps":["plan-author"],"required":true,"title":"Plan reviewer (Codex)"},{"id":"plan-reviewer-grok","kind":"work_unit","phase_id":"plan","role":"reviewer","agent_type":"grok","work_unit_key":"plan|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md|reviewer|grok|none","deps":["plan-author"],"required":true,"title":"Plan reviewer (Grok)"},{"id":"task-1-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":1,"work_unit_key":"task|1|implementer|codex|none","deps":["plan-reviewer-codex","plan-reviewer-grok"],"required":true,"title":"Task 1 implementer"},{"id":"task-1-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":1,"work_unit_key":"task|1|reviewer|codex|none","deps":["task-1-impl"],"required":true,"title":"Task 1 reviewer (codex)"},{"id":"task-1-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":1,"work_unit_key":"task|1|reviewer|grok|none","deps":["task-1-impl"],"required":true,"title":"Task 1 reviewer (grok)"},{"id":"task-2-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":2,"work_unit_key":"task|2|implementer|codex|none","deps":["task-1-rev-codex","task-1-rev-grok"],"required":true,"title":"Task 2 implementer"},{"id":"task-2-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":2,"work_unit_key":"task|2|reviewer|codex|none","deps":["task-2-impl"],"required":true,"title":"Task 2 reviewer (codex)"},{"id":"task-2-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":2,"work_unit_key":"task|2|reviewer|grok|none","deps":["task-2-impl"],"required":true,"title":"Task 2 reviewer (grok)"},{"id":"task-3-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":3,"work_unit_key":"task|3|implementer|codex|none","deps":["task-2-rev-codex","task-2-rev-grok"],"required":true,"title":"Task 3 implementer"},{"id":"task-3-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":3,"work_unit_key":"task|3|reviewer|codex|none","deps":["task-3-impl"],"required":true,"title":"Task 3 reviewer (codex)"},{"id":"task-3-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":3,"work_unit_key":"task|3|reviewer|grok|none","deps":["task-3-impl"],"required":true,"title":"Task 3 reviewer (grok)"},{"id":"task-4-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":4,"work_unit_key":"task|4|implementer|codex|none","deps":["task-3-rev-codex","task-3-rev-grok"],"required":true,"title":"Task 4 implementer"},{"id":"task-4-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":4,"work_unit_key":"task|4|reviewer|codex|none","deps":["task-4-impl"],"required":true,"title":"Task 4 reviewer (codex)"},{"id":"task-4-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":4,"work_unit_key":"task|4|reviewer|grok|none","deps":["task-4-impl"],"required":true,"title":"Task 4 reviewer (grok)"},{"id":"task-5-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":5,"work_unit_key":"task|5|implementer|codex|none","deps":["task-4-rev-codex","task-4-rev-grok"],"required":true,"title":"Task 5 implementer"},{"id":"task-5-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":5,"work_unit_key":"task|5|reviewer|codex|none","deps":["task-5-impl"],"required":true,"title":"Task 5 reviewer (codex)"},{"id":"task-5-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":5,"work_unit_key":"task|5|reviewer|grok|none","deps":["task-5-impl"],"required":true,"title":"Task 5 reviewer (grok)"},{"id":"task-6-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":6,"work_unit_key":"task|6|implementer|codex|none","deps":["task-5-rev-codex","task-5-rev-grok"],"required":true,"title":"Task 6 implementer"},{"id":"task-6-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":6,"work_unit_key":"task|6|reviewer|codex|none","deps":["task-6-impl"],"required":true,"title":"Task 6 reviewer (codex)"},{"id":"task-6-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":6,"work_unit_key":"task|6|reviewer|grok|none","deps":["task-6-impl"],"required":true,"title":"Task 6 reviewer (grok)"},{"id":"task-7-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":7,"work_unit_key":"task|7|implementer|codex|none","deps":["task-6-rev-codex","task-6-rev-grok"],"required":true,"title":"Task 7 implementer"},{"id":"task-7-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":7,"work_unit_key":"task|7|reviewer|codex|none","deps":["task-7-impl"],"required":true,"title":"Task 7 reviewer (codex)"},{"id":"task-7-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":7,"work_unit_key":"task|7|reviewer|grok|none","deps":["task-7-impl"],"required":true,"title":"Task 7 reviewer (grok)"},{"id":"task-8-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":8,"work_unit_key":"task|8|implementer|codex|none","deps":["task-7-rev-codex","task-7-rev-grok"],"required":true,"title":"Task 8 implementer"},{"id":"task-8-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":8,"work_unit_key":"task|8|reviewer|codex|none","deps":["task-8-impl"],"required":true,"title":"Task 8 reviewer (codex)"},{"id":"task-8-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":8,"work_unit_key":"task|8|reviewer|grok|none","deps":["task-8-impl"],"required":true,"title":"Task 8 reviewer (grok)"},{"id":"task-9-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":9,"work_unit_key":"task|9|implementer|codex|none","deps":["task-8-rev-codex","task-8-rev-grok"],"required":true,"title":"Task 9 implementer"},{"id":"task-9-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":9,"work_unit_key":"task|9|reviewer|codex|none","deps":["task-9-impl"],"required":true,"title":"Task 9 reviewer (codex)"},{"id":"task-9-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":9,"work_unit_key":"task|9|reviewer|grok|none","deps":["task-9-impl"],"required":true,"title":"Task 9 reviewer (grok)"},{"id":"task-10-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"grok","task_index":10,"work_unit_key":"task|10|implementer|grok|none","deps":["task-9-rev-codex","task-9-rev-grok"],"required":true,"title":"Task 10 implementer"},{"id":"task-10-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":10,"work_unit_key":"task|10|reviewer|codex|none","deps":["task-10-impl"],"required":true,"title":"Task 10 reviewer (codex)"},{"id":"task-11-impl","kind":"work_unit","phase_id":"tasks","role":"implementer","agent_type":"codex","task_index":11,"work_unit_key":"task|11|implementer|codex|none","deps":["task-10-rev-codex"],"required":true,"title":"Task 11 implementer"},{"id":"task-11-rev-codex","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"codex","task_index":11,"work_unit_key":"task|11|reviewer|codex|none","deps":["task-11-impl"],"required":true,"title":"Task 11 reviewer (codex)"},{"id":"task-11-rev-grok","kind":"work_unit","phase_id":"tasks","role":"reviewer","agent_type":"grok","task_index":11,"work_unit_key":"task|11|reviewer|grok|none","deps":["task-11-impl"],"required":true,"title":"Task 11 reviewer (grok)"},{"id":"final-reviewer-codex","kind":"work_unit","phase_id":"final","role":"reviewer","agent_type":"codex","work_unit_key":"final_review|reviewer|codex|none","deps":["task-11-rev-codex","task-11-rev-grok"],"required":true,"title":"Final reviewer (Codex)"},{"id":"final-fixer-grok","kind":"work_unit","phase_id":"final","role":"fixer","agent_type":"grok","work_unit_key":"final_review|fixer|grok|none","deps":["final-reviewer-codex"],"required":false,"title":"Final fixer (Grok)"}],"edges":[],"gates":[{"id":"design","reviewer_cohort_node_ids":["design-reviewer-codex"],"required_reviewer_node_ids":["design-reviewer-codex"],"resolution_mode":"parent_adjudication","gate_kind":"design"},{"id":"plan","reviewer_cohort_node_ids":["plan-reviewer-codex","plan-reviewer-grok"],"required_reviewer_node_ids":["plan-reviewer-codex","plan-reviewer-grok"],"resolution_mode":"parent_adjudication","gate_kind":"plan"}],"task_policies":[{"task_index":1,"risk":{"level":"high","hard_triggers":[{"kind":"unsafe_ffi","evidence":["first exported C ABI and Rust/C++ layout"]},{"kind":"public_compatibility","evidence":["ABI version and symbols become the native-shell contract"]}],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["C++ UI thread and Rust Tokio core"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["CMake, staticlib, bridge, app entry"]},{"kind":"shared_interface","score":1,"evidence":["public C ABI surface"]},{"kind":"dependency_or_build","score":1,"evidence":["EUI-NEO submodule and native link"]}],"score":5,"reason":"high: hard ABI triggers apply"},"route":{"implementer_node_id":"task-1-impl","reviewer_node_ids":["task-1-rev-codex","task-1-rev-grok"]},"allow_noop_verification":false},{"task_index":2,"risk":{"level":"high","hard_triggers":[{"kind":"security_trust_boundary","evidence":["ambient main-app roots must not cross into EUI"]},{"kind":"concurrency_lifecycle","evidence":["env pinned before worker or logger starts"]}],"soft_signals":[{"kind":"multiple_ownership_modules","score":1,"evidence":["data-root, logging, AppState bootstrap"]},{"kind":"shared_interface","score":1,"evidence":["AppState::new_eui profile"]}],"score":2,"reason":"high: data-root trust and startup ordering hard triggers"},"route":{"implementer_node_id":"task-2-impl","reviewer_node_ids":["task-2-rev-codex","task-2-rev-grok"]},"allow_noop_verification":false},{"task_index":3,"risk":{"level":"high","hard_triggers":[{"kind":"unsafe_ffi","evidence":["pointer ownership, panic containment, validation"]},{"kind":"concurrency_lifecycle","evidence":["UI thread, Tokio workers, bounded queues, shutdown drain/join"]}],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["UI and Tokio"]},{"kind":"shared_interface","score":1,"evidence":["async request/completion ABI"]},{"kind":"multi_layer_without_test_seam","score":1,"evidence":["queue/frame/lifecycle integrated"]}],"score":4,"reason":"high: unsafe_ffi and concurrency_lifecycle hard triggers"},"route":{"implementer_node_id":"task-3-impl","reviewer_node_ids":["task-3-rev-codex","task-3-rev-grok"]},"allow_noop_verification":false},{"task_index":4,"risk":{"level":"high","hard_triggers":[{"kind":"security_trust_boundary","evidence":["auth/config files and launch credentials"]},{"kind":"public_compatibility","evidence":["facade is shared public Rust contract"]}],"soft_signals":[{"kind":"multiple_ownership_modules","score":1,"evidence":["ACP facade, DTO, bridge handlers"]},{"kind":"shared_interface","score":1,"evidence":["settings JSON completion payloads"]}],"score":2,"reason":"high: credential/config boundary and public facade hard triggers"},"route":{"implementer_node_id":"task-4-impl","reviewer_node_ids":["task-4-rev-codex","task-4-rev-grok"]},"allow_noop_verification":false},{"task_index":5,"risk":{"level":"high","hard_triggers":[{"kind":"concurrency_lifecycle","evidence":["agent process spawn, selection epochs, linked sends, cancellation, connection ownership"]}],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["agent subprocesses"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["session facade, bridge, DB, manager"]},{"kind":"shared_interface","score":1,"evidence":["session/send ABI ops"]},{"kind":"broad_production_surface","score":1,"evidence":["workspace/conversation/history/send path"]}],"score":5,"reason":"high: agent lifecycle hard trigger; soft threshold also reached"},"route":{"implementer_node_id":"task-5-impl","reviewer_node_ids":["task-5-rev-codex","task-5-rev-grok"]},"allow_noop_verification":false},{"task_index":6,"risk":{"level":"high","hard_triggers":[{"kind":"concurrency_lifecycle","evidence":["snapshot/subscribe race, lag, overflow, session switches"]},{"kind":"security_trust_boundary","evidence":["permission/question/plan must fail closed"]}],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["event pump across UI/Tokio"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["projector, permission policy, frame projection"]},{"kind":"shared_interface","score":1,"evidence":["needs_resync frame flags"]}],"score":4,"reason":"high: lifecycle and permission hard triggers"},"route":{"implementer_node_id":"task-6-impl","reviewer_node_ids":["task-6-rev-codex","task-6-rev-grok"]},"allow_noop_verification":false},{"task_index":7,"risk":{"level":"high","hard_triggers":[],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["C++ copies Rust frames"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["bridge/model/pages"]},{"kind":"shared_interface","score":1,"evidence":["CodegEuiFrame consumption"]},{"kind":"dependency_or_build","score":1,"evidence":["EUI compose/render stack"]}],"score":5,"reason":"high: soft threshold across ABI, EUI, native build"},"route":{"implementer_node_id":"task-7-impl","reviewer_node_ids":["task-7-rev-codex","task-7-rev-grok"]},"allow_noop_verification":false},{"task_index":8,"risk":{"level":"high","hard_triggers":[{"kind":"concurrency_lifecycle","evidence":["stale completions, cancel, disconnect/switch ownership"]}],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["Rust lifecycle + C++ controls"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["selection/cancel/P1 settings"]},{"kind":"shared_interface","score":1,"evidence":["cancel/select completions"]}],"score":4,"reason":"high: lifecycle hard trigger and soft threshold"},"route":{"implementer_node_id":"task-8-impl","reviewer_node_ids":["task-8-rev-codex","task-8-rev-grok"]},"allow_noop_verification":false},{"task_index":9,"risk":{"level":"high","hard_triggers":[],"soft_signals":[{"kind":"cross_runtime_or_process","score":2,"evidence":["EUI and WebView markers"]},{"kind":"broad_production_surface","score":1,"evidence":["perf scripts and README evidence"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["Rust/C++/React markers"]},{"kind":"shared_interface","score":1,"evidence":["shared t0/t_first_presented anchors"]},{"kind":"dependency_or_build","score":1,"evidence":["fixture and compare scripts"]}],"score":6,"reason":"high: soft threshold across both shells and tooling"},"route":{"implementer_node_id":"task-9-impl","reviewer_node_ids":["task-9-rev-codex","task-9-rev-grok"]},"allow_noop_verification":false},{"task_index":10,"risk":{"level":"normal","hard_triggers":[],"soft_signals":[{"kind":"multiple_ownership_modules","score":1,"evidence":["aggregates Task 1-9 packages"]}],"score":1,"reason":"normal: read-only aggregation with one ownership signal"},"route":{"implementer_node_id":"task-10-impl","reviewer_node_ids":["task-10-rev-codex"]},"allow_noop_verification":false},{"task_index":11,"risk":{"level":"high","hard_triggers":[],"soft_signals":[{"kind":"broad_production_surface","score":1,"evidence":["default regressions + native smoke + agents"]},{"kind":"multiple_ownership_modules","score":1,"evidence":["all new surfaces"]},{"kind":"dependency_or_build","score":1,"evidence":["full verification suite"]}],"score":3,"reason":"high: aggregate verification soft threshold"},"route":{"implementer_node_id":"task-11-impl","reviewer_node_ids":["task-11-rev-codex","task-11-rev-grok"]},"allow_noop_verification":false}]}
```

## Source DB workflow row (delegation_workflows)
```json
{"workflow_id":"19646b57-f773-4f48-9ee0-ae228cb0d00d","parent_conversation_id":3,"workflow_kind":"brainstorm_to_delivery","schema_version":2,"active_manifest_revision":6,"graph_revision":196,"workflow_state":"approved","capability_version":"workflow_manifest_v2","publication_token":"db8314a8-c587-4471-84e2-13397ae45f99","supersedes_approved_revision":null,"created_at":"2026-08-09T01:03:25.533742968+00:00","updated_at":"2026-08-09T16:35:03.994823638+00:00","structural_revision":5,"design_fingerprint":"c81283a68262e4a2b75ff05e0a1f3b3fa219de3a274dcd46048f3d5f3b142fc3","plan_fingerprint":"386c9d36cb672202db329db507df3745dfa45b7cb8a2dc473a1c22ae20cceb0b","block_cause_code":null,"block_source_manifest_revision":null,"completion_protocol_version":1,"completion_protocol_mode":"v1","legacy_source_workflow_id":null}
```

## Manifest revision index (delegation_workflow_manifest_revisions meta)
```json
[
  {
    "workflow_id": "19646b57-f773-4f48-9ee0-ae228cb0d00d",
    "manifest_revision": 1,
    "manifest_state": "skeleton",
    "document_digest": "fd45998bbfee9146bad3992336d396ab5e73c63b04008987323dc42c6da9f050",
    "created_at": "2026-08-09T01:03:25.533742968+00:00",
    "revision_kind": "publication",
    "source_manifest_revision": null,
    "recovery_authorization_id": null,
    "transition_reason_code": null,
    "consumer_correlation_id": null,
    "graph_revision": 1,
    "recovery_source_state_fingerprint": null,
    "recovery_risk_class": null
  },
  {
    "workflow_id": "19646b57-f773-4f48-9ee0-ae228cb0d00d",
    "manifest_revision": 2,
    "manifest_state": "skeleton",
    "document_digest": "7d058fc1101a449a34038e95e0078c803b935a1d5074e7cb86af30502b06955f",
    "created_at": "2026-08-09T01:13:34.618271658+00:00",
    "revision_kind": "publication",
    "source_manifest_revision": null,
    "recovery_authorization_id": null,
    "transition_reason_code": null,
    "consumer_correlation_id": null,
    "graph_revision": 5,
    "recovery_source_state_fingerprint": null,
    "recovery_risk_class": null
  },
  {
    "workflow_id": "19646b57-f773-4f48-9ee0-ae228cb0d00d",
    "manifest_revision": 3,
    "manifest_state": "estimated",
    "document_digest": "d8cad41da95d9bc122220b564b6c8cc735309b886f969638809c4106a8e8351e",
    "created_at": "2026-08-09T01:57:30.222409977+00:00",
    "revision_kind": "publication",
    "source_manifest_revision": null,
    "recovery_authorization_id": null,
    "transition_reason_code": null,
    "consumer_correlation_id": null,
    "graph_revision": 16,
    "recovery_source_state_fingerprint": null,
    "recovery_risk_class": null
  },
  {
    "workflow_id": "19646b57-f773-4f48-9ee0-ae228cb0d00d",
    "manifest_revision": 4,
    "manifest_state": "estimated",
    "document_digest": "e17e79b303e5598036bc508a0dfb811f30a5390415b107755bd986522b11b0ff",
    "created_at": "2026-08-09T02:32:16.666460392+00:00",
    "revision_kind": "publication",
    "source_manifest_revision": null,
    "recovery_authorization_id": null,
    "transition_reason_code": null,
    "consumer_correlation_id": null,
    "graph_revision": 26,
    "recovery_source_state_fingerprint": null,
    "recovery_risk_class": null
  },
  {
    "workflow_id": "19646b57-f773-4f48-9ee0-ae228cb0d00d",
    "manifest_revision": 5,
    "manifest_state": "estimated",
    "document_digest": "2bf52977c19d2d3e59989a41d24b33f82910052d65f52d4b3c556be2828a2218",
    "created_at": "2026-08-09T02:53:36.579650221+00:00",
    "revision_kind": "publication",
    "source_manifest_revision": null,
    "recovery_authorization_id": null,
    "transition_reason_code": null,
    "consumer_correlation_id": null,
    "graph_revision": 36,
    "recovery_source_state_fingerprint": null,
    "recovery_risk_class": null
  },
  {
    "workflow_id": "19646b57-f773-4f48-9ee0-ae228cb0d00d",
    "manifest_revision": 6,
    "manifest_state": "approved",
    "document_digest": "3c47b7a8e30ed66aa50a77fb930efc185a81f85783255ba5dd187a603f0ccbfa",
    "created_at": "2026-08-09T02:58:40.038532024+00:00",
    "revision_kind": "state_only",
    "source_manifest_revision": 5,
    "recovery_authorization_id": null,
    "transition_reason_code": "plan_gate_approved",
    "consumer_correlation_id": null,
    "graph_revision": 42,
    "recovery_source_state_fingerprint": null,
    "recovery_risk_class": null
  }
]
```

## Selected run_bindings (tasks 1–6)
```json
[
  {
    "task_id": "8e7439f5-1937-4af4-8b94-54b993ed3c13",
    "node_id": "task-1-impl",
    "manifest_revision": 6,
    "artifact_digest": "6fcfd6999d69d16d829b0410c1e828069aec0628",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 9,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T02:59:15.953574236+00:00",
    "updated_at": "2026-08-09T03:44:05.577974504+00:00"
  },
  {
    "task_id": "0bec4a1c-3f2a-4d20-9dab-379a187dc435",
    "node_id": "task-1-impl",
    "manifest_revision": 6,
    "artifact_digest": "6fcfd6999d69d16d829b0410c1e828069aec0628",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 10,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T03:44:43.164978604+00:00",
    "updated_at": "2026-08-09T03:45:33.711574582+00:00"
  },
  {
    "task_id": "f4d869a8-62b0-4805-8499-5fd8e0c285a8",
    "node_id": "task-2-impl",
    "manifest_revision": 6,
    "artifact_digest": "8bac8d78bcdf7f189304fa714d068e2d73ddb541",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 14,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T03:56:46.795029922+00:00",
    "updated_at": "2026-08-09T05:00:47.895853975+00:00"
  },
  {
    "task_id": "eb250a5f-e61e-441f-af46-f5130a615ed8",
    "node_id": "task-2-impl",
    "manifest_revision": 6,
    "artifact_digest": "8bac8d78bcdf7f189304fa714d068e2d73ddb541",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 15,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T05:01:24.049143877+00:00",
    "updated_at": "2026-08-09T05:01:36.795380400+00:00"
  },
  {
    "task_id": "315c9c36-091c-4146-95de-0f071d43b7cf",
    "node_id": "task-2-impl",
    "manifest_revision": 6,
    "artifact_digest": "1e92ed75da0702bc628b5f42e0af7fe5d48c7814",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 16,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T05:12:56.870631668+00:00",
    "updated_at": "2026-08-09T05:20:06.887727960+00:00"
  },
  {
    "task_id": "dc04d65a-a464-4e31-9c57-497a4792a0e6",
    "node_id": "task-2-impl",
    "manifest_revision": 6,
    "artifact_digest": "be8b41cf8545470694e2d0b490ec5b6f6cb1a227",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 17,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T05:24:58.662475390+00:00",
    "updated_at": "2026-08-09T05:35:45.318028901+00:00"
  },
  {
    "task_id": "7263af9f-2323-4096-bf60-32c39480ff90",
    "node_id": "task-3-impl",
    "manifest_revision": 6,
    "artifact_digest": "be8b41cf8545470694e2d0b490ec5b6f6cb1a227",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 20,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T05:38:50.821076512+00:00",
    "updated_at": "2026-08-09T05:39:08.330531319+00:00"
  },
  {
    "task_id": "f2147912-2873-40fd-b49d-93fb97034bdc",
    "node_id": "task-3-impl",
    "manifest_revision": 6,
    "artifact_digest": "b55f20ddb97706ebd78126e5ffd5ef4cb249ab57",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 21,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T05:42:33.014618248+00:00",
    "updated_at": "2026-08-09T06:44:24.420611992+00:00"
  },
  {
    "task_id": "e53d2f15-9667-4dc8-94d0-ff366f390e36",
    "node_id": "task-3-impl",
    "manifest_revision": 6,
    "artifact_digest": "b55f20ddb97706ebd78126e5ffd5ef4cb249ab57",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 22,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T06:45:09.778776953+00:00",
    "updated_at": "2026-08-09T06:45:22.203752120+00:00"
  },
  {
    "task_id": "7c1d5962-2516-4fd1-97cf-b243cccc55ac",
    "node_id": "task-3-impl",
    "manifest_revision": 6,
    "artifact_digest": "b55f20ddb97706ebd78126e5ffd5ef4cb249ab57",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 23,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T06:59:14.176835451+00:00",
    "updated_at": "2026-08-09T07:02:59.938300503+00:00"
  },
  {
    "task_id": "e83e1833-71b2-412f-a158-bea9a83bd423",
    "node_id": "task-3-impl",
    "manifest_revision": 6,
    "artifact_digest": "66f7cff1ee5b02773f19f938482c3a112792ecb0",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 24,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T07:03:14.737554442+00:00",
    "updated_at": "2026-08-09T07:10:54.847209782+00:00"
  },
  {
    "task_id": "b6be3ba3-92c5-400a-96c9-0734f59ac216",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "66f7cff1ee5b02773f19f938482c3a112792ecb0",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 26,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T07:19:49.070266225+00:00",
    "updated_at": "2026-08-09T07:19:59.074693662+00:00"
  },
  {
    "task_id": "d53101ff-fd12-44a9-8b7b-37f2dffc2f56",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "66f7cff1ee5b02773f19f938482c3a112792ecb0",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 27,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T09:05:12.556154845+00:00",
    "updated_at": "2026-08-09T09:16:09.027915106+00:00"
  },
  {
    "task_id": "34112004-35ad-4760-aac4-8c55f5f2b0e0",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "66f7cff1ee5b02773f19f938482c3a112792ecb0",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 28,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T09:16:21.475947857+00:00",
    "updated_at": "2026-08-09T09:58:11.244118799+00:00"
  },
  {
    "task_id": "02d4ed4b-2c6f-4d0b-8472-fd9165b3bc85",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "66f7cff1ee5b02773f19f938482c3a112792ecb0",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 29,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T09:58:54.139757568+00:00",
    "updated_at": "2026-08-09T09:59:23.928602625+00:00"
  },
  {
    "task_id": "5a1570eb-d2cb-4dfa-b75e-fb966dee328c",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 30,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T10:02:10.431671300+00:00",
    "updated_at": "2026-08-09T10:22:46.143478584+00:00"
  },
  {
    "task_id": "48d79f89-e4ef-4240-8092-f98bc9306cf2",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 31,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T10:22:59.596920627+00:00",
    "updated_at": "2026-08-09T10:23:08.249137515+00:00"
  },
  {
    "task_id": "03e0633c-037b-47a2-85c4-af48570e824e",
    "node_id": "task-4-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 32,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T10:33:28.473262743+00:00",
    "updated_at": "2026-08-09T10:42:14.651419847+00:00"
  },
  {
    "task_id": "64afea01-0fb1-4b59-8233-084c7bcd81d6",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 35,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T10:48:29.750757911+00:00",
    "updated_at": "2026-08-09T11:15:52.867355169+00:00"
  },
  {
    "task_id": "d483e232-b3d8-408e-b341-b985978fc8d8",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 36,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:01:50.637622296+00:00",
    "updated_at": "2026-08-09T13:02:20.824420463+00:00"
  },
  {
    "task_id": "b9129d8c-6d2f-47e1-b214-3d1f2516f824",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 37,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:04:50.158124855+00:00",
    "updated_at": "2026-08-09T13:05:20.143369037+00:00"
  },
  {
    "task_id": "88fe8bf2-a6e6-47de-88f6-036a6f58eb18",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 38,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:08:35.086222506+00:00",
    "updated_at": "2026-08-09T13:09:06.420778416+00:00"
  },
  {
    "task_id": "502d861b-a63f-47a1-ab3e-fe68b11d8b1d",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 39,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:13:19.263947237+00:00",
    "updated_at": "2026-08-09T13:13:49.886361344+00:00"
  },
  {
    "task_id": "330346cf-5036-44fb-8a7e-701e8d40ecbf",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "29904a3a8fe6a741372809dfccb08f7a2e194e9f",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 40,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:16:45.228398367+00:00",
    "updated_at": "2026-08-09T13:17:16.747768773+00:00"
  },
  {
    "task_id": "02ed13f4-9a63-4166-b5b5-fa688f6b3c1c",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "624fa8c37c82233a07eaa25cfc166992ee8c9c96",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 41,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:23:25.414608900+00:00",
    "updated_at": "2026-08-09T13:51:23.066610627+00:00"
  },
  {
    "task_id": "ca07b7cb-bc13-437d-afaa-3060e6f50523",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "624fa8c37c82233a07eaa25cfc166992ee8c9c96",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 42,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:52:18.748608421+00:00",
    "updated_at": "2026-08-09T13:52:25.900870473+00:00"
  },
  {
    "task_id": "7fdfe74c-7256-485d-8d06-fe5df8c23875",
    "node_id": "task-5-rev-grok",
    "manifest_revision": 6,
    "artifact_digest": "624fa8c37c82233a07eaa25cfc166992ee8c9c96",
    "reviewed_task_id": "ca07b7cb-bc13-437d-afaa-3060e6f50523",
    "summary_validated": 1,
    "lineage_ordinal": 43,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:54:03.222027673+00:00",
    "updated_at": "2026-08-09T13:58:49.143305204+00:00"
  },
  {
    "task_id": "c1d8b190-5363-4659-99da-11be3b3ac2f0",
    "node_id": "task-5-rev-codex",
    "manifest_revision": 6,
    "artifact_digest": "624fa8c37c82233a07eaa25cfc166992ee8c9c96",
    "reviewed_task_id": "ca07b7cb-bc13-437d-afaa-3060e6f50523",
    "summary_validated": 1,
    "lineage_ordinal": 44,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T13:54:03.250093821+00:00",
    "updated_at": "2026-08-09T14:02:39.702467625+00:00"
  },
  {
    "task_id": "e0a74684-b606-4fb4-980a-8a196aaa7fdb",
    "node_id": "task-5-rev-grok",
    "manifest_revision": 6,
    "artifact_digest": "624fa8c37c82233a07eaa25cfc166992ee8c9c96",
    "reviewed_task_id": "ca07b7cb-bc13-437d-afaa-3060e6f50523",
    "summary_validated": 1,
    "lineage_ordinal": 44,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T14:03:29.541435630+00:00",
    "updated_at": "2026-08-09T14:03:41.846998574+00:00"
  },
  {
    "task_id": "07f57b49-9563-463e-a93d-0818c3afe49a",
    "node_id": "task-5-impl",
    "manifest_revision": 6,
    "artifact_digest": "1b4712060387299c21c6780ccdf3a346fed63864",
    "reviewed_task_id": null,
    "summary_validated": 1,
    "lineage_ordinal": 43,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T14:03:29.560212076+00:00",
    "updated_at": "2026-08-09T14:31:00.439376573+00:00"
  },
  {
    "task_id": "6cf2fab0-1fc5-4328-8111-14ad16342938",
    "node_id": "task-5-rev-codex",
    "manifest_revision": 6,
    "artifact_digest": "1b4712060387299c21c6780ccdf3a346fed63864",
    "reviewed_task_id": "07f57b49-9563-463e-a93d-0818c3afe49a",
    "summary_validated": 1,
    "lineage_ordinal": 45,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T14:31:42.890534715+00:00",
    "updated_at": "2026-08-09T14:36:50.626611481+00:00"
  },
  {
    "task_id": "5686f59e-faf5-4d07-be03-e614cb42698e",
    "node_id": "task-5-rev-grok",
    "manifest_revision": 6,
    "artifact_digest": "1b4712060387299c21c6780ccdf3a346fed63864",
    "reviewed_task_id": "07f57b49-9563-463e-a93d-0818c3afe49a",
    "summary_validated": 1,
    "lineage_ordinal": 45,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T14:31:42.908379234+00:00",
    "updated_at": "2026-08-09T14:33:03.281273728+00:00"
  },
  {
    "task_id": "962b7d60-203e-4e4b-b6e7-8009b55eeed4",
    "node_id": "task-6-impl",
    "manifest_revision": 6,
    "artifact_digest": "48e083d714d6b9e3326e209833841c2c67e8d539",
    "reviewed_task_id": null,
    "summary_validated": 0,
    "lineage_ordinal": 46,
    "producer_baseline_head": null,
    "created_at": "2026-08-09T14:37:33.401086205+00:00",
    "updated_at": "2026-08-09T16:35:03.994823638+00:00"
  }
]
```

## Node bindings snapshot
Full list in `plan/db-workflow-meta.json` → `nodes` (38 nodes). Key frozen keys:
`task|1|…` through `task|6|…` implementer/reviewer; Task 6 reviewers unobserved.

## Do / Don't
**Do:** index-first recovery; read SDD reports; platform-selected nodes; high dual
review; v1 cards; skip full cargo; serial tasks.
**Don't:** parent implement Task code; rewrite Plan; high on single reviewer;
map cancel→unresumable; demand full cargo; wait for human UAT mid-pipeline.

## First message after tools
Call `get_workflow_state`, print actionable routes + Task 6 node status, then
resume Task 6 producer without redoing Tasks 1–5.
