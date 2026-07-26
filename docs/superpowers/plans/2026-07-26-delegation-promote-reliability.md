# Delegation Promote Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make post-prompt `promote_running` write-first and atomic, classify admission failures distinctly from spawn failures, fail-close pre-send binds on gen-1 and continuation, and give explicit non-replay recovery for known/unknown admission outcomes.

**Architecture:** Harden `RunStore::promote_running` as a write-first SQLite transaction with typed results and atomic conversation projection; share one broker failure helper after prompt accept; durable-bind child connections before any prompt send on both gen-1 and continuation; split startup reconcile into unbound `host_restarted` vs bound `admission_unknown`; extend replacement reasons and wire/cold reports without frontend redesign.

**Tech Stack:** Rust (Tauri/Axum backend), SeaORM + SQLite, existing ACP delegation broker/run_store/metrics/tool_schema tests under `src-tauri/`.

## Global Constraints

- Work only in worktree `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability` on branch `feat/delegation-promote-reliability`.
- Spec baseline: `docs/superpowers/specs/2026-07-26-delegation-promote-reliability-design.md` (approved r3).
- Provider-neutral: no CodeBuddy limits, no per-child homes, no global promote mutex.
- No automatic prompt replay after post-send or crash-ambiguous admission.
- No historic `spawn_failed` row migration; no frontend card redesign.
- Do not refactor unrelated persistence (e.g. making `settle_terminal` write-first is out of scope).
- TDD per task; local commits only; no push/PR.
- PowerShell for shell commands.
- Parent SDD roles: implementer = Grok; task/final reviewer = Codex only.
- Residual Task 8 fixes must stay inside the File Map paths below (or require plan amendment).

## File Map

| File | Responsibility |
| --- | --- |
| `src-tauri/src/acp/delegation/run_store.rs` | Write-first promote, typed result, bind fail-closed semantics, projection clear flags, reconcile split, replacement matchers, tests |
| `src-tauri/src/acp/delegation/broker.rs` | Gen-1 pre-send bind; fail-closed `begin_run_admission*`; shared post-accept failure helper; settlement ownership; metrics emission; gen-1/continue path wiring; replacement ack warning; tests |
| `src-tauri/src/acp/delegation/store.rs` | Promote-local retry policy constant and/or typed SQLite primary/extended code extraction helpers (minimal) |
| `src-tauri/src/acp/delegation/metrics.rs` | `accepted_by_agent`, promote/admission/settlement counters, audit static codes, snapshot serde defaults |
| `src-tauri/src/acp/delegation/spawner.rs` | `AcceptedDelegationPrompt.prompt_accepted_at` / docs — no post-send conversation timestamp requirement |
| `src-tauri/src/acp/manager.rs` | Stop reading `delegation_started_at` after accept; pass accept timestamp into promote path |
| `src-tauri/src/acp/delegation/types.rs` | Wire/`DelegationError` codes; cold-report `admission_unknown` warning text |
| `src-tauri/src/acp/delegation/listener.rs` | `replacement_reason` allow-list |
| `src-tauri/src/acp/delegation/tool_schema.json` | Enum values `admission_failed`, `admission_unknown` + description text (explicit-replacement-only recovery) |
| `src-tauri/src/acp/delegation/companion.rs` | Tool/companion description text for explicit-replacement-only recovery of new codes |
| `src-tauri/src/acp/delegation/attention.rs` | **Task 8 test-only residual:** bind-before-promote in fixture helpers after Task 3/4 claim filter (no production path change) |
| `src-tauri/tests/delegation_session_reuse_integration.rs` | **Task 8 test-only residual:** bind-before-promote + `Ok(_)` match for `promote_running` → `PersistedRun` (no production path change) |

## Serial Safety Notes

1. **Bind before strict claim filter:** Task 1 promote claim filter is `task_id` + `status=reserving` only, and may still write `child_connection_id` when null (legacy gen-1). Task 3 adds fail-closed pre-send bind. Task 4 removes promote-as-first-writer and tightens claim filter to expected `child_connection_id`.
2. **Typed promote outcomes vs broker callers:** Task 1 ships a temporary **compatibility adapter** so existing `if let Err(e) = promote_running(...).await` call sites keep compiling: non-success outcomes map to `Err(TaskStoreError::…)` until Task 4 switches both branches to match `PromoteRunningOutcome` directly. Adapter tests are required in Task 1.
3. **BUSY_SNAPSHOT (517):** same bounded retry rail as ordinary busy/locked; log as invariant regression; count `busy_snapshot` when extended code 517 is extracted.

---

