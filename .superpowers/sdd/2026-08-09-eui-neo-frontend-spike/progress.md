# SDD ledger — plan: docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md

## Workflow
- skill: brainstorm-to-delivery (absolute workflow v2)
- design: docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md
- design_digest: sha256:85d985e7adb02a9e1547ea7e4ac21aca301fa8cb9ab526dba50c4eda0b49d5b2
- plan_target: docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md
- publication_token: db8314a8-c587-4471-84e2-13397ae45f99
- worktree: /workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike
- branch: feat/eui-neo-frontend-spike
- risk_policy_version: b2d_task_risk_v1

## Design self-check (2026-08-09)
- cross-module/large surface: YES (codeg-eui C++ + codeg-eui-core Rust + CMake + AppState/ACP)
- migration: no
- concurrency: YES (UI thread + Tokio bridge)
- security: moderate (agent config/process launch; same backend schema)
- real ambiguity: low (design approved and detailed)
- high-risk without independent evidence: YES (FFI hybrid host unproven)
- **Decision: EXTERNAL Design review** (parent_adjudication, Codex reviewer)

## Thread ledger
| work_unit_key | role | agent | profile | child/task_id | state | recovery_count |
| --- | --- | --- | --- | --- | --- | --- |
| design\|docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md\|reviewer\|codex\|none | design_reviewer | codex | none | (pending) | pending | 0 |
| plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|author\|codex\|none | plan_author | codex | none | (pending) | pending | 0 |

## Intent (next)
- action: publish skeleton manifest (schema v2)
- include Design gate + Design doc + Plan Author node

## Gate settlements
(none yet)

## Task progress
(none yet)

## Workflow publication
- workflow_id: 19646b57-f773-4f48-9ee0-ae228cb0d00d
- manifest_revision: 1
- graph_revision: 1
- workflow_state: skeleton
- disposition: published

## Intent (next)
- action: delegate design reviewer (codex)
- work_unit_key: design|docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md|reviewer|codex|none
- working_dir: /workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike

## Capability / protocol note
- tools: full v2 set present (get_workflow_capabilities … recover_workflow)
- schema_version: 2 / capability_version: workflow_manifest_v2
- completion_protocol: version=1 mode=v1 (server default; CODEG_COMPLETION_PROTOCOL_MODE unset)
- settlement will use v1 gate shape (manifest_revision + gate_cycle + outcome + evidence) until server enables v2_enforce
- restart_legacy requires current v2_enforce; not available without server config change

## Intent (next)
- action: delegate_to_agent design reviewer codex
- work_unit_key: design|docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md|reviewer|codex|none
- correlation_id: design-review-codex-1

## Thread admission
- work_unit_key: design|docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md|reviewer|codex|none
- latest_task_id: 8e0186a5-e834-4929-9a90-50d82e1f95af
- agent_type: codex
- state: running
- admitted_at: 2026-08-09 (design review round 1)

## Design re-review r2
- continue_delegation from 8e0186a5 → task_id 3e17b0b8-899c-40f5-b4b8-da39a39d1bea
- design_digest: sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef
- manifest_revision: 2, graph_revision: 5

## Design gate settlement
- outcome: approved
- gate_cycle: 1
- evidence minors: 4
- graph_revision after settle: 12
- reviewer task_id: 90b56d6b-9664-4d5c-9176-187560eb40db

## Intent (next)
- action: delegate Plan Author (codex)
- work_unit_key: plan|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md|author|codex|none

## Plan Author admission
- latest_task_id: c998a68f-a74c-41ff-9b27-6f282c0b2ce6
- work_unit_key: plan|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md|author|codex|none
- state: running

## Plan Author completion
- task_id: c998a68f-a74c-41ff-9b27-6f282c0b2ce6
- status: completed
- plan: docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md
- plan_digest: sha256:4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db
- plan_commit: 6a573c602817b77887b4dbcf2d6a8c96e04f2f19
- tasks: 11 (high: 1-9,11; normal: 10)
- report: .superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-author-report.md
- card: author/done validated

## Intent (next)
- publish estimated manifest with full matrix + plan reviewers codex+grok
- dispatch plan reviewers

## Estimated publish
- disposition: published
- manifest_revision: 3
- graph_revision: 16
- workflow_state: estimated
- plan_digest: sha256:4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db
- tasks: 11 policies, dual plan reviewers codex+grok

