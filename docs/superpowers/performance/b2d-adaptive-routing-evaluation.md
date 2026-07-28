# B2D Adaptive Routing — End-to-End Verification Evaluation

**Plan:** `docs/superpowers/plans/2026-07-27-brainstorm-to-delivery-adaptive-routing.md`  
**Design:** `docs/superpowers/specs/2026-07-27-brainstorm-to-delivery-adaptive-routing-design.md`  
**Branch:** `feat/b2d-adaptive-routing`  
**Original Task 10 base:** `97b5d305d0730d7498d386a21c2d9f847ac222f7`  
**Prerequisite lint repair:** `285f01b6695c0d230a36a0ba644315ae06f76780` — `style(scripts): satisfy frontend lint`  
**Evaluation commit parent:** `285f01b6…`  
**Verifier:** Grok (Task 10)  
**Date:** 2026-07-28  

## Summary

All mandatory product verification gates for adaptive routing are **GREEN** after
the prerequisite Prettier layout repair in `src-tauri/scripts/stage-codex-acp.mjs`.
Ten deterministic design scenarios are covered by real backend fixtures executed
in the desktop Rust matrix. Comparative live multi-run measurements remain
**external blockers** (exact reasons recorded; no fabricated values). No cost
product surface was added.

This report is evidence-only. It does not change runtime, Skill, or frontend
contracts.

---

## Prerequisite (separate from Task 10 product work)

| Item | Value |
| --- | --- |
| Commit | `285f01b6695c0d230a36a0ba644315ae06f76780` |
| Subject | `style(scripts): satisfy frontend lint` |
| Scope | Prettier layout only in `src-tauri/scripts/stage-codex-acp.mjs` |
| Independent Codex review | No Critical / Important / Minor; Spec compliant; Quality Approved |
| Relation to adaptive routing | Pre-existing packaging lint residual, not Tasks 1–9 File Map |

First-round Rust/static/scenario evidence remains valid because the prerequisite
is JavaScript formatting only.

---

## Step 1 — Formatting and static contract scans

From repository root (first-round Task 10, still binding):

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | 0 | PASS |
| `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs` | 0 | PASS — 0 failures, **28** checks |
| `python …/quick_validate.py .agents\skills\brainstorm-to-delivery` | 0 | PASS — `Skill is valid!` |
| `rg -n "workflow_v1\|workflow_manifest_v1\|pair_frozen" …` | 0 (raw matches) | semantic-zero **PASS** (classified below) |
| `git diff --check` | 0 | PASS (also re-run at commit time) |

### Semantic-zero classification (every `rg` match)

Policy: raw matches allowed only in rejection tests, validator forbidden-pattern
lists, and historical migration seeds. Production parsers/capability and
`SKILL.md` must not enable or recommend v1 fallback.

| Match | Classification |
| --- | --- |
| `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs` (`workflow_manifest_v1`, `pair_frozen` in FORBIDDEN) | **approved** — validator forbidden-pattern list |
| `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs` (rejects those literals) | **approved** — rejection tests |
| `src-tauri/src/acp/delegation/companion.rs` — `workflow_v1` parse must not enable tools | **approved** — rejection / capability denial test |
| `src-tauri/src/acp/connection.rs` — assert features have no `workflow_v1` | **approved** — v2-only assertion |
| `src-tauri/src/acp/delegation/run_store.rs` — seed `capability_version: "workflow_manifest_v1"` | **approved** — historical / migration test seed |
| `src-tauri/src/acp/delegation/workflow/store.rs` — seed v1 header for rejection path | **approved** — historical / migration rejection seed |

No production path or Skill prose enables v1 fallback. `pair_frozen` appears only
as a forbidden/rejection token; product uses `cohort_frozen`.

---

## Step 2 — Rust desktop matrix (`src-tauri/`)

First-round (binding; formatting-only prerequisite):

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo check` | 0 | PASS |
| `cargo test --features test-utils` | 0 | **3771** passed, 0 failed, **1** ignored |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | 0 | PASS |

Allowed non-failures: third-party `proc-macro-error2` future-incompat note;
documented `codeg-mcp` sidecar 0-byte placeholder warning.

---

## Step 3 — Server / MCP matrices (`src-tauri/`)

First-round (binding):

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo check --no-default-features --bin codeg-server` | 0 | PASS |
| `cargo test --no-default-features --bin codeg-server --lib` | 0 | **3593** passed, 0 failed, 1 ignored |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | 0 | PASS |
| `cargo check --no-default-features --bin codeg-mcp` | 0 | PASS |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | 0 | PASS |

### Grok tools/list budget (resume re-run, exact brief command)

```powershell
cargo test --features test-utils grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --nocapture
```

