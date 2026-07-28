# B2D Adaptive Routing — End-to-End Verification Evaluation

**Plan:** `docs/superpowers/plans/2026-07-27-brainstorm-to-delivery-adaptive-routing.md`
**Design:** `docs/superpowers/specs/2026-07-27-brainstorm-to-delivery-adaptive-routing-design.md`
**Branch:** `feat/b2d-adaptive-routing`
**Original Task 10 base:** `97b5d305d0730d7498d386a21c2d9f847ac222f7`
**Prerequisite lint repair:** `285f01b6695c0d230a36a0ba644315ae06f76780` — `style(scripts): satisfy frontend lint`
**Evaluation parent lineage:** `285f01b6` → evaluation → Task 4 score-high store fixture `319f9529`
**Verifier:** Grok (Task 10 evidence correction)
**Date:** 2026-07-28

## Summary

All mandatory **product** verification gates for adaptive routing are **GREEN** on a
fresh full matrix at HEAD `319f9529` (includes Task 4 score-high SQLite store
fixture). Ten deterministic design scenarios map to real backend fixtures; the
score-3 path uses both the soft-score validator table **and** the SQLite store
fixture `task4_score3_high_route_persists_and_recovers`. Comparative live multi-run
measurements remain **EXTERNAL BLOCKED** with a concrete host capability reason
(not a product RED). No cost product surface was added.

This report is evidence-only. It does not change runtime, Skill, or frontend
contracts.

---

## Prerequisite and owning-task lineage (not Task 10 product work)

| Item | Value |
| --- | --- |
| Lint repair commit | `285f01b6695c0d230a36a0ba644315ae06f76780` |
| Subject | `style(scripts): satisfy frontend lint` |
| Scope | Prettier layout only in `src-tauri/scripts/stage-codex-acp.mjs` |
| Score-high store fixture | `319f95296007d37f6f06e716f5a5a32a52c7e1ad` — `test(workflow): cover score-high integration` |
| Relation | Lint residual + Task 4 integration fixture; outside Task 10 File Map edits |

---

## Step 1 — Formatting and static contract scans

Fresh run from repository root (evidence correction round):

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | 0 | PASS |
| `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs` | 0 | PASS — 0 failures, **28** checks |
| `python …/quick_validate.py .agents\skills\brainstorm-to-delivery` | 0 | PASS — `Skill is valid!` |
| `rg -n "workflow_v1\|workflow_manifest_v1\|pair_frozen" …` | 0 (raw matches printed) | semantic-zero **PASS** (classified below) |
| `git diff --check 285f01b6..HEAD` | **2 before correction**; **0 after** (controller-retained) | Pre-fix HEAD had trailing whitespace on metadata lines 3–10 (observed exit **2**). Post-fix retained evidence: `fresh-git-diff-check-controller.exit.log` records `range_exit=0` for `git diff --check 285f01b6..HEAD`. |
| `git diff --check` (working tree) | **0** (controller-retained) | Same marker file records `worktree_exit=0` for `git diff --check`. |

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

Fresh evidence-correction run:

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo check` | 0 | PASS |
| `cargo test --features test-utils` (first full attempt) | **101** | **RED momentarily** — lib `3669` passed / **1** failed / `1` ignored: `update::install::tests::swap_dir_via_copy_keeps_backup_and_swaps` panicked with Windows `PermissionDenied` / `os error 5`. Unrelated to adaptive-routing modules. Log: `fresh-cargo-test-desktop.out.log`. |
| focused re-run of that filter | 0 | **1/1 ok** (environmental flaky permission). Log: `fresh-cargo-test-swap-dir-rerun.out.log`. |
| `cargo test --features test-utils` (binding full suite) | **0** (controller-retained) | **Controller** ran `cargo test --features test-utils` via `Start-Process` + `Process.WaitForExit()` (PID-only); outer **`controller_exit=0`** after **241.4s**. Explicit marker `fresh-cargo-test-desktop-controller3.exit.log` records `lib_result=3670 passed; 0 failed; 1 ignored`. Full stdout `fresh-cargo-test-desktop-controller3.out.log` continues through all bins/integration/doc targets with zero failures (aggregate **3772** passed / **0** failed / **1** ignored across targets). stderr: `fresh-cargo-test-desktop-controller3.err.log`. |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | 0 | PASS |

Binding product status for this report: **GREEN** on controller-retained full desktop suite (first attempt residual recorded above, not concealed).

Allowed non-failures: third-party `proc-macro-error2` future-incompat note;
documented `codeg-mcp` sidecar 0-byte placeholder warning.

Logs (under `.superpowers/sdd/2026-07-27-brainstorm-to-delivery-adaptive-routing/`):
- `fresh-cargo-test-desktop.out.log` (first attempt, exit 101)
- `fresh-cargo-test-desktop-controller3.out.log` / `.err.log` / `.exit.log` (**binding** exit 0)

---

## Step 3 — Server / MCP matrices (`src-tauri/`)

Fresh mandatory full server suite (prior retained log had 3592 pass / 1 fail in
`preflight_unavailable_clean_raise_fail_cancels_active_runner`; controller focused
re-run was 1/1; this round re-ran the **full** suite):

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo check --no-default-features --bin codeg-server` | 0 | PASS |
| `cargo test --no-default-features --bin codeg-server --lib` | 0 | **3594** passed, **0** failed, **1** ignored |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | 0 | PASS |
| `cargo check --no-default-features --bin codeg-mcp` | 0 | PASS |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | 0 | PASS |