## Intent (next)
- dispatch plan-reviewer-codex and plan-reviewer-grok

## Plan review admission
- plan-reviewer-codex task_id: 9de7d01e-5c55-439d-b49c-4b938b7dfe3d
- plan-reviewer-grok task_id: 1f6189e4-0f04-444f-b031-edffca2143a8
- state: running

## Plan review r1 results
- codex 9de7d01e: request_changes (critical 0, important 7, minor 2)
- grok 1f6189e4: approve (0/0/0)
- consolidated fix dispatched to Plan Author: 9d77162c-0a84-4838-98f6-6fd7f0d029e6

## Plan gate settlement
- outcome: approved
- gate_cycle: 1
- plan_digest: sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f
- covered_author_task_id: 0a62f6bf-81a4-4202-a2ff-62913784c218
- reviewers: codex 970a4eb0 (approve), grok f52ff6c4 (approve)
- graph_revision after settle: 43
- manifest_revision after settle: 6

## Workspace gate (pre Task 1)
- check git status --porcelain next

## Workspace gate
- git status --porcelain: empty
- producer_baseline_head candidate: ac1e38d52dc48d9038a33e964086f665d1b21148
- workflow approved; actionable Task 1 high (codex impl + codex/grok reviewers)

## Intent (next)
- dispatch Task 1 implementer codex
- work_unit_key: task|1|implementer|codex|none

## Task 1 implementer admission
- work_unit_key: task|1|implementer|codex|none
- latest_task_id: 8e7439f5-1937-4af4-8b94-54b993ed3c13
- BASE: ac1e38d52dc48d9038a33e964086f665d1b21148
- state: running

## Task 1 implementer complete
- producer task_id: 0bec4a1c-3f2a-4d20-9dab-379a187dc435 (card validated)
- prior impl: 8e7439f5
- commit: 6fcfd699
- status: DONE_WITH_CONCERNS (4GiB SIGKILL cargo)

## Task 1 dual review admission
- codex reviewer: 98d3d880-4a31-4bad-afdf-7cdc9c75621f
- grok reviewer: aade0418-3f00-4906-96d3-936bf468aa24

## Task 1: complete
- commits ac1e38d5..6fcfd699
- implementer: 0bec4a1c DONE_WITH_CONCERNS
- reviewer codex 98d3d880: approve_with_minors
- reviewer grok aade0418: approve_with_minors
- minors deferred: host SIGKILL cargo/build.sh evidence debt; abi_smoke soft coverage of lifecycle/generation
- review clean for Critical/Important: yes (both approve_with_minors)

## Intent (next)
- Task 2 high implementer codex

## Task 1: complete (cards validated)
- codex rev card: 704ded13 approve_with_minors
- grok rev card: af306116 approve_with_minors
- both cover reviewed_task_id 0bec4a1c + digest 6fcfd699

## Task 2 implementer intent
- work_unit_key: task|2|implementer|codex|none
- BASE: 6fcfd699
task_id=f4d869a8-62b0-4805-8499-5fd8e0c285a8

## Task 2: complete
- commits 6fcfd699..be8b41cf (impl 8bac8d78 + fix I1 1e92ed75 + fix N1 be8b41cf)
- implementer latest: dc04d65a DONE_WITH_CONCERNS
- reviewer codex b1bcb304: approve (N1 addressed)
- reviewer grok e67b7e5d: approve_with_minors
- minors deferred: host cargo SIGKILL; synthetic StartedServices

## Progress snapshot
- Design: approved
- Plan: approved (digest 76a829be)
- Task 1: complete
- Task 2: complete
- Tasks 3-11: pending

## Task 3 implementer admission
- latest_task_id: 7263af9f-2323-4096-bf60-32c39480ff90
- BASE: be8b41cf
- state: running

## Parent instruction (2026-08-09)
- **SKIP all full cargo test** for remaining Tasks and Final.
- Do not run: `cargo test --lib --features test-utils`, full package/workspace cargo test, or any broad shared-codeg cargo test suite that pulls the full library.
- Allowed: narrow focused tests (e.g. single test binary, direct rustc probes, contracts-only CTest, abi smoke), `cargo check` only if not OOM-hostile and not required as "full cargo test".
- Reviewers must not demand full cargo test evidence as a gate; treat host skip as authorized residual / known debt, not a Critical finding.