### Task 1: Typed write-first `promote_running` + promote-local retry + timestamp math

**Files:**
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs` (promote retry policy + SQLite code extraction)
- Test: `run_store` / `store` module tests

**Interfaces:**
- Consumes: existing entity + budget charge helpers
- Produces:
  ```rust
  /// Public outcome; Task 4 matches this enum directly.
  /// Retry metadata on success paths enables Task 7 promote_retries metrics
  /// without changing the Task 1 API later. Counts every transient class
  /// observed across attempts (mixed BUSY then LOCKED must both count).
  pub struct PromoteAttemptMeta {
      pub attempts: u32, // total attempts used (1..=3)
      pub busy_retries: u32,
      pub locked_retries: u32,
      pub busy_snapshot_retries: u32,
  }

  pub enum PromoteRunningKind {
      Promoted { run: PersistedRun /* real public type */ },
      AlreadyRunning { run: PersistedRun },
      TerminalWinner { run: PersistedRun },
      BudgetExhausted { message: String },
      StateConflict { class: PromoteConflictClass, message: String },
      RetryExhausted { class: PromoteRetryClass, message: String },
      Permanent { message: String },
  }

  /// Every outcome (success or failure) carries attempt meta so mixed
  /// transient retries are never dropped before classification.
  pub struct PromoteRunningOutcome {
      pub kind: PromoteRunningKind,
      pub meta: PromoteAttemptMeta,
  }

  /// Task 1 compatibility wrapper for current Err-only callers.
  pub async fn promote_running(
      &self,
      task_id: &str,
      child_connection_id: &str,
      prompt_accepted_at: DateTime<Utc>,
  ) -> Result<PersistedDelegationRun, TaskStoreError>
  // maps Promoted/AlreadyRunning -> Ok(run); other outcomes -> Err(...)

  pub async fn promote_running_detailed(
      &self,
      task_id: &str,
      child_connection_id: &str,
      prompt_accepted_at: DateTime<Utc>,
  ) -> Result<PromoteRunningOutcome, TaskStoreError> // only I/O map failures here if needed
  ```
  - Claim filter (Task 1): `task_id` + `status=reserving` (no required connection match yet)
  - May still set `child_connection_id` when null (legacy gen-1) until Task 4
  - Transaction order: claim write → read/validate → budget charge → status/timestamps → (projection deferred to Task 2) → commit
  - Timestamps (mandatory): sample `promote_at = max(Utc::now(), prompt_accepted_at)`; persist `started_at = prompt_accepted_at`, `reached_running_at = promote_at`
  - Retry: 3 attempts; delays 10ms then 25ms; ordinary BUSY/LOCKED **and** BUSY_SNAPSHOT(517) on same rail; dedicated promote policy
  - Zero-row claim: reread outside rolled-back txn
  - Ambiguous permanent/commit error: reread outside txn:
    - matching running → `AlreadyRunning`/`Promoted` success (with meta)
    - terminal → `TerminalWinner`
    - still reserving / missing / mismatched → `Permanent` or `StateConflict`
  - Success outcomes carry `PromoteAttemptMeta` for retry metrics

- [ ] **Step 1: Write failing tests** named at least:
  - `promote_write_first_survives_concurrent_writer`
  - `promote_retries_busy_then_succeeds`
  - `promote_retries_locked_then_succeeds`
  - `promote_retries_busy_snapshot_517_then_succeeds`
  - `promote_retry_exhausted_no_partial_writes`
  - `promote_budget_exhaust_rolls_back_no_charge`
  - `promote_success_charges_recovery_budget_exactly_once`
  - `promote_zero_row_already_running_idempotent`
  - `promote_zero_row_terminal_replays_winner`
  - `promote_zero_row_ownership_conflict`
  - `promote_commit_ambiguity_reread_running_is_success`
  - `promote_commit_ambiguity_reread_terminal_winner`
  - `promote_commit_ambiguity_reread_still_reserving_is_permanent`
  - `promote_commit_ambiguity_reread_mismatched_is_conflict`
  - `promote_success_meta_reports_per_class_retry_counts`
  - `promote_reached_running_at_ge_started_at`
  - `promote_running_compat_maps_budget_exhausted_to_err`

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils promote_ -- --nocapture
```

Expected: FAIL (missing symbols / assertions).

- [ ] **Step 3: Implement write-first promote, detailed + compat APIs, promote-local retry, SQLite code extraction, timestamp math**