### Grok tools/list budget (exact brief command)

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

Log: `.superpowers/sdd/…/fresh-cargo-test-server.out.log`, `fresh-budget-filter.out.log`

---

## Step 4 — Frontend matrix

From repository root (fresh):

| Command | Exit | Result |
| --- | ---: | --- |
| `pnpm eslint .` | **0** | **0 errors**, **23 warnings** (pre-existing hooks/unused-vars; not Task 10) |
| `pnpm test` | **0** | **316** test files, **4275** tests passed |
| `pnpm build` | **0** | Static export **33/33** routes; prerendered static content |

---

## Step 5 — Ten deterministic scenarios (product fixtures)

All exercised with real backend store/admission/gate/plan_review/validate
fixtures (not mocked gate booleans). Each primary fixture name below executed
**ok** inside the clean desktop matrix log.

| # | Scenario | Primary fixtures | Concrete task IDs / digests / routes / gate cycles / typed failures |
| ---: | --- | --- | --- |
| 1 | **normal** | `validate::normal_and_high_routes_match_agent_matrix`; `gates::task6_normal_route_requires_its_one_reviewer`; `project::task6_projects_normal_and_high_routes_with_redacted_policy_metadata` | Route cohort size **2**: Grok implementer + one Codex reviewer. Gate producer task `impl-1` gen **1** digest `digest-1`; single reviewer node `task-1-codex-reviewer` approve → `ExecutionGateReason::Passed`. Gate cycle: **not applicable** (execution-gate unit fixture, not Plan settle cycles). |
| 2 | **hard-high** | `validate::every_hard_trigger_forces_high_risk`; `admission::task5_high_risk_reviewers_cannot_share_child_and_route_freezes_three_nodes`; recovery high seed in `store::task4_plan_initial_round_persists_derived_state_and_full_recovery` | Hard kinds force high (6 enumerated kinds). High freezes **3** cohort nodes (`cohort_frozen`). Recovery seed: hard `concurrency_lifecycle` + `migration_destructive_persistence`, soft `shared_interface` score **1**, implementer `task-1-impl` (codex), reviewers include `task-1-rev-grok`. Soft-score-only path: **not applicable** (hard triggers present). |
| 3 | **score-3-high** | **SQLite store:** `store::task4_score3_high_route_persists_and_recovers`; validator table: `validate::soft_score_threshold_table_selects_risk` | Publication token `tok-task10-score3-high-store`. Soft score **3** with hard_triggers **[]**: `cross_runtime_or_process` score **2** + `shared_interface` score **1**. Reason: `three canonical soft-signal points require high-risk routing`. Route nodes: implementer `task-1-impl` (codex), reviewers `task-1-rev` + `task-1-rev-grok`. Persist: `manifest_revision` **1**, `risk_policy_version` `b2d_task_risk_v1`, level **High**. Recovery via `get_workflow_state_core` restores same score/route/signals. Gate cycles: **not applicable** (publish/recover only). Validator-only score table is supporting, **not** a substitute for the store fixture. |
| 4 | **scoped owner review** | `plan_review::owner_subset_is_derived_from_open_blocking_findings`; `scoped_round_accepts_a_new_finding_from_a_required_owner`; `store::task4_scoped_round_uses_active_owner_subset_and_material_requires_cohort` | Author task `author-task-scoped`; digest `sha256:plan`; owner subset `plan-reviewer-1`; evidence tasks `review-scoped-c1-1`, `review-scoped-c1-2`, `review-scoped-c2`; gate_cycle **1→2**. Pure unit owner derivation uses synthetic reviewers `reviewer-a/b/c` (no store task IDs — labeled unit-only). |
| 5 | **material full reset** | `plan_review::material_and_full_localized_revisions_restore_full_cohort`; material failure path in scoped store fixture | Unit: material/full localized restore complete cohort `reviewer-a/b/c`. Store: material settle with incomplete cohort → `WorkflowStoreError::PlanReview`; scoped owner path still uses digest `sha256:plan`, author `author-task-scoped`. |
| 6 | **one rewrite then user block** | `plan_review` stagnation suite; `store::task4_plan_stagnation_rewrite_then_user_decision_blocks` | Author `author-task-stagnation`; digest `sha256:plan`; reviewer evidence `review-stagnation-{1..5}`. After cycle **3**: `HolisticRewriteRequired`, stagnation_count=**2**, `rewrite_used=false`. After cycle **5**: `UserDecisionRequired`, `rewrite_used=true`, outcome **Blocked**, header `WorkflowState::Blocked`. Exactly **one** holistic rewrite before user-decision block. |
| 7 | **split high verdicts → both re-review** | `gates::task6_one_approval_plus_one_request_changes_fails_strict_and`; `project::task6_high_route_counts_strict_and_and_invalidates_both_old_approvals` | Strict **AND** over non-empty digest (`abc` / `digest-1`); one approve + one request_changes → fail (`ReviewerNotTerminalPass` on `task-1-grok-reviewer`). New producer invalidates both prior approvals. Plan gate cycles: **not applicable**. |
| 8 | **recovery** | `store::task4_plan_initial_round_persists_derived_state_and_full_recovery` | Author `author-task-recovery`; reviewers `review-task-recovery-1/2`; digest `sha256:plan`; reports `reports/author-recovery.md`, `reports/reviewer-*.md`; high policy + dual route persisted/recovered. |
| 9 | **v1 rejection** | `validate::v1_manifest_is_rejected`; `store::task4_publish_rejects_v1_manifest`; companion/connection denials | v1 / partial capability rejected; no fallback. Task IDs / digests: **not applicable** (rejection path before durable route evidence). |
| 10 | **pre-admission revision vs post-admission freeze** | `admission::task5_policy_revision_is_allowed_before_admission_but_frozen_afterward`; `store::workflow_v2_typed_error_real_producers_cohort_frozen` | Pre-admission material risk/route revision allowed (tokens `tok-task5-policy-before` → `tok-task5-policy-material`). Post-admission mutation → `WorkflowStoreError::CohortFrozen`. Artifact digests for this mutation path: **not applicable**. |