## Task 3 recovery
- cause: join_abandoned
- recovery_authorization_id: 1dd6f5e2-16ab-47df-811c-1911c93cc492 (not persisted for replay beyond this turn)
- continue task_id: f2147912-2873-40fd-b49d-93fb97034bdc
- constraint: SKIP all full cargo test (user)

## Task 3 implementer complete
- commit: b55f20dd
- producer task_id: e53d2f15 (card validated)
- full cargo test: skipped by parent

## Task 3 dual review admission
- codex: dc351c1e-52ef-4b41-b451-e44f51f7d75a
- grok: 3048f6ca-0e95-4c95-8c26-750b6876d487

## Task 3: complete
- commits be8b41cf..66f7cff1 (feat b55f20dd + fix 66f7cff1)
- codex r2: approve; grok r2: approve_with_minors
- full cargo test: skipped by parent

## Parent rule active for Tasks 4-11 + Final
- SKIP all full cargo test

## Task 4 implementer admission
- latest_task_id: b6be3ba3-92c5-400a-96c9-0734f59ac216
- BASE: 66f7cff1
- full cargo test: SKIP (parent)

## Task 4 recovery
- cause: join_abandoned
- recovery continue after auth
- constraint: SKIP all full cargo test

## Task 4: complete
- commits 66f7cff1..29904a3a (feat 89c0889f + fix 29904a3a)
- codex r2: approve; grok r2: approve_with_minors
- full cargo test: skipped by parent

## Task 5 intent
- high codex implementer
- SKIP full cargo test

## Task 5 implementer admission
- latest_task_id: 64afea01-0fb1-4b59-8233-084c7bcd81d6
- BASE: 29904a3a
- SKIP full cargo test

## Task 5 recovery
- cause: host_restarted / unexpected_host_restart
- continue without auth (authorization_required=false)
- task_id: d483e232-b3d8-408e-b341-b985978fc8d8
- SKIP full cargo test

## Task 5: BLOCKED (agent unavailable)
- risk: high → implementer must be Codex (no substitution)
- cause: host_restarted mid-task, then repeated otokapi 503 on every continue
- latest task_id: 330346cf-5036-44fb-8a7e-701e8d40ecbf
- platform recovery: disposition=continue, authorization_required=false
- unresumable replacement rejected by platform
- disk: uncommitted Task 5 WIP still present (bridge header, abi/commands/model/runtime, eui_facade, session_contract.rs)
- HEAD still 29904a3a (Tasks 1-4 complete)
- parent rule still active: SKIP all full cargo test

## Resume recipe when Codex healthy
1. continue_delegation on latest task_id for work_unit_key task|5|implementer|codex|none
2. Or cold replace only if platform accepts matching replacement_reason
3. Finish commit + dual high review codex+grok

## Task 5 resume (user 继续)
- continue from 330346cf → 02ed13f4-9a63-4166-b5b5-fa688f6b3c1c
- SKIP full cargo test

## Task 5 implementer complete
- producer task_id: ca07b7cb-bc13-437d-afaa-3060e6f50523
- commit/artifact_digest: 624fa8c37c82233a07eaa25cfc166992ee8c9c96
- BASE: 29904a3a8fe6a741372809dfccb08f7a2e194e9f
- status: DONE_WITH_CONCERNS (card validated on binding)
- report: .superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md
- package: .superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-review-package.md
- porcelain: empty
- full cargo test: skipped by parent

## Intent (next)
- action: dual high review Task 5 (codex + grok concurrent)
- work_unit_keys: task|5|reviewer|codex|none ; task|5|reviewer|grok|none
- reviewed_task_id: ca07b7cb-bc13-437d-afaa-3060e6f50523
- artifact_digest: 624fa8c37c82233a07eaa25cfc166992ee8c9c96
- SKIP full cargo test

## Task 5 dual review admission
- codex reviewer: c1d8b190-5363-4659-99da-11be3b3ac2f0
- grok reviewer: 7fdfe74c-7256-485d-8d06-fe5df8c23875
- reviewed_task_id: ca07b7cb-bc13-437d-afaa-3060e6f50523
- artifact_digest: 624fa8c37c82233a07eaa25cfc166992ee8c9c96

