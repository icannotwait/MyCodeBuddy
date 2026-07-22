# Task 7 Report — Persist Settings and Expose Shared Control APIs

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Commit:** `27500da5` — `feat(acp): expose tool watchdog settings and controls`  
**Base:** `ab3c5589` — `fix(acp): advance watchdog warnings into Grace on production scan`  
**Date:** 2026-07-23  
**Worktree:** `D:\MyCodeBuddy\.worktrees\tool-execution-watchdog`

## Summary

Task 7 adds durable tool-watchdog settings, shared desktop/server control
APIs, live registry apply after successful persist, secret-safe metrics, and
startup settings load before the existing single supervisor loop.

## What landed

### Settings persistence + clamp

Exact `app_metadata` keys (no migration):

| Key | Default |
| --- | --- |
| `tool_watchdog.enabled` | `true` |
| `tool_watchdog.warning_after_seconds` | `600` |
| `tool_watchdog.grace_seconds` | `600` |

- Missing / non-numeric / non-bool values → product defaults
- Durations clamped to `60..=3600` (59→60, 3601→3600)
- Live registry is updated **only after** a successful DB transaction

### Shared cores + transport

| Operation | Core | Tauri | Axum (`POST /api/...`) |
| --- | --- | --- | --- |
| Get settings | `acp_get_tool_watchdog_settings_core` | `acp_get_tool_watchdog_settings` | `/acp_get_tool_watchdog_settings` |
| Set settings | `acp_set_tool_watchdog_settings_core` | `acp_set_tool_watchdog_settings` | `/acp_set_tool_watchdog_settings` |
| Extend | `acp_tool_watchdog_extend_core` | `acp_tool_watchdog_extend` | `/acp_tool_watchdog_extend` |
| Cancel | `acp_tool_watchdog_cancel_core` | `acp_tool_watchdog_cancel` | `/acp_tool_watchdog_cancel` |

- Extend/cancel request bodies contain only `lease_id` + `version`
- Stale CAS returns message/code `stale_tool_watchdog_lease` without mutation
- Cancel claims `UserStop`, emits Cancelling, escalates in background

### Startup + supervisor

- Desktop (`lib.rs`) and server (`codeg_server.rs`) call
  `apply_persisted_tool_watchdog_settings` **before**
  `spawn_tool_watchdog_supervisor`
- In-memory registry starts empty; old `in_progress` rows are not rehydrated
  here (existing boot reconciliation owns that)
- Supervisor uses coalescing `Notify` wake + 1s bounded periodic scan;
  deadlines remain timestamp-based

### Metrics (secret-safe)

`acp/tool_watchdog/metrics.rs` counters:

- warning episodes, extensions, automatic timeouts, user stops
- specific-cancel success, turn fallback, disconnect fallback,
  cancellation failure

Labels limited to agent type + coarse tool category (`terminal` /
`delegation` / `mcp` / `other`). Snapshots exclude raw input, tool_call_id,
tokens, cancel handles.

## Files

| File | Change |
| --- | --- |
| `commands/tool_watchdog.rs` | **Create** — load/set/extend/cancel cores + Tauri commands + tests |
| `web/handlers/tool_watchdog.rs` | **Create** — Axum mirrors + auth/parity tests |
| `acp/tool_watchdog/metrics.rs` | **Create** — labeled counters |
| `commands/mod.rs`, `web/handlers/mod.rs`, `web/router.rs` | Register modules/routes |
| `lib.rs`, `bin/codeg_server.rs` | Startup settings load + command registration |
| `app_state.rs` | Coalescing wake + periodic scan supervisor |
| `acp/manager.rs` | Metrics/wake fields; extend/user_cancel; scan metrics |
| `acp/tool_watchdog/{mod,registry,supervisor,types}.rs` | Exports, helpers, clippy |
| `keyring_store.rs` | Server-only clippy `question_mark` fix (unblock brief) |

## Test summary