Supporting (executed ok):

- `skill_forward_routing_invariants_nine_scenarios` — Skill contract matrix
- `listener::workflow_manifest_v2_framed_publish_and_plan_settle_reach_store` — schema_version **2**, `risk_policy_version: b2d_task_risk_v1`

**Scenario matrix: GREEN.**

---

## Step 6 — Comparative measurements (**EXTERNAL BLOCKED**)

No new cost product code. Values only from auditable session tools / fixtures.

### Exact live-measurement blocker (current host/session)

1. Fresh `get_session_info(2070, max_messages=0)` returns **metadata only**:
   title `B2D会话writing-plans多轮并行审核耗时与Token优化分析`, agent `grok`,
   status `pending_review`, branch `main`, workspace `D:\MyCodeBuddy`,
   **message count 0**. **No** usage totals, timestamps, or reviewer-call counts
   are exposed. The prior claim of aggregate tokens **`100438` is deleted** as
   unverified against this read-only surface.
2. Validated unrestarted-host evidence: `get_workflow_capabilities` exposes only
   **`workflow_manifest_v1`** (no **`workflow_v2`**); `get_workflow_state` for
   the verifier parent returned **`workflow not found`**. Therefore this host
   cannot publish/run v2 B2D workflows, so three-run Codex/Grok availability
   measurements are not reachable without an external host restart / capability
   upgrade outside Task 10.

### Comparison table

| Row | Run count | Total tokens | Elapsed time | Plan reviewer calls | Task reviewer calls | Gate cycles |
| --- | --- | --- | --- | --- | --- | --- |
| Session 2070 baseline | 1 historical session id only | **unavailable** — `get_session_info` metadata-only (0 messages; no usage fields) | **unavailable** — no workflow start/finish timestamps in session metadata | **unavailable** — API does not return Plan reviewer call counts | **unavailable** — same | **unavailable** — same |
| Normal median (3 runs) | **not reachable** — host lacks `workflow_v2` publish/run (`workflow_manifest_v1` only; parent workflow not found) | same blocker | same blocker | same blocker | same blocker | same blocker |
| Hard-trigger-high median (3 runs) | same host `workflow_v2` unavailability | same blocker | same blocker | same blocker | same blocker | same blocker |
| Score-trigger-high median (3 runs) | same host `workflow_v2` unavailability | same blocker | same blocker | same blocker | same blocker | same blocker |