| Field | Value |
| --- | --- |
| Exit | **0** |
| Matching test executed | **1** (`acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget`) |
| Result | **ok** |
| Measured JSONL bytes | **7669** |
| Fixed ceiling | **7680** |
| Contract | Root + coordination_v1 + workflow_v2 tool catalog stays host-safe; companion ask excluded from Grok list as asserted by test |

---

## Step 4 — Frontend matrix (resume re-run, full)

From repository root:

| Command | Exit | Result |
| --- | ---: | --- |
| `pnpm eslint .` | **0** | **0 errors**, **23 warnings** (pre-existing hooks/unused-vars; not Task 10) |
| `pnpm test` | **0** | **316** test files, **4275** tests passed |
| `pnpm build` | **0** | Static export **33/33** routes; prerendered static content |

---

## Step 5 — Ten deterministic scenarios (product fixtures)

All exercised with **real backend store/admission/gate/plan_review/validate
fixtures** (not mocked gate booleans). Tests executed and passed inside
`cargo test --features test-utils`.

| # | Scenario | Primary fixtures | Concrete IDs / digests / routes / cycles / failures |
| ---: | --- | --- | --- |
| 1 | **normal** | `validate::normal_and_high_routes_match_agent_matrix`; `gates::task6_normal_route_requires_its_one_reviewer`; `project::task6_projects_normal_and_high_routes_with_redacted_policy_metadata` | Normal route: Grok implementer + one Codex reviewer; producer `impl-1` gen 1 digest `digest-1`; gate **Passed** with single reviewer `task-1-codex-reviewer` |
| 2 | **hard-high** | `validate::every_hard_trigger_forces_high_risk`; `admission::task5_high_risk_reviewers_cannot_share_child_and_route_freezes_three_nodes`; recovery high seed in `store::task4_plan_initial_round_persists_derived_state_and_full_recovery` | Hard kinds force high; high freezes **3** cohort nodes (`cohort_frozen`); recovery seed hard triggers `concurrency_lifecycle` + `migration_destructive_persistence`, soft `shared_interface`, reason string, dual reviewers |
| 3 | **score-3-high** | `validate::soft_score_threshold_table_selects_risk` | Soft score **3** (`cross_runtime_or_process` + `shared_interface`) → **high** with Codex implementer + Codex/Grok reviewers; scores 0–2 remain normal without hard triggers |
| 4 | **scoped owner review** | `plan_review::owner_subset_*`; `scoped_round_accepts_a_new_finding_*`; `store::task4_scoped_round_uses_active_owner_subset_and_material_requires_cohort` | Author `author-task-scoped`; digest `sha256:plan`; owner `plan-reviewer-1`; gate_cycle **1→2**; scoped evidence task `review-scoped-c2` |
| 5 | **material full reset** | `plan_review::material_and_full_localized_revisions_restore_full_cohort`; material path in scoped store fixture | Material/full localized revisions restore complete Plan reviewer cohort |
| 6 | **one rewrite then user block** | `plan_review` stagnation suite; `store::task4_plan_stagnation_rewrite_then_user_decision_blocks` | Author `author-task-stagnation`; digest `sha256:plan`; after cycle **3**: `HolisticRewriteRequired`, stagnation_count=2, `rewrite_used=false`; after cycle **5**: `UserDecisionRequired`, `rewrite_used=true`, outcome **Blocked**, header `WorkflowState::Blocked` |
| 7 | **split high verdicts → both re-review** | `gates::task6_one_approval_plus_one_request_changes_fails_strict_and`; `project::task6_high_route_counts_strict_and_and_invalidates_both_old_approvals` | Strict **AND** over same non-empty digest `digest-1`; one approve + one request_changes fails; new producer invalidates both prior approvals |
| 8 | **recovery** | `store::task4_plan_initial_round_persists_derived_state_and_full_recovery` | Author `author-task-recovery`; reviewers `review-task-recovery-1/2`; digest `sha256:plan`; reports `reports/author-recovery.md`, `reports/reviewer-*.md`; high policy + dual route persisted |
| 9 | **v1 rejection** | `validate::v1_manifest_is_rejected`; `store::task4_publish_rejects_v1_manifest`; companion/connection denials | v1 / partial capability rejected; no fallback |
| 10 | **pre-admission revision vs post-admission freeze** | `admission::task5_policy_revision_is_allowed_before_admission_but_frozen_afterward`; `store::workflow_v2_typed_error_real_producers_cohort_frozen` | Pre-admission material risk/route revision allowed; post-admission mutation → `WorkflowStoreError::CohortFrozen` |

Supporting:

- `skill_forward_routing_invariants_nine_scenarios` — Skill contract matrix **ok**
- `listener::workflow_manifest_v2_framed_publish_and_plan_settle_reach_store` — schema_version **2**, `risk_policy_version: b2d_task_risk_v1`

**Scenario matrix: GREEN.**

---

## Step 6 — Comparative measurements (external blockers allowed)

