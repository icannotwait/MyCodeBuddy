# Task 8 Report — Make Warning Projection Replayable and Flush-Sensitive

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Commit:** `973ffb4c` — `feat(acp): replay tool watchdog warning state`  
**Base:** `18538a5f` — `fix(acp): bound control-lane CancelTurn admit during escalation`  
**Date:** 2026-07-23  
**Worktree:** `D:\MyCodeBuddy\.worktrees\tool-execution-watchdog`

## Summary

Task 8 makes tool-watchdog warning state **replayable on attach** and
**flush-sensitive on the desktop batcher**, so concurrent Grace leases survive
cold attach and control transitions are not delayed behind content batches.

## What landed

### Actionable projection map (`SessionState` / `LiveSessionSnapshot`)

- New field `tool_watchdog_projections: BTreeMap<lease_id, ToolWatchdogProjection>`
- Capacity tracks live leases only (no soft eviction of Warning/Grace/Cancelling)
- Wire: `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]` so empty
  maps stay off the wire (byte-identical with pre-feature snapshots)

### `apply_event` semantics

| Phase | Effect |
| --- | --- |
| `warning` / `grace` / `cancelling` | Upsert per `lease_id` if `version >=` existing |
| `cleared` / `timed_out` | Remove key if `version >=` existing (stale clear/timeout cannot drop a newer sibling reopen; TimedOut is event-only, not a durable map entry) |

Older versions never replace newer projections. Warning→Grace keeps the final
Grace version so the first Stop/Extend click is not predictably stale.

### Desktop batch flush policy

`is_flush_sensitive` now returns true for `ToolWatchdogChanged` when phase is:

- `warning`
- `cancelling`
- `timed_out`
- `cleared`

`grace` remains non-sensitive (may batch with content). Countdown ticks stay
client-side and never enter the event stream.

### Event-size accounting

`estimate_envelope_size` sizes `ToolWatchdogChanged` structurally from
secret-safe projection fields only (`lease_id`, title, phase, timestamps,
optional scope/error). No raw input / provider `tool_call_id` path exists.

### Frontend wire / denormalize

- `LiveSessionSnapshot.tool_watchdog_projections?: Record<string, ToolWatchdogProjection>`
- `SnapshotPatch.toolWatchdogProjections` defaults to `{}` when omitted
- Context test mocks updated for the new patch field (Task 9 wires reducer/UI)

## Files

| File | Change |
| --- | --- |
| `src-tauri/src/acp/session_state.rs` | Map field, apply_event, snapshot, tests |
| `src-tauri/src/acp/desktop_event_batcher.rs` | Flush-sensitive policy + tests |
| `src-tauri/src/acp/event_stream.rs` | Structural size estimate + tests |
| `src-tauri/src/acp/tool_watchdog/types.rs` | `ToolWatchdogPhase: Copy` (unit enum) |
| `src/lib/types.ts` | Snapshot field |
| `src/lib/snapshot-denormalize.ts` | Denormalize map |
| `src/lib/snapshot-denormalize.test.ts` | Concurrent / absent / >32 tests |
| `src/contexts/acp-connections-context.test.tsx` | Mock patch field |

## Test summary

```powershell
cd src-tauri
cargo test --lib --features test-utils tool_watchdog_snapshot
cargo test --lib --features test-utils desktop_event_batcher
cd ..
pnpm test -- src/lib/snapshot-denormalize.test.ts src/contexts/acp-connections-context.test.tsx
```

| Suite | Result |
| --- | --- |
| `tool_watchdog_snapshot` | **6 passed** |
| `desktop_event_batcher` | **13 passed** (incl. flush + cleared-with-preceding) |
| FE snapshot-denormalize + acp-connections-context | **111 passed** (14 + 97) |

Covered assertions:

- Concurrent Grace leases round-trip on snapshot attach/replay
- Stale version cannot replace newer projection
- Warning then Grace stores actionable Grace version
- Per-lease clear leaves siblings intact; stale clear cannot drop newer reopen
- >32 concurrent Grace leases all survive (40)
- Cleared / warning / cancelling / timed_out flush with preceding events
- Grace is not flush-sensitive
- Event-size estimate never undercounts; serialized envelope has no `raw_input` / `tool_call_id`
- FE denormalize carries concurrent map; absent → `{}`; 40 leases preserved

## Out of scope (Task 9)

- ConnectionState reducer / live `tool_watchdog_changed` reduction
- Persistent banner UI, extend/cancel API UX, notifications

## Self-review

1. **Spec coverage:** lossless map, version CAS, flush-sensitive transitions,
   secret-safe size accounting, FE denormalize for attach.
2. **Placeholders:** none; Task 9 owns banner + event reduction.
3. **Unrelated dirty files left unstaged:** `.superpowers/sdd/task-6-report.md`,
   `docs/superpowers/specs/2026-07-22-tool-execution-watchdog-design.md`
   (pre-existing / non–Task-8 edits; not committed).

---

# Task 8 P1 Fix Report

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Review:** `.superpowers/sdd/task-8-review.md` (2 P1s)  
**Date:** 2026-07-23

## Summary

Closes both Task 8 P1 findings so attach/replay never keeps stale Grace or an
unbounded TimedOut ledger.

## Fixes

### P1-1 — Emit Cleared on normal complete and Grace→Running progress

| Path | Change |
| --- | --- |
| `complete_tool` host path | Emit `ToolWatchdogChanged` for **any** `Cleared` / `TimedOut` (removed `error_code.is_some()` gate that dropped normal completes). |
| Progress demotion | `renew_lease_to_running` returns `Cleared` when demoting Warning/Grace; `ToolProgressApply.cleared` surfaces it. |
| Connection wiring | Emit Cleared from status progress, terminal offset/exit, agent-activity fallback renew, and background handoff complete. |

