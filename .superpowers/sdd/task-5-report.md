# Task 5 Report — Connection Incarnations and Register Foreground Work

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Commit:** `4c7b9eb9` — `feat(acp): register foreground tools with watchdog leases`  
**Date:** 2026-07-23

## Summary

Wired Task 4’s `ToolExecutionLeaseRegistry` into the live ACP connection path:

1. **Incarnation + shared registry** — `ConnectionManager` owns `Arc<ToolExecutionLeaseRegistry>` (shared via `clone_ref` / `with_spawn_handshake_timeout`). Every `AgentConnection` and `SessionState` is stamped with the same spawn-time UUID `connection_incarnation` (rebind does not change it; reconnect/replacement mints a new value).
2. **Attribution facade** — `tool_watchdog/attribution.rs` normalizes host-side register / progress / pause / handoff / clear. Lifecycle tests prove isolation across incarnation, parallel tools, terminal offsets, status duplicates, untracked agent content, pause, background handoff, and disconnect/turn clear.
3. **Authoritative progress sources** — tool_call / tool_call_update (before frontend emit), terminal `next_offset` / exit from the poller, agent message/thought content → untracked fallback only, permission/question pause+resume, verified `parent_tool_use_id → task_id` delegation activity (300s soft supervisor unchanged).
4. **Cleanup** — `complete_turn` before TurnComplete; connection cleanup guard calls `remove_connection` for the incarnation.

Full cancel supervisor remains Task 6.

## Files changed

| File | Change |
| --- | --- |
| `src-tauri/src/acp/tool_watchdog/attribution.rs` | **Create** — `LeaseAttribution` + 12 lifecycle/attribution tests |
| `src-tauri/src/acp/tool_watchdog/mod.rs` | Export attribution helpers |
| `src-tauri/src/acp/tool_watchdog/registry.rs` | `Debug` impl for registry (SessionState derive) |
| `src-tauri/src/acp/session_state.rs` | `connection_incarnation`, shared registry Arc, turn stamp helpers |
| `src-tauri/src/acp/manager.rs` | Own registry; stamp test/spawn constructors; question pause/resume |
| `src-tauri/src/acp/connection.rs` | Spawn incarnation; register/progress/complete; terminal offsets; cleanup |
| `src-tauri/src/acp/lifecycle.rs` | Synthetic connection fields |
| `src-tauri/src/acp/delegation/event_emitter.rs` | Verified child activity → parent tool lease |

## Behavior delivered

| Rule | Implementation |
| --- | --- |
| Manager owns registry; clone_ref shares | `ConnectionManager.tool_lease_registry` |
| Immutable incarnation at spawn | UUID at `spawn_agent_connection`; rebind leaves it |
| Prompt admission starts fallback clock | `tool_watchdog_start_turn` after `Prompting` |
| Exact tool id lease | `tool_watchdog_on_tool_event` on ToolCall/Update |
| Unambiguous terminal → Terminal capability | `bind_terminal_if_unambiguous`; multi-id stays Turn |
| Background handoff drops foreground | `meta_marks_background` → `background_handoff` |
| Agent content → untracked only | message/thought → `record_agent_activity` |
| Permission / question pause | permission emit + manager question paths |
| Delegation child activity | verified binding only in `emit_observation_changed` |
| Turn complete / disconnect clear | `complete_turn` / cleanup `remove_connection` |
| No raw tool input in projections | unchanged (Task 3/4 types) |
| 300s delegation soft timeout | supervisor `derive_observation` untouched |

## TDD evidence

| Requirement | Test |
| --- | --- |
| New incarnation cannot mutate old lease | `tool_watchdog_attribution_new_incarnation_cannot_mutate_old_lease` |
| Parallel tools renew only themselves | `tool_watchdog_attribution_parallel_tools_renew_only_themselves` |
| Terminal offsets renew across truncation | `tool_watchdog_attribution_terminal_offsets_renew_across_truncation` |
| Status-only duplicates do not renew | `tool_watchdog_attribution_status_only_duplicates_do_not_renew` |
| Generic content renews only untracked turn | `tool_watchdog_attribution_generic_content_renews_only_untracked_turn` |
| Permission/question pause | `tool_watchdog_attribution_permission_and_question_pause` |
| Background handoff removes foreground | `tool_watchdog_attribution_background_handoff_removes_foreground` |
| Turn complete / disconnect clear | `tool_watchdog_attribution_turn_complete_and_disconnect_clear_leases` |
| Ambiguous terminal stays Turn | `tool_watchdog_attribution_ambiguous_terminal_stays_turn_capability` |
| Delegation hits only parent tool | `tool_watchdog_attribution_delegation_child_activity_hits_parent_tool` |

## Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils tool_watchdog_attribution -- --nocapture
cargo test --lib --features test-utils terminal_output_delta
cargo test --lib --features test-utils delegation::supervisor
cargo clippy --lib --features test-utils -- -D warnings
```

| Check | Result |
| --- | --- |
| `tool_watchdog_attribution` | **12 passed** |
| `terminal_output_delta` | **0 matched** (no tests with that filter; no failures) |
| `delegation::supervisor` | **6 passed** |
| `tool_watchdog` (all modules) | **52 passed** |
| clippy `-D warnings` | **clean** |

## Concerns / follow-ups

1. **Task 6** owns cancel supervisor (warning publish, claim_cancel execution, CancelTerminal control, convergence). This task only registers / progresses / completes leases.
2. **Background handoff detection** currently keys off CodeBuddy `codebuddy.ai/isBackground` meta **and** Claude transcript `async_launched` / `backgroundTaskId` acks (see review fix below).
3. **`terminal_output_delta` filter** matches zero unit tests today; terminal progress is covered via attribution tests + existing `terminal_runtime` suite (not renamed).
4. **Design doc** under `docs/superpowers/specs/` had local edits left unstaged (out of brief scope `git add src-tauri/src/acp`).
5. Manager disconnect paths clear the incarnation immediately after map removal; Drop still clears registry-first then map as a backstop.

---

## Review fix (r1 → `89973361`)

**Review:** `.superpowers/sdd/task-5-review.md` (FAIL — 4 High, 2 Medium)  
**Commit:** `89973361` — `fix(acp): resolve Task 5 review findings for watchdog leases`

### Fixes

| Severity | Finding | Fix |
| --- | --- | --- |
| High | Terminal bind used pre-status stamp → stale CAS | `tool_watchdog_on_tool_event` binds with stamp after `record_status` (or register stamp when status is no-op) |
| High | Capability from per-frame ids; multi never downgrades; fallback unbound | `sync_terminal_association` from accumulated `TrackedTerminalToolCall` + fallback merge; multi → `Turn` |
| High | Claude `async_launched` / `backgroundTaskId` never handed off | `background_watch` queues exact `tool_use_id`; `run_watch` calls `background_handoff` |
| High | Suspension cleared gen without completing leases | `tool_watchdog_complete_turn` before `clear_suspended_turn` |
| Medium | Disconnect map drop before registry clear | Manager disconnect paths call `remove_connection` immediately after map remove; Drop clears registry then map |
| Medium | Tests only checked Running phase | Strengthened version/capability assertions; live order, singleton→multi, fallback, suspend, disconnect-scan tests |

### Files

- `src-tauri/src/acp/tool_watchdog/registry.rs` — `tool_stamp` / `lease_capability` / `lease_stamp`; idempotent same-capability bind
- `src-tauri/src/acp/tool_watchdog/attribution.rs` — `sync_terminal_association` + strengthened lifecycle tests (17)
- `src-tauri/src/acp/connection.rs` — stamp order; accumulated sync; suspension complete_turn; Drop order
- `src-tauri/src/acp/background_watch.rs` — Claude ack → foreground handoff queue
- `src-tauri/src/acp/manager.rs` — disconnect / idle / by-owner / all clear leases post-remove

### Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils tool_watchdog_attribution -- --nocapture
cargo test --lib --features test-utils tool_watchdog -- --nocapture
cargo test --lib --features test-utils delegation::supervisor -- --nocapture
cargo clippy --lib --features test-utils -- -D warnings
```

| Check | Result |
| --- | --- |
| `tool_watchdog_attribution` | **17 passed** |
| `tool_watchdog` (all modules) | **57 passed** |
| `delegation::supervisor` | **6 passed** |
| clippy `-D warnings` | **clean** |
| `background_acks_queue_exact_foreground_tool_use_ids` | **passed** |

---

## Review fix (r2 → `5f93151b`)

**Review:** `.superpowers/sdd/task-5-review-r2.md` (FAIL — 4 Important I1–I4)  
**Commit:** `5f93151b` — `fix(acp): close Task 5 r2 lease races for disconnect, bind, handoff, offsets`

### Fixes

| Severity | Finding | Fix |
| --- | --- | --- |
| Important | I1 Manager disconnect map-before-registry orphan window | All manager disconnect paths (`disconnect`, `disconnect_if_owner`, `take_connections_for_disconnect`, idle sweep) clear registry **before** map removal; re-CAS after clear; Drop remains registry-first |
| Important | I2 Per-frame `bind_terminal_if_unambiguous` races multi-terminal | Removed frame-only bind from `tool_watchdog_on_tool_event`; capability only via accumulated `sync_terminal_association` / `tool_watchdog_sync_tracked_terminals` |
| Important | I3 Delayed background handoff uses current turn | Snapshot originating turn stamp before bg-watch tick; `background_handoff` completes exact lease first and sets `verified_background` only on success |
| Important | I4 Multi-terminal max-offset misses peer advances | Per-terminal offset map in `ProgressFingerprint`; poller reports each terminal via `record_terminal_offset_for` |

### Files