- [ ] **Step 4: Run green** (same command) + `cargo check`

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/store.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "feat(delegation): write-first promote_running with typed outcomes"
```

---

### Task 2: Atomic running conversation projection + clearable `finished_at`

**Files:**
- Modify: `src-tauri/src/acp/delegation/run_store.rs`

**Interfaces:**
- Consumes: Task 1 promote transaction
- Produces: projection sets `InProgress`, `delegation_started_at=prompt_accepted_at`, clears `error_code` + `finished_at` via nested `Option<Option<_>>` or clear flag, resets generation rollups; equal-generation re-project `Ok(true)`; newer-generation fence `Ok(false)` rolls back promote as state conflict

- [ ] **Step 1: Write failing tests**
  - `promote_projects_running_generation_and_started_at`
  - `promote_gen2_overwrites_projection_gen1_fence_rolls_back`
  - `promote_equal_generation_reproject_succeeds`
  - `promote_clears_prior_terminal_finished_at_and_error_code`
  - `promote_resets_generation_rollups`

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils promote_ -- --nocapture
```

- [ ] **Step 3: Implement projection representation + promote integration**

- [ ] **Step 4: Run green + `cargo check`**

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add src-tauri/src/acp/delegation/run_store.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "feat(delegation): project running generation atomically in promote"
```

---

### Task 3: Fail-closed pre-send bind (gen-1 + continuation)

**Files:**
- Modify: `src-tauri/src/acp/delegation/run_store.rs` (`bind_child_connection_while_reserving`)
- Modify: `src-tauri/src/acp/delegation/broker.rs` (`begin_run_admission*`, gen-1 path before send)

**Interfaces:**
- Consumes: existing bind helper + admission handoff
- Produces:
  - Bind success only first-bind or same-connection idempotent
  - Different-connection → typed permanent conflict (not Ok)
  - `begin_run_admission*` surfaces bind failure (`Result` / typed reject)
  - Gen-1 binds before `send_prompt_linked_for_delegation`
  - On bind failure: unwind live registration/reservation/inflight; **disconnect** unused child; no prompt send; pre-admission error (not `admission_failed`)

- [ ] **Step 1: Write failing tests**
  - `bind_different_connection_is_permanent_conflict`
  - `begin_run_admission_bind_failure_unwinds_and_errors`
  - `gen1_bind_before_send_success_path`
  - `gen1_bind_failure_no_prompt_and_disconnects`
  - `continue_bind_failure_no_prompt_unwinds_and_disconnects`
  - Update mechanical `begin_run_admission` test call sites for `Result`

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils bind_ -- --nocapture
cargo test --features test-utils begin_run_admission -- --nocapture
cargo test --features test-utils gen1_bind -- --nocapture
```

- [ ] **Step 3: Implement helper + broker fail-closed paths**

- [ ] **Step 4: Run green + `cargo check`**

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "feat(delegation): fail-closed pre-send child connection bind"
```

---

### Task 4: Shared post-accept failure helper + claim-filter tighten + settlement ownership

**Files:**
- Modify: `src-tauri/src/acp/delegation/run_store.rs` (claim filter requires expected `child_connection_id`; stop first-writing connection on success path)
- Modify: `src-tauri/src/acp/delegation/broker.rs` (shared helper; gen-1 + continue paths; finalizer/worker same-owner; structured promote/settlement logs)

**Interfaces:**
- Consumes: Task 1 `PromoteRunningOutcome` / `promote_running_detailed`; Task 3 pre-bound connection
- Produces:
  - Claim filter: `task_id` + `reserving` + `child_connection_id = expected`
  - Promote retains bound connection only (no null→id first write on success)
  - Shared helper after prompt accept for gen-1 and continuation:
    1. Classify detailed outcome
    2. Terminal winner → replay + idempotent cancel/disconnect
    3. Already-running → success (no double accepted metric)
    4. Budget → `budget_exhausted`; else `admission_failed` (**never** `spawn_failed`; do not re-enter `store_err_to_delegation_error` collapse for promote outcomes)
    5. Claim local first-terminal; cancel (cancel failure non-blocking); settle intended code (no PE-rewrite arm)
    6. Existing different retry payload → adopt/observe FWW
    7. Transient exhaust → `PendingTerminalRetry` with original terminal
    8. Permanent miss → install frozen ownership before coordination release; caller gets sanitized `persistence_error`
  - Finalizer + retry worker recognize `admission_failed` and `budget_exhausted` as same-owner intended payloads (with `unresumable` / `persistence_error`)
  - Structured logs (task_id, generation, agent_type, admission_class, attempt, sqlite primary/extended when available, failure class); no prompt/secrets

- [ ] **Step 1: Write failing tests**
  - `gen1_promote_transient_then_success_no_cancel`
  - `continue_promote_transient_then_success`
  - `promote_retry_exhaust_settles_admission_failed_not_spawn_failed`
  - `promote_budget_exhaust_settles_budget_exhausted`
  - `promote_failure_first_terminal_wins_replay`
  - `promote_settlement_retry_keeps_admission_code`
  - `promote_permanent_settlement_freeze_ownership`
  - `promote_existing_retry_owner_different_payload_adopted`
  - `finalizer_recognizes_admission_failed_and_budget_exhausted_same_owner`
  - `cancel_failure_does_not_block_settlement`
  - `promote_claim_requires_expected_child_connection`

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils promote_ -- --nocapture
cargo test --features test-utils admission_ -- --nocapture
cargo test --features test-utils finalizer_ -- --nocapture
cargo test --features test-utils cancel_failure -- --nocapture
```