### P1-2 — TimedOut is not a durable map entry

`SessionState::apply_event` actionable map:

| Phase | Effect |
| --- | --- |
| `warning` / `grace` / `cancelling` | Upsert if `version >=` existing |
| `cleared` / `timed_out` | Remove if `version >=` existing |

TimedOut remains a one-shot event for transcript/UI; attach snapshots only carry
live actionable leases.

## Tests added

- `tool_watchdog_snapshot_grace_then_progress_clear_is_empty`
- `tool_watchdog_snapshot_grace_then_complete_clear_is_empty`
- `tool_watchdog_snapshot_timed_out_does_not_accumulate`
- `progress_in_warning_and_grace_returns_to_running` (asserts Cleared projection)
- `progress_from_running_does_not_emit_cleared`
- `grace_progress_and_complete_clear_projections_for_replay_map`

## Verification

```powershell
cd src-tauri
cargo test --lib --features test-utils tool_watchdog_snapshot
cargo test --lib --features test-utils tool_watchdog
cargo test --lib --features test-utils desktop_event_batcher
```

| Suite | Result |
| --- | --- |
| `tool_watchdog_snapshot` | **9 passed** |
| `tool_watchdog` (filter) | **111 passed** |
| `desktop_event_batcher` | **13 passed** |

---

# Task 8 r2 P1 Fix Report

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Review:** `.superpowers/sdd/task-8-review-r2.md` (1 P1)  
**Date:** 2026-07-23

## Summary

Closes the r2 P1: every registry transition that demotes or retires an
actionable (Warning/Grace) lease now returns a `Cleared` projection, and every
production host path publishes `ToolWatchdogChanged { Cleared }` so
`SessionState` attach maps cannot retain stale Stop/Extend surfaces.

## Root cause

r1 fixed complete/progress paths on the connection wire, but other production
paths mutated the registry and discarded returned `Cleared` (or never produced
one). Reproducible stale-map case: delegated child activity demoted Grace →
Running while the attach map kept Grace.

## Fixes

| Path | Change |
| --- | --- |
| `record_delegation_activity` / child activity | `tool_watchdog_on_verified_child_activity` emits Cleared when progress demotes Grace |
| `apply_settings` disable | Returns `Vec<Cleared>`; settings command emits via `emit_tool_watchdog_clears` |
| `pause_turn` | Returns `Vec<Cleared>` for demoted Warning/Grace; permission / question / delegation-waiting hosts emit |
| `register_tool` retire fallback | `RegisterToolOutcome.cleared` for Grace/Warning/Cancelling fallback; connection tool events emit |
| Helpers | `emit_tool_watchdog_clear(s)` in connection + manager + delegation event_emitter |

## Tests added

- `delegation_activity_in_grace_returns_cleared`
- `apply_settings_disable_returns_cleared_for_grace`
- `pause_turn_returns_cleared_for_grace_leases`
- `register_tool_retires_grace_fallback_with_cleared`
- `tool_watchdog_attribution_child_activity_clears_grace_attach_map` (registry → Cleared → SessionState snapshot empty)

## Verification

```powershell
cd src-tauri
cargo test --lib --features test-utils tool_watchdog
```

| Suite | Result |
| --- | --- |
| `tool_watchdog` (filter) | **116 passed** |

## Unrelated dirty (left unstaged)

- `.superpowers/sdd/task-6-report.md`
- `docs/superpowers/specs/2026-07-22-tool-execution-watchdog-design.md`

---

# Task 8 r3 P1 Fix Report

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Review:** `.superpowers/sdd/task-8-review-r3.md` (1 P1)  
**Date:** 2026-07-23

## Summary

Closes the r3 P1: `ConnectionManagerParentLookup::bind_delegation_wait` now
publishes `ToolWatchdogChanged { Cleared }` when missing-stamp wait admission
retires a Grace/Warning fallback via `register_or_touch_tool`, so
`SessionState` attach maps cannot retain a Stop/Extend surface for a removed
fallback lease.

## Root cause

r2 routed clears for child activity, settings disable, pauses, and normal
connection tool registration. Multi-task wait admission still discarded
`RegisterToolOutcome::cleared` after `register_or_touch_tool` and kept only
`outcome.stamp`. That first tracked-tool admission permanently removes the
fallback, so a later attach could replay Grace for a lease that no longer
exists.

`emit_tool_watchdog_clears` cannot recover the connection after retirement
because it looks up `lease_stamp` on an already-removed fallback.

## Fix

| Path | Change |
| --- | --- |
| `ConnectionManagerParentLookup::bind_delegation_wait` | On missing stamp, if `register_or_touch_tool` returns `outcome.cleared`, emit via `manager.emit_tool_watchdog_changed(parent_connection_id, cleared)` **before** bind |

## Test added

- `bind_delegation_wait_retires_grace_fallback_clears_attach_map`  
  Seeds Grace fallback on the attach map, calls missing-stamp
  `bind_delegation_wait`, asserts snapshot map is empty and fallback lease is
  gone.

## Verification

```powershell
cd src-tauri
cargo test --lib --features test-utils bind_delegation_wait_retires_grace_fallback_clears_attach_map
cargo test --lib --features test-utils tool_watchdog
cargo test --lib --features test-utils register_tool_retires_grace
```

| Suite | Result |
| --- | --- |
| `bind_delegation_wait_retires_grace_fallback_clears_attach_map` | **1 passed** |
| `tool_watchdog` (filter) | **116 passed** |
| `register_tool_retires_grace_fallback_with_cleared` | **1 passed** |

## Unrelated dirty (left unstaged)

- `.superpowers/sdd/task-6-report.md`
- `docs/superpowers/specs/2026-07-22-tool-execution-watchdog-design.md`