```powershell
cargo test --lib --features test-utils commands::tool_watchdog
cargo test --no-default-features --lib tool_watchdog
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

| Suite | Result |
| --- | --- |
| `commands::tool_watchdog` (desktop features) | **5 passed** |
| `tool_watchdog` filter (server / no-default-features) | **96 passed** (includes commands, handlers, registry, supervisor, metrics) |
| clippy desktop `-D warnings` | **clean** |
| clippy server `-D warnings` | **clean** |

Covered assertions:

- missing/malformed → defaults
- 59→60, 3601→3600 clamp
- live registry updates only after successful persist
- startup registry empty + `apply_persisted_*` reloads
- stale extend/cancel without mutation
- extend/cancel body shape (`lease_id`, `version` only)
- desktop/server share cores (Axum handlers call same cores)
- metrics secret scan

## Concerns / follow-ups

1. **Frontend settings UI + banner actions** are Task 9/10 — APIs are ready.
2. **cancellation_failure** counter is defined and testable but not yet wired
   to a production failure path beyond API surface (escalation records
   specific/turn/disconnect stages).
3. **User cancel escalation is fire-and-forget** — API returns Cancelling
   immediately; full convergence is async (matches scan concurrency model).
4. Left unstaged (out of scope): `.superpowers/sdd/task-6-report.md` and
   design doc local edits.

## Self-review

### Spec compliance
- Exact app_metadata keys
- Exact operation / core names
- Stale code without mutation
- Live apply after persist only
- Startup load before single supervisor
- Coalescing wake + bounded scan
- Metrics labels agent + coarse category only
- Desktop/server same cores

### Quality
- Shared `_core` path for Tauri and Axum
- Clippy clean under `-D warnings` for desktop and server
- Focused tests for clamp, persist ordering, stale CAS, wire shapes

---

**Report path:** `.superpowers/sdd/task-7-report.md`

---

# Task 7 Review Fix Report (P1×4 + P2)

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Base:** `27500da5` — `feat(acp): expose tool watchdog settings and controls`  
**Review:** `.superpowers/sdd/task-7-review.md`  
**Date:** 2026-07-23  
**Worktree:** `D:\MyCodeBuddy\.worktrees\tool-execution-watchdog`

## Summary

Closes Task 7 review FAIL findings: scan no longer awaits escalations, user
cancel returns the atomic claim projection, cancellation_failure is wired from
real escalate outcomes, settings write+apply is serialized with consistent
three-key load, and failed-persist leaves live settings unchanged.

## Fixes

| Severity | Finding | Fix |
| --- | --- | --- |
| P1-1 | Periodic scanner blocked by `join_all` of escalations | `scan_and_execute_cancellations` spawns each escalation independently; report counts `escalations_spawned`; scan stays 1s-responsive |
| P1-2 | Successful cancel can return stale after live re-lookup | `claim_cancel` returns `(CancellationClaim, Cancelling projection)` under one lock; `tool_watchdog_user_cancel` never re-looks up `live_projection` |
| P1-3 | `cancellation_failure` never produced in production | `EscalationReport` preserves `specific_failed` / `turn_failed` / `disconnect_failed`; `record_escalation` increments failure counter from those outcomes |
| P1-4 | Concurrent settings saves diverge durable vs live | `tool_watchdog_settings_gate` serializes persist+apply; three keys load via one `get_values_conn` snapshot query |
| P2 | Failed-persist path untested | `failed_persist_leaves_live_settings_unchanged` forces bare SQLite upsert failure and asserts live unchanged |

## Files

| File | Change |
| --- | --- |
| `acp/manager.rs` | Spawn escalations; settings gate; user_cancel atomic projection; race + failure metric tests |
| `acp/tool_watchdog/supervisor.rs` | Operation outcome fields on `EscalationReport` |
| `acp/tool_watchdog/metrics.rs` | `record_escalation(&report)` wires `cancellation_failure` |
| `acp/tool_watchdog/registry.rs` | Atomic claim projection return |
| `app_state.rs` | Supervisor log uses `escalations_spawned` |
| `commands/tool_watchdog.rs` | Gate on set; consistent load; failed-persist + concurrent-save tests |
| `db/service/app_metadata_service.rs` | `get_values_conn` multi-key snapshot |

## Regressions

| Finding | Test |
| --- | --- |
| P1-1 | `scan_and_execute_cancellations_runs_escalations_concurrently` (scan returns before convergence budget) |
| P1-1 / C1 | `scan_and_execute_advances_warning_to_grace_then_claim_cancel` |
| P1-2 | `user_cancel_returns_claim_projection_when_complete_races` |
| P1-2 | registry claim projection survives settle / live miss |
| P1-3 | `escalate_records_cancellation_failure_from_host_outcomes` |
| P1-3 | metrics unit test counts failures from report outcomes |
| P1-4 | `concurrent_settings_saves_keep_live_and_durable_aligned` |
| P2 | `failed_persist_leaves_live_settings_unchanged` |

## Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils commands::tool_watchdog
cargo test --lib --features test-utils tool_watchdog
cargo test --lib --features test-utils scan_and_execute
cargo test --lib --features test-utils user_cancel_returns_claim
cargo test --lib --features test-utils escalate_records_cancellation
cargo test --no-default-features --lib tool_watchdog
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

| Check | Result |
| --- | --- |
| `commands::tool_watchdog` | **7 passed** |
| `tool_watchdog` (desktop features) | **98 passed** |
| `tool_watchdog` (server / no-default-features) | **98 passed** |
| `scan_and_execute*` | **2 passed** |
| `user_cancel_returns_claim_projection_when_complete_races` | **passed** |
| `escalate_records_cancellation_failure_from_host_outcomes` | **passed** |
| clippy desktop `-D warnings` | **clean** |
| clippy server `-D warnings` | **clean** |

Design doc under `docs/superpowers/specs/` and pre-existing
`.superpowers/sdd/task-6-report.md` edits left unstaged (out of fix scope).

---

# Task 7 Review Fix Report (r2 → I1)

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Review:** `.superpowers/sdd/task-7-review-r2.md` (FAIL — 1 Important I1)  
**Base:** `1d9f09e1` — `fix(acp): resolve Task 7 review P1s for watchdog scan and settings`  
**Date:** 2026-07-23  
**Worktree:** `D:\MyCodeBuddy\.worktrees\tool-execution-watchdog`

## Summary

Closes Task 7 r2 I1: control-lane `CancelTurn` (and disconnect stage) admission
is now bounded so a saturated/stalled control receiver cannot hang a claimed
escalation forever. On admit timeout the turn stage is marked failed, escalation
continues to disconnect/settlement, and `cancellation_failure` advances.

## Root cause

`cancel_turn_if_current` awaited unbounded `control_tx.send(CancelTurn)`.
`LaneSender::send` is a plain MPSC await; with a full lane and stalled
receiver the background escalation never reached the convergence timer,
disconnect fallback, settlement, or failure metric. The lease stayed
`Cancelling` and later scans could not reclaim it.

## Fixes

| Severity | Finding | Fix |
| --- | --- | --- |
| Important | I1 CancelTurn (and other stage) control admit unbounded | `CONTROL_LANE_ADMIT_TIMEOUT` (200ms); `cancel_turn_if_current` wraps send in `tokio::time::timeout` → `Err` on timeout/closed; `disconnect` bounds Disconnect control send the same way (leases/map already cleared) |

## Files

| File | Change |
| --- | --- |
| `acp/tool_watchdog/supervisor.rs` | Introduce `CONTROL_LANE_ADMIT_TIMEOUT`; `TERMINAL_ADMIT_TIMEOUT` aliases it |
| `acp/tool_watchdog/mod.rs` | Re-export `CONTROL_LANE_ADMIT_TIMEOUT` |
| `acp/manager.rs` | Bound CancelTurn + Disconnect control-lane admit; saturated-lane regression |

## Regressions

| Finding | Test |
| --- | --- |
| I1 | `saturated_turn_control_lane_escalation_terminates_with_failure_metric` (manager) |

Assertions: outer timeout proves termination; `turn_failed`; stage
`Disconnect`; lease not live; `cancellation_failure_total` +1.

## Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils saturated_turn_control_lane
cargo test --lib --features test-utils tool_watchdog
cargo test --lib --features test-utils escalate_records_cancellation
cargo test --lib --features test-utils production_cancel_host_wait
cargo test --lib --features test-utils scan_and_execute
cargo test --no-default-features --lib tool_watchdog
cargo test --no-default-features --lib saturated_turn_control_lane
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

| Check | Result |
| --- | --- |
| `saturated_turn_control_lane_escalation_terminates_with_failure_metric` | **passed** (desktop + server) |
| `tool_watchdog` (desktop features) | **98 passed** |
| `tool_watchdog` (server / no-default-features) | **98 passed** |
| `escalate_records_cancellation_failure_from_host_outcomes` | **passed** |
| `production_cancel_host_wait_uses_full_stamp_and_cause` | **passed** |
| `scan_and_execute*` | **2 passed** |
| clippy desktop `-D warnings` | **clean** |
| clippy server `-D warnings` | **clean** |

Design doc under `docs/superpowers/specs/` and pre-existing
`.superpowers/sdd/task-6-report.md` edits left unstaged (out of fix scope).