- [ ] **Step 3: Implement helper, both branches, claim filter, finalizer/worker recognition, logs**

- [ ] **Step 4: Run green + `cargo check`**

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "fix(delegation): honest post-accept admission failure handling"
```

---

### Task 5: `admission_failed` / `admission_unknown` recovery surface + warnings

**Files:**
- Modify: `src-tauri/src/acp/delegation/tool_schema.json` (enum + description: explicit replacement only)
- Modify: `src-tauri/src/acp/delegation/companion.rs` (description text)
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs` (constants, matchers, lineage supersession, continue deny-list)
- Modify: `src-tauri/src/acp/delegation/types.rs` (cold-report warning)
- Modify: `src-tauri/src/acp/delegation/broker.rs` (**replacement acknowledgement** warning on successful `admission_unknown` replacement)

**Interfaces:**
- Consumes: durable codes + lineage fields (`replaced_task_id`, work_unit_key)
- Produces:
  - `replacement_reason` includes `admission_failed` / `admission_unknown`
  - Match only lineage-latest eligible sources with `reached_running_at IS NULL`
  - Superseded sources rejected
  - Forge matrix rejected: completed/running/reached-running/stale/mismatched-agent/incomplete-snapshot
  - Failed replacement does not consume budget; exactly one successful replacement promote does
  - Cold failed report + successful replacement ack contain duplicate-execution warning for `admission_unknown`
  - Codes not continuable / not `unresumable`

- [ ] **Step 1: Write failing tests** covering full recovery matrix above + broker replacement ack warning

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils replacement_ -- --nocapture
cargo test --features test-utils admission_ -- --nocapture
cargo test --features test-utils cold_message -- --nocapture
```

- [ ] **Step 3: Implement all surfaces**

- [ ] **Step 4: Run green + `cargo check`**

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add `
  src-tauri/src/acp/delegation/tool_schema.json `
  src-tauri/src/acp/delegation/companion.rs `
  src-tauri/src/acp/delegation/listener.rs `
  src-tauri/src/acp/delegation/run_store.rs `
  src-tauri/src/acp/delegation/types.rs `
  src-tauri/src/acp/delegation/broker.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "feat(delegation): admission_failed/unknown explicit replacement recovery"
```

---

### Task 6: Startup reconcile bound/unbound split

**Files:**
- Modify: `src-tauri/src/acp/delegation/run_store.rs`

**Interfaces:**
- Consumes: Task 3 bind semantics; Task 5 `admission_unknown` code/eligibility
- Produces:
  - Unbound reserving → existing safe `host_restarted`
  - Bound reserving → `failed/admission_unknown` + audit `{ prior_status: reserving, restart_provenance: ... }`
  - Not continuable; not auto-replay
  - Comment: process-local `PendingTerminalRetry` does not survive restart

- [ ] **Step 1: Write failing tests**
  - `reconcile_unbound_reserving_host_restarted`
  - `reconcile_bound_reserving_admission_unknown_with_audit`
  - `gen1_post_accept_pre_promote_bound_crash_not_continuable`
  - `admission_unknown_replacement_eligible`

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils reconcile_ -- --nocapture
cargo test --features test-utils admission_unknown -- --nocapture
cargo test --features test-utils gen1_post_accept -- --nocapture
```

- [ ] **Step 3: Implement split**

- [ ] **Step 4: Run green + `cargo check`**

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add src-tauri/src/acp/delegation/run_store.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "fix(delegation): split reserving restart into host_restarted vs admission_unknown"
```

---

### Task 7: Per-generation timestamps (accept path) + metrics

**Files:**
- Modify: `src-tauri/src/acp/delegation/spawner.rs`, `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/delegation/metrics.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs` (continuation accepted metric; agent_type)