No new cost product code. Populate from existing usage/timestamps only.

### Comparison table

| Row | Run count | Total tokens | Elapsed time | Plan reviewer calls | Task reviewer calls | Gate cycles |
| --- | --- | --- | --- | --- | --- | --- |
| Session 2070 baseline | 1 (historical session) | **100438** (aggregate only; `codeg-mcp__get_session_info`) | **BLOCKER** — session metadata has no workflow start/finish timestamps | **BLOCKER** — API returns aggregate tokens only; no Plan reviewer call counts | **BLOCKER** — same | **BLOCKER** — same |
| Normal median (3 runs) | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** |
| Hard-trigger-high median (3 runs) | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** |
| Score-trigger-high median (3 runs) | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** | **BLOCKER** |

**Exact external blocker (three-run rows):** Task 10 verification session cannot launch
three complete local B2D adaptive workflows with configured Codex/Grok agents and
collect child usage / `started_at` / `finished_at` without product changes or
assuming live multi-agent availability. No auditable multi-run measurement
artifacts exist under `.superpowers/sdd` for this plan.

**Session 2070 partial evidence:** title
`B2D会话writing-plans多轮并行审核耗时与Token优化分析`; agent `grok`;
status `pending_review`; branch `main`; workspace `D:\MyCodeBuddy`;
**total tokens 100438**. Missing columns as above.

### Structural metrics from fixtures (not live medians)

| Metric | Evidence |
| --- | --- |
| Scoped fan-out vs complete cohort | Scoped owner subset = open Critical/Important owners; material restores full cohort |
| High-risk signal distribution | Hard-trigger enumeration + soft-score table in validate tests |
| Holistic rewrite count | Exactly one rewrite then user-decision block (`rewrite_used`) |
| Implementation / fix rounds | Gate-cycle and strict-AND dual-review fixtures |

---

## Binding product contracts verified

| Contract | Evidence |
| --- | --- |
| `schema_version=2` only | validators + listener publish/settle fixture |
| `workflow_manifest_v2` / `workflow_v2` | connection + listener + companion catalog |
| Policy `b2d_task_risk_v1` | publish/settle and recovery fixtures |
| Normal route: Grok implementer + Codex reviewer | validate + gates |
| High route: Codex implementer + exactly Codex and Grok reviewers | validate + admission freeze of three nodes |
| Strict AND on same latest non-empty artifact digest | gates/project high-route tests |
| Final whole-branch behavior unchanged | skill_forward final scenarios + final gate suite |
| Grok tools/list ≤ 7680 bytes | measured **7669** |
| No cost product surface | evaluation-only report; no schema/API/UI cost code |

---

## Completion Evidence checklist

| Criterion | Mapped evidence | Status |
| --- | --- | --- |
| One Codex Author node + Plan digest/task ID coverage | admission/store author tasks e.g. `author-task-recovery` / `sha256:plan` | GREEN |
| Full initial Plan review, owner-only re-review, material reset | plan_review + store scoped/material tests | GREEN |
| Stable findings; one-rewrite / user-block boundaries | stagnation store fixture cycles 3 and 5 | GREEN |
| Versioned risk record + reason per Task | `b2d_task_risk_v1` + hard/soft reasons in recovery seed | GREEN |
| Exact normal/high implementer and reviewer sets | validate route matrix | GREEN |
| Strict AND over latest non-empty producer artifact | gates/project dual-review digests | GREEN |
| Persisted/recovered cohort, owner, counter, report-path, route | recovery store test | GREEN |
| Graph Author ordering, reviewer fan-out, redacted risk codes | project redacted projection tests | GREEN |
| v2-only capability + fixed Grok catalog budget | v1 reject + **7669**/7680 budget test | GREEN |
| Unchanged final whole-branch review behavior | skill_forward + final gate tests | GREEN |
| Skill pressure + Rust/frontend/static checks | validators 28/28; desktop 3771; server 3593; eslint 0 err; vitest 4275; build 33/33 | GREEN |
| Measured fan-out/token/time without cost product | partial 2070 tokens; external blockers for medians | GREEN (blockers explicit) |

---

## Scope of this evaluation commit

- **Creates only:** `docs/superpowers/performance/b2d-adaptive-routing-evaluation.md`
- **Does not modify:** runtime, frontend, Skill, tests, config, lockfiles
- **Prerequisite (already landed):** `285f01b6` style-only scripts fix

---

## Residual concerns (non-blocking for Task 10 product gates)

1. Live 3× normal/hard-high/score-high median table remains externally blocked.
2. Session 2070 provides only aggregate tokens (100438), not full comparison columns.
3. ESLint still emits 23 pre-existing warnings (0 errors); not introduced by adaptive routing.
4. Windows `core.autocrlf` can reintroduce CRLF in script worktrees; CI/LF trees are authoritative for prettier.