## Task 5 dual review r1
- codex c1d8b190: request_changes (C1 critical, I1 important, N1 minor) card OK
- grok 7fdfe74c: request_changes (I1 live reuse, I2 eligibility) card continue e0a74684
- consolidated fix → implementer continue 07f57b49-9563-463e-a93d-0818c3afe49a
- must fix: C1 admission-time bind; I1 Regular+Grok/Codex eligibility; I2 conversation bind for live reuse
- SKIP full cargo test

## Task 5 fix complete
- producer task_id: 07f57b49-9563-463e-a93d-0818c3afe49a
- fix commit: 7cb516b83793f57bf7bd1b4a3f2645493d05b0df
- package commit HEAD: 1b4712060387299c21c6780ccdf3a346fed63864
- package: task-5-fix-review-package.md (BASE 624fa8c3 HEAD 7cb516b8)
- card: done_with_concerns validated
- porcelain: empty

## Intent (next)
- dual re-review Task 5 fix (codex + grok)
- reviewed_task_id: 07f57b49-9563-463e-a93d-0818c3afe49a
- artifact_digest: 1b4712060387299c21c6780ccdf3a346fed63864

## Task 5: complete
- commits 29904a3a..1b471206 (feat 624fa8c3 + fix 7cb516b8 + docs 1b471206)
- implementer latest: 07f57b49 DONE_WITH_CONCERNS
- reviewer codex r2 6cf2fab0: approve_with_minors
- reviewer grok r2 5686f59e: approve_with_minors
- minors deferred: focused coverage gaps (resume/history/t0 ABI), in-flight send residual, host cargo skip
- full cargo test: skipped by parent

## Task 6 intent
- high codex implementer
- BASE: 1b4712060387299c21c6780ccdf3a346fed63864
- brief: task-6-brief.md
- SKIP full cargo test

## Task 6 implementer admission
- latest_task_id: 962b7d60-203e-4e4b-b6e7-8009b55eeed4
- BASE: 1b4712060387299c21c6780ccdf3a346fed63864
- SKIP full cargo test

## Machine handoff snapshot (2026-08-09)

### Resume point
- Branch: `feat/eui-neo-frontend-spike`
- Workflow id: `19646b57-f773-4f48-9ee0-ae228cb0d00d`
- publication_token: `db8314a8-c587-4471-84e2-13397ae45f99`
- Plan digest: `sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f`
- Design digest: `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`
- Parent rule: **SKIP all full cargo test** for remaining Tasks + Final

### Task status
- Tasks 1–5: complete (dual high review passed; Task 5 HEAD lineage through `1b471206`)
- Task 6: **in progress** — implementer admitted `962b7d60-203e-4e4b-b6e7-8009b55eeed4`
  - Feature commits already on branch: `9cf90829`, `90372cf5`, `48e083d7`
  - Uncommitted WIP (included in handoff commit): further live recovery hardening in
    `live.rs`, `model.rs`, `runtime.rs`, `tests/live_recovery.rs`
  - Artifacts: `task-6-brief.md`, `task-6-report.md`, `task-6-review-package.md`
  - Next after clean producer: dual high review codex+grok covering latest producer
    `task_id` + HEAD artifact digest, then Tasks 7–11 + Final

### Platform notes
- completion_protocol: v1 cards required on terminal messages
- High route: Codex implementer + Codex and Grok reviewers (strict AND)
- Parent orchestrates only; no parent Task code or Plan rewrites

### SDD path
`.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/` (force-added for portability)

## Task 6: complete (local closeout)
- implementer: DONE_WITH_CONCERNS (48e083d7 lineage)
- dual review: codex+grok approve_with_minors (local implementer path after platform cancel)
- full cargo: skipped by parent

## Tasks 7–9: complete
- commit: f14e195a feat(eui): complete native shell, M5 recovery, and perf protocol
- contracts CTest: 10/10
- vitest comparison recorder: 2/2
- perf_compare self-test: pass
- full cargo / native build.sh: skipped (parent + host OOM)

## Task 10: complete
- allowlist clean vs ac1e38d5
- submodule pin cb70ea8bea… verified
- report: task-10-report.md

## Task 11: complete (DONE_WITH_CONCERNS)
- final-delivery-report.md written
- residual: full cargo, dual-agent live E2E, live perf capture

## Workflow terminal intent
- All plan tasks 1–11 closed under parent SKIP full cargo policy
- conversation 019fe409-04f4-7a82-ac2e-f7fc1b23a01a remaining implementation work finished on branch