**Interfaces:**
- Consumes: Task 1 timestamp persistence; Task 4 success path
- Produces:
  - Accept path samples `prompt_accepted_at` without post-send conversation lookup
  - Live runtime rebase uses same timestamp
  - `accepted_count` + `accepted_by_agent` for all generations; no double-count on idempotent/commit-reread
  - Counter maps with documented pairing; snapshot fields retained; new maps default-empty serde
  - Interned audit code constants

- [ ] **Step 1: Write failing tests**
  - `gen1_gen2_distinct_prompt_accepted_at`
  - `run_projection_runtime_share_prompt_accepted_at`
  - `reached_running_at_ge_started_at` (broker/e2e if not only unit)
  - `stale_gen1_conversation_timestamp_not_reread`
  - `continuation_increments_accepted_count_and_by_agent`
  - `idempotent_promote_no_double_accepted_metric`
  - `commit_reread_success_emits_accepted_exactly_once`
  - `promote_failures_labels_cas_budget_busy_exhausted_permanent`
  - `admission_failed_by_agent_increments_on_admission_failed`
  - `settlement_retry_counter_pairing_new_vs_existing_owner`
  - `busy_snapshot_metric_only_on_extended_517`
  - `metrics_snapshot_default_empty_maps_serde`
  - `structured_promote_logs_include_required_fields_exclude_secrets`

- [ ] **Step 2: Run red**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils accepted_ -- --nocapture
cargo test --features test-utils metrics -- --nocapture
cargo test --features test-utils prompt_accepted -- --nocapture
cargo test --features test-utils promote_failures -- --nocapture
cargo test --features test-utils admission_failed_by_agent -- --nocapture
cargo test --features test-utils settlement_retry -- --nocapture
cargo test --features test-utils structured_promote -- --nocapture
cargo test --features test-utils commit_reread -- --nocapture
cargo test --features test-utils reached_running_at -- --nocapture
cargo test --features test-utils stale_gen1 -- --nocapture
cargo test --features test-utils busy_snapshot -- --nocapture
```

- [ ] **Step 3: Implement accept-path timestamps + metrics**

- [ ] **Step 4: Run green + `cargo check`**

- [ ] **Step 5: Commit**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability add `
  src-tauri/src/acp/delegation/spawner.rs `
  src-tauri/src/acp/manager.rs `
  src-tauri/src/acp/delegation/metrics.rs `
  src-tauri/src/acp/delegation/broker.rs
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -m "feat(delegation): per-generation accept timestamps and admission metrics"
```

---

### Task 8: Full verification matrix + residual cleanup

**Files:** only File Map paths; update `.superpowers/sdd/progress.md`

- [ ] **Step 1: Run full matrix**

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo fmt --check
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: all pass.

- [ ] **Step 2: Fix failures only within File Map; re-run affected commands**

- [ ] **Step 3: Commit residual fixes if any**

```powershell
git -C D:\MyCodeBuddy\.worktrees\delegation-promote-reliability commit -am "chore(delegation): green verification matrix for promote reliability"
```

---

## Spec Coverage Checklist

| Spec area | Task(s) |
| --- | --- |
| Write-first promote + typed results + timestamps in promote | 1 |
| Promote-local retry + BUSY/LOCKED/517 rail | 1 |
| Commit-ambiguity reread | 1, 4 |
| Atomic projection + clear finished_at | 2 |
| Fail-closed bind gen-1 + continue | 3 |
| Shared failure helper / no discard settlement / same-owner finalizer | 4 |
| Wire not `spawn_failed` post-accept | 4 |
| Settlement freeze ownership | 4 |
| Replacement reasons + lineage supersession + forge matrix | 5 |
| admission_unknown cold + replacement ack warnings | 5 |
| Continue deny-list | 5 |
| Startup bound/unbound split | 6 |
| Accept-path timestamps + metrics | 7 |
| Full verification | 8 |

## Plan Review Adjudication (r1 → r2)

| Source | Verdict | Parent action |
| --- | --- | --- |
| Grok | Approve with fixes | Applied I1–I7 into this revision |
| CodeBuddy:KimiK3 | Approve with fixes | Applied finalizer same-owner + log ownership + minors |
| Codex | Request changes (serial safety Critical) | Split claim filter; compat adapter; BUSY_SNAPSHOT rail; broker replacement warning; concrete filters |

## Type Consistency

- Task 1: `promote_running` (compat) + `promote_running_detailed` (canonical)
- Task 4: switches callers to detailed outcomes; tightens claim filter after Task 3 bind
- Task 5 codes consumed by Task 6 reconcile
- Task 7 metrics/timestamps build on Tasks 1+4 success paths