### Structural metrics from fixtures (not live medians)

| Metric | Numeric / boundary evidence |
| --- | --- |
| Normal Task cohort size | **2** nodes (Grok implementer + Codex reviewer) |
| High Task cohort size | **3** nodes (Codex implementer + Codex reviewer + Grok reviewer); admission freezes three |
| Soft-score distribution / boundary | Scores **0,1,2 → normal**; **3,4 → high** (`soft_score_threshold_table_selects_risk`); score-3 store uses 2+1 soft signals, hard=[] |
| Scoped Plan owner subset vs complete cohort | Owner subset = open Critical/Important finding owners (e.g. store owner `plan-reviewer-1` only); material/full restore complete cohort (unit cohort size **3**: `reviewer-a/b/c`) |
| Holistic rewrite count | Exactly **one** rewrite (`rewrite_used` false at cycle 3, true at cycle 5) then user-decision block |
| Implementation / fix rounds | **unavailable** as live multi-run counts; fixture-level gate invalidation paths exist (new producer invalidates prior approvals) but do not supply multi-run medians |
| Live token / elapsed medians | **unavailable** (EXTERNAL BLOCKED as above) |

---

## Binding product contracts verified

| Contract | Evidence |
| --- | --- |
| `schema_version=2` only | validators + listener publish/settle fixture |
| `workflow_manifest_v2` / `workflow_v2` | connection + listener + companion catalog tests (product); live host capability still v1-only for this verifier session |
| Policy `b2d_task_risk_v1` | publish/settle, recovery, and score-3 store fixtures |
| Normal route: Grok implementer + Codex reviewer | validate + gates |
| High route: Codex implementer + exactly Codex and Grok reviewers | validate + admission freeze of three nodes + score-3 store |
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
| Versioned risk record + reason per Task | `b2d_task_risk_v1` + hard/soft reasons; score-3 reason string | GREEN |
| Exact normal/high implementer and reviewer sets | validate route matrix + score-3 store nodes | GREEN |
| Strict AND over latest non-empty producer artifact | gates/project dual-review digests | GREEN |
| Persisted/recovered cohort, owner, counter, report-path, route | recovery store test + score-3 persist/recover | GREEN |
| Graph Author ordering, reviewer fan-out, redacted risk codes | project redacted projection tests | GREEN |
| v2-only capability + fixed Grok catalog budget | v1 reject + **7669**/7680 budget test | GREEN |
| Unchanged final whole-branch review behavior | skill_forward + final gate tests | GREEN |
| Skill pressure + Rust/frontend/static checks | validators 28/28; desktop **3772**; server **3594**; eslint 0 err; vitest **4275**; build 33/33 | GREEN |
| Measured fan-out/token/time without cost product | session 2070 metadata-only; host `workflow_manifest_v1` only (no `workflow_v2`); parent workflow not found | **EXTERNAL BLOCKED** |

---

## Scope of this evaluation commit

- **Updates only:** `docs/superpowers/performance/b2d-adaptive-routing-evaluation.md`
- **Does not modify:** runtime, frontend, Skill, tests, config, lockfiles
- **Prerequisite / Task 4 (already landed):** lint repair `285f01b6`; score-high store fixture `319f9529`

---

## Residual concerns

1. Live 3× normal/hard-high/score-high median table remains **EXTERNAL BLOCKED**:
   host exposes only `workflow_manifest_v1` (no `workflow_v2`); parent workflow
   not found; therefore this host cannot publish/run v2 B2D workflows.
2. Session 2070 yields metadata only (0 messages); no token/elapsed/reviewer
   columns are available from `get_session_info`.
3. ESLint still emits 23 pre-existing warnings (0 errors); not introduced by adaptive routing.
4. First desktop full-suite attempt flaked once on
   `update::install::tests::swap_dir_via_copy_keeps_backup_and_swaps` (Windows
   permission); binding full suite is the controller-retained exit-0 run above.
   Not an adaptive-routing product defect.
5. Windows `core.autocrlf` can reintroduce CRLF in script worktrees; CI/LF trees remain
   authoritative for prettier.