- `src-tauri/src/acp/manager.rs` — registry-first disconnect paths + manager race test
- `src-tauri/src/acp/connection.rs` — no frame bind; per-terminal offset reporting
- `src-tauri/src/acp/background_watch.rs` — originating turn stamp before tick
- `src-tauri/src/acp/tool_watchdog/attribution.rs` — exact-lease handoff; per-terminal offset API; I2/I3/I4 regressions
- `src-tauri/src/acp/tool_watchdog/progress.rs` — `per_terminal_offsets` + multi-terminal renew test
- `src-tauri/src/acp/tool_watchdog/registry.rs` — `TerminalOffset { terminal_id_hash, next_offset }`

### Regressions

| Finding | Test |
| --- | --- |
| I1 | `disconnect_clears_registry_before_map_invisible_to_scan` (manager) |
| I2 | `tool_watchdog_attribution_no_frame_only_terminal_bind` |
| I3 | `tool_watchdog_attribution_delayed_handoff_does_not_touch_next_turn` |
| I4 | `multi_terminal_lower_offset_peer_still_renews` + `tool_watchdog_attribution_multi_terminal_peer_offset_renews` |

### Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils tool_watchdog_attribution -- --nocapture
cargo test --lib --features test-utils tool_watchdog -- --nocapture
cargo test --lib --features test-utils disconnect_clears_registry_before_map_invisible -- --nocapture
cargo clippy --lib --features test-utils -- -D warnings
```

| Check | Result |
| --- | --- |
| `tool_watchdog_attribution` | **20 passed** |
| `tool_watchdog` (all modules) | **61 passed** |
| `disconnect_clears_registry_before_map_invisible_to_scan` | **passed** |
| clippy `-D warnings` | **clean** |

Design doc under `docs/superpowers/specs/` left unstaged (pre-existing local edits, out of fix scope).

---

## Review fix (r3 → `f2d7650f`)

**Review:** `.superpowers/sdd/task-5-review-r3.md` (FAIL — 2 Important I1–I2)  
**Commit:** `f2d7650f` — `fix(acp): fence disconnect admission and sync multi-terminal capability before emit`

### Fixes

| Severity | Finding | Fix |
| --- | --- | --- |
| Important | I1 Registry clear without admission fence still allows late re-register | `ToolExecutionLeaseRegistry::fence_connection` marks `(connection_id, incarnation)` closed; `register_tool` / `start_turn` reject; `remove_connection` fences under the same lock; manager `clear_tool_leases` order is fence → clear before map remove / Disconnect |
| Important | I2 Multi-terminal association stays `Terminal(A)` across frontend await | After `track_terminal_tool_calls` / merge, call `tool_watchdog_sync_tracked_terminals` **before** any other await; after `tool_watchdog_on_tool_event` in `emit_conversation_update`, sync from accumulated tracked map **before** `emit_with_state` |

### Files

- `src-tauri/src/acp/tool_watchdog/registry.rs` — `IncarnationKey` / `fenced` set; `fence_connection` / `is_fenced`; admission checks; fence inside `remove_connection`
- `src-tauri/src/acp/tool_watchdog/attribution.rs` — `fence_connection` facade; I1/I2 regression tests; host multi-sync ordering test
- `src-tauri/src/acp/manager.rs` — `clear_tool_leases` fences then clears; manager late-register race test
- `src-tauri/src/acp/connection.rs` — pre-emit tracked sync; post-register sync before frontend emit

### Regressions

| Finding | Test |
| --- | --- |
| I1 | `fence_connection_rejects_register_and_start_turn_after_clear` |
| I1 | `fence_does_not_block_new_incarnation` |
| I1 | `tool_watchdog_attribution_fence_blocks_late_register` |
| I1 | `disconnect_fences_admission_before_late_tool_reregister` (manager) |
| I2 | `tool_watchdog_attribution_no_frame_only_terminal_bind` (requires immediate multi sync) |
| I2 | `tool_watchdog_attribution_multi_association_claim_never_sees_terminal` |

### Verification

```powershell
cd D:\MyCodeBuddy\.worktrees\tool-execution-watchdog\src-tauri
cargo test --lib --features test-utils tool_watchdog -- --nocapture
cargo test --lib --features test-utils disconnect_ -- --nocapture
cargo test --lib --features test-utils delegation::supervisor -- --nocapture
cargo clippy --lib --features test-utils -- -D warnings
```

| Check | Result |
| --- | --- |
| `tool_watchdog` (all modules) | **65 passed** |
| `disconnect_*` filter | **13 passed** |
| `delegation::supervisor` | **6 passed** |
| `disconnect_fences_admission_before_late_tool_reregister` | **passed** |
| `tool_watchdog_attribution_multi_association_claim_never_sees_terminal` | **passed** |
| clippy `-D warnings` | **clean** |

Design doc under `docs/superpowers/specs/` left unstaged (pre-existing local edits, out of fix scope).
