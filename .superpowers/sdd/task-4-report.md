# Task 4 Report — Lease Registry State Machine

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Commit:** `78a49180` — `feat(acp): add semantic tool execution leases`  
**Date:** 2026-07-23

## Summary

Implemented the host-owned **ToolExecutionLeaseRegistry** state machine and bounded semantic-progress fingerprints (no connection wiring, no supervisor cancellation):

1. **`progress.rs`** — `ProgressFingerprint` + `apply_semantic_progress`; renews only on new offset/status/MCP/delegation/agent facts; never retains output text.
2. **`registry.rs`** — Full CAS/versioned lease lifecycle: register, bind capability, progress, pause/resume, complete, scan, warning→grace split, extend, claim_cancel, settings disable/re-enable, untracked fallback (1800+600).
3. **`mod.rs`** — Exports registry + progress + types.

## Files changed

| File | Change |
| --- | --- |
| `src-tauri/src/acp/tool_watchdog/registry.rs` | **Create** — registry API, state machine, 16 controlled-clock tests |
| `src-tauri/src/acp/tool_watchdog/progress.rs` | **Create** — fingerprint apply + unit tests |
| `src-tauri/src/acp/tool_watchdog/mod.rs` | Re-export registry/progress modules |

Design doc edits under `docs/superpowers/specs/` were **not** committed (out of scope).

## Behavior delivered

| Rule | Implementation |
| --- | --- |
| 599s quiet / 600s warn only | `scan` Running → Warning emits `PublishWarning` only |
| Warning publish starts full grace | `warning_published` captures `grace_seconds`, sets deadline = at + grace |
| No same-pass warn+cancel | Warning and Grace are separate transitions |
| Extend | Grace only; version++; new deadline; **not** last_progress_at |
| Progress in Warning/Grace | → Running; clear warning fields; new progress window |
| Duplicate offset/status | Fingerprint no-op; no renew |
| Pause/resume | Permission/question/waiting_input; resume fresh window |
| Disable | Clears Warning/Grace → Running without inventing progress |
| Re-enable | May warn overdue work; cannot cancel same scan |
| Single winner | Complete loses after Cancelling; late_activity++; progress cannot revive |
| Stale CAS | Wrong version/incarnation/turn → `StaleLease` |
| Ambiguous terminal | Default capability `Turn`; bind upgrades when unambiguous |
| Untracked fallback | Fixed 1800s warn + 600s grace; independent of live warning_after |
| Fallback lifecycle | start_turn register; register_tool retire; complete re-arm if eligible; background handoff blocks re-arm |
| Public projection | `ToolCategory` only; no provider `tool_call_id` on wire |

### Extra host helpers (for Task 5, tested here)

- `record_tool_progress_at` / `record_turn_progress_at` — controlled-clock injection
- `set_verified_background_work` — background handoff eligibility
- `fallback_eligible(...)` — pure predicate (single source of truth)
- `late_activity` / `lease_phase` / `has_fallback` — inspection for tests/host

## TDD evidence (brief coverage list)

| Requirement | Test |
| --- | --- |
| 599s running; 600s warning only | `running_599s_no_warning_600s_warning_only` |
| Warning publication starts new 600s grace | `warning_publication_starts_new_600s_grace` |
| Extension version/deadline not last_progress | `extension_changes_version_and_deadline_not_last_progress` |
| Progress clears warning/grace | `progress_in_warning_and_grace_returns_to_running` |
| Duplicate terminal / unchanged offset | `duplicate_terminal_snapshot_and_unchanged_offset_do_not_renew` + progress unit tests |
| Permission pause; resume fresh window | `permission_pause_and_resume_fresh_progress_window` |
| Disable clears without inventing progress | `disable_clears_warning_grace_without_inventing_progress` |
| Re-enable warns, no same-pass cancel | same + `setting_reduction_warns_but_no_same_pass_cancel` |
| Completion vs stop vs timeout one winner | `completion_progress_user_stop_timeout_single_winner` |
| Stale lease/version/incarnation/turn | `stale_lease_version_incarnation_turn_generation_rejected` |
| Ambiguous terminal → Turn only | `ambiguous_terminal_binding_retains_only_turn_capability` |
| Untracked 1800+600 | `untracked_turn_uses_1800_plus_600_timing` |
| Fallback retire/re-arm + background | `register_tool_retires_fallback_complete_rearms_when_eligible` |
| ToolCategory projection | `public_projection_uses_tool_category_not_free_form_title` |
| Agent activity renews fallback only | `agent_activity_renews_fallback_only` |
| fallback_eligible predicate | `fallback_eligible_predicate` |

## Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils tool_watchdog::registry -- --nocapture
cargo clippy --lib --features test-utils -- -D warnings
```

| Check | Result |
| --- | --- |
| `tool_watchdog::registry` | **16 passed**, 0 failed |
| `tool_watchdog` (types+progress+registry) | **29 passed** |
| clippy `-D warnings` | **clean** |

## Concerns / follow-ups

1. **Task 5** must wire registry into `ConnectionManager`, stamp incarnation, feed progress from terminal/MCP/delegation sources, and call `set_verified_background_work` on acknowledged background handoff.
2. **Task 6** owns supervisor: publish warning actions, call `warning_published`, execute `CancellationClaim`, escalate after `CANCEL_CONVERGENCE_SECS`.
3. **`record_tool_progress` without `at`** uses wall clock `WatchdogInstant::now()`; production wiring should prefer `record_tool_progress_at` with the same host clock used by `scan`.
4. **`session_id` on lease** is retained for cancel routing but unused until Task 5/6 (`#[allow(dead_code)]`).
5. Fallback `tool_call_id` uses host constant `__untracked_turn__` on internal stamps only; public projections still omit free-form titles via `ToolCategory::Other`.

---

## Review fix-up (Task 4 review findings)

**Status:** FIXED  
**Review:** `.superpowers/sdd/task-4-review.md` (`3 High`, `2 Medium`)  
**Date:** 2026-07-23

### Fixes

| Severity | Finding | Fix |
| --- | --- | --- |
| High | Fallback grace used live `settings.grace_seconds` | `warning_published` captures `DEFAULT_GRACE_SECS` for fallback leases |
| High | `complete_turn` completed `Cancelling` leases | Cancelling retains claim; `late_activity++` only; lease stays |
| High | Cancelling tracked omitted from fallback eligibility | `is_tracked_present()` includes Cancelling; blocks re-arm |
| Medium | Duplicate agent hash advanced re-arm baseline | Turn-level `agent_content_hash`; baseline only on new fact |
| Medium | `actionable_projections` exposed Warning | Actionable = Grace + Cancelling only (Warning publish-transition) |

### Regression tests added

| Test | Guards |
| --- | --- |
| `untracked_fallback_grace_ignores_live_grace_seconds_setting` | grace_seconds=60 still cancels at 1800+600 |
| `complete_turn_does_not_complete_cancelling_lease` | claim survives complete_turn |
| `cancelling_tracked_lease_blocks_fallback_rearm` | resume / background-clear cannot re-arm |
| `duplicate_agent_activity_hash_does_not_postpone_fallback_rearm` | re-arm baseline stays on first accepted hash |
| `actionable_projections_exclude_warning_until_grace` | empty between scan and warning_published |

### Verification (post-fix)

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils tool_watchdog::registry -- --nocapture
cargo clippy --lib --features test-utils -- -D warnings
```

| Check | Result |
| --- | --- |
| `tool_watchdog::registry` | **21 passed**, 0 failed |
| clippy `-D warnings` | **clean** |
