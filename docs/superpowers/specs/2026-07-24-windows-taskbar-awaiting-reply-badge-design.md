# Windows Taskbar Awaiting-Reply Badge

**Date:** 2026-07-24  
**Status:** Approved for planning  
**Scope:** Desktop (`tauri-runtime`) on Windows only

## Problem

When conversations enter the “awaiting user reply” state (sidebar red-dot), users who minimize or leave the app have no OS-level cue of how many sessions still need attention. We want the **main window taskbar button** to show the total count of those sessions.

## Goals

- Show a numeric badge on the Windows taskbar icon for the main window (`"main"`).
- Count uses the same **business** definition as the red-dot (not the UI “selected hides dot” rule).
- Backend is the source of truth; no frontend store dependency.
- Zero impact on server builds and non-Windows desktops (compile-time no-op).

## Non-goals

- macOS Dock badge, Linux launcher count, or system-tray badge.
- Counting pending permissions or other attention types.
- Including remote workspaces.
- User preference toggle (always on in v1).
- Taskbar flash / extra system notifications.
- Per-window badges on detached conversation windows.

## Counting rules

A conversation is counted when **all** of the following hold in the local SQLite DB:

1. `deleted_at IS NULL`
2. `status = pending_review`
3. `awaiting_reply_token IS NOT NULL`

Aligned with sidebar red-dot **eligibility** (`pending_review` + non-null token), with one intentional difference:

| Case | Sidebar red-dot | Taskbar count |
|------|-----------------|---------------|
| Selected conversation, token not yet CAS-cleared | Hidden (`!isSelected`) | **Included** |
| Token cleared after focus/view | Hidden | Not included |

Sub-sessions and background roots normally never receive a token (`parent_id IS NULL` and `mark_awaiting_reply` gate minting), so they do not inflate the count.

**Workspace scope:** all local DB conversations (all folders). Remote workspaces are out of scope.

## Architecture

```
Durable write that can change the awaiting-reply set
        │
        ▼
awaiting_reply_badge::schedule_*  (desktop Windows only; elsewhere compile-time no-op)
        │  detached spawn; never awaited by business paths
        ▼
sync_once (serialized by async Mutex)
  1. COUNT(*) with the rules above
  2. If count == last_successfully_applied (or uninitialized sentinel differs):
     render 16×16 overlay (or clear) → set_overlay_icon on main window
  3. Update last_successfully_applied ONLY after a confirmed successful apply
     (or confirmed successful clear for count 0)
```

### Module layout (Rust)

```
src-tauri/src/awaiting_reply_badge/
  mod.rs     // schedule_from_emitter / schedule_from_app, sync_once, gates, Mutex state
  count.rs   // pure COUNT query (unit-testable without Tauri)
  icon.rs    // pure 16×16 RGBA renderer (unit-testable without Tauri)
```

Feature / OS gates:

```rust
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
```

### Public scheduling facade (compile-time boundary)

Shared call sites (including `emit_conversation_state`, which compiles for server)
must **not** import Tauri types. The facade always exists as thin functions:

| API | Behavior on Windows + `tauri-runtime` | Elsewhere |
|-----|----------------------------------------|-----------|
| `schedule_from_emitter(emitter: &EventEmitter)` | If `EventEmitter::Tauri(app)`, clone `AppHandle`, resolve `AppDatabase` via `app.try_state`, spawn detached `sync_once`. Other emitter variants → no-op. | `#[inline] fn` empty body (always compiled) |
| `schedule_from_app(app: &AppHandle)` | Same: clone handle + try_state DB + detached spawn. Used by lifecycle no-emitter paths and startup. | Not compiled / not called (`#[cfg(...)]` at call sites) |

Rules:

1. **Never await** badge work from conversation, lifecycle, or ACP paths.
2. **Never return** badge errors to callers; log only inside the module.
3. Schedule is based on a **durable state transition having been attempted/committed** or a State-emission **invocation**, not on webview `app.emit` success (`emit_event` already discards emit errors).
4. Acquiring `AppDatabase` fails (missing state) → `warn` + skip (no panic).

`lib.rs` registers `mod awaiting_reply_badge` unconditionally; non-Windows / non-desktop bodies are empty stubs so `emit_conversation_state` can always call `schedule_from_emitter` without `cfg` at every call site.

### Window target

- Only `app.get_webview_window("main")` (matches existing `WebviewWindowBuilder::new(app, "main", …)`).
- Use Tauri 2 `WebviewWindow::set_overlay_icon(Option<Image>)`.
- If the window is missing or already closed: silent skip; **do not** update last_successfully_applied (so a later sync after window exists can apply).

### Apply path / main-thread hop

The repository has no shared main-thread helper today. Implementation **must** use Tauri’s `AppHandle::run_on_main_thread` (or equivalent `WebviewWindow::run_on_main_thread`) to run `set_overlay_icon`.

Result handoff:

1. Outer async task holds the apply mutex.
2. It enqueues work on the main thread with a oneshot (or shared `Result` cell) for the **inner** `set_overlay_icon` result.
3. If `run_on_main_thread` itself fails to enqueue → treat as apply failure (warn; keep cache).
4. If enqueue succeeds but inner setter fails → treat as apply failure (warn; keep cache).
5. Only when both enqueue and setter succeed → update last_successfully_applied.

## Display rules

| Count | Overlay |
|------:|---------|
| `0` | `set_overlay_icon(None)` |
| `1`–`9` | Red rounded badge + white digit |
| `≥10` | Red badge + white `9+` |

Icon specs:

- 16×16 RGBA
- Background ~`#EF4444` (destructive-adjacent red)
- Embedded 5×7 bitmap digits (`0–9`, `+`) — no system fonts
- Built with existing `image` crate → `tauri::image::Image::new_owned` (same pattern as tray template icon load)

## Refresh funnel

### Primary hook

At the end of `emit_conversation_state` (shared helper), call
`awaiting_reply_badge::schedule_from_emitter(emitter)`.

This is **not** “after successful webview delivery”; it is “after the State-emission function is invoked following a durable patch.” Badge schedule is fire-and-forget and independent of `app.emit` result.

Covers the common paths that already emit State:

- End-turn → `pending_review` + token mint **when a live connection emitter exists**
- `clear_awaiting_reply` when `changed=true`
- Other status/token transitions that already call `emit_conversation_state`

### Mandatory secondary hooks (named; not open audit)

These durable writes change the awaiting-reply **set** without going through
`emit_conversation_state`. Each **must** schedule a badge sync after successful
commit (desktop Windows only):

| Path | Why State is missing | Hook |
|------|----------------------|------|
| Soft-delete orchestrator | `delete_conversation_with_cleanup_core` soft-deletes then emits **Deleted** only | After successful soft-delete + before/after Deleted emit is fine; prefer **after commit of soft_delete**, call `schedule_from_emitter(emitter)` when emitter is Tauri, or `schedule_from_app` on the desktop command path |
| Public status mutation (token-clearing) | `update_status` clears `awaiting_reply_token`, then callers emit **Upsert** only (not State). Applies to both the Tauri command and the shared HTTP handler (`POST /update_conversation_status`). Desktop may run an **embedded** web server with `EventEmitter::Tauri`, so the HTTP path is not “server-only.” | After successful `update_conversation_status_core`, schedule badge from the **caller that has an emitter/app**: (1) Tauri command → `schedule_from_app(&app)` (or `schedule_from_emitter` after building `EventEmitter::Tauri`); (2) HTTP handler → `schedule_from_emitter(&state.emitter)` (no-op for WebOnly / non-Windows). Shared **core** stays free of badge imports; both transport wrappers hook after commit. |
| Lifecycle end-turn, no live emitter | `handle_turn_complete_internal` can CAS mint token then skip State when `live` is `None` | After successful `finish_end_turn_if_in_progress` when `live` is missing, call `schedule_from_app` using a process-level AppHandle source (see below) |
| Lifecycle orphan reconcile, connection gone | Re-applies end-turn CAS, logs only, no State emit | After successful re-CAS when emitter missing, same `schedule_from_app` |

**AppHandle without EventEmitter (lifecycle):** Lifecycle tasks have `ConnectionManager` / DB but may lack a live per-connection emitter. On desktop Windows setup, store a clone of `AppHandle` in a process-local once-cell or managed state that lifecycle can resolve (e.g. `awaiting_reply_badge::set_app_handle` during setup, `try_app_handle()` for schedule). If unset (tests / server), secondary schedule is a no-op. Do **not** block lifecycle on badge work.

Do not double-call when State emit already covers the same transition (harmless under the mutex + equal-count skip, but prefer one schedule).

### Startup

After the main window is created in desktop setup (`lib.rs`), call
`schedule_from_app(app.handle())` once so cold-start pending sessions show a
badge without waiting for a new event. Register the process-level AppHandle
for lifecycle secondary hooks in the same setup path (before or with this call).

### Non-triggers (correct)

- Stale `clear_awaiting_reply` (`changed=false`, no State emit)
- Title / pin / model-only Upserts
- Server binary, non-Windows desktop, web-only runtime
- Standalone server HTTP status path is a **compile-time no-op** for badge (facade stub), not an intentional skip of a needed desktop update

## Concurrency and performance

- No polling.
- **Serialize** the full `COUNT → compare → render → apply → cache` sequence with a module-level `tokio::sync::Mutex` (or `async_lock`). Concurrent `schedule_*` spawns may pile up; each run re-reads COUNT after acquiring the mutex so the last run converges to the latest DB state.
- `last_successfully_applied: AtomicU32` (or field behind the same mutex) caches the last **successfully applied** count. Sentinel `u32::MAX` = never successfully applied.
- Equal to last successful → skip image work and skip Win32 call.
- COUNT is a single aggregate over conversation rows with selective filters; expected volume is desktop-scale (hundreds–low thousands).
- 16×16 bitmap draw runs only when the count **differs** from last successful apply.
- Optional future: coalesce multi-settle bursts with a short debounce; **not required for v1**.
- Stale races without a mutex are **out of policy**; v1 requires the mutex (or an equivalent generation fence that drops stale applies).

## Error handling

| Failure | Behavior |
|---------|----------|
| COUNT fails | `tracing::warn`; leave overlay and cache unchanged |
| No main window | Silent skip; leave cache unchanged |
| `run_on_main_thread` enqueue fails | `warn`; leave cache unchanged |
| `set_overlay_icon` fails | `warn`; leave cache unchanged |
| `AppDatabase` / AppHandle missing | `warn` or silent; skip |

Never propagate badge errors into conversation or ACP control flow.

## Frontend

**No frontend changes in v1.** Sidebar red-dot and `ConversationAwaitingReplyClearer` stay as-is. Brief divergence when a row is selected but the token is not yet cleared is expected.

## Testing

### Unit

- `count_awaiting_reply`: 0 / 1 / N; cleared token excluded; `in_progress` excluded; soft-deleted excluded; child without token excluded; root with token included.
- `render_badge_icon`: for 1, 9, and 10 (`9+`):
  - output is 16×16 RGBA
  - **deterministic glyph distinction**: not all-zero alpha; pixel samples or checksums differ across `{1, 9, 10}` so identical opaque circles cannot pass
- Scheduler / apply state machine (with a test double for the setter):
  - last_successfully_applied updates **only** after successful setter
  - missing-window / setter failure leaves cache and allows a later retry to apply
  - overlapping schedules serialize and converge to the latest COUNT

### Hook completeness (unit or lightweight integration)

Prove schedule is invoked (or schedule stub is called) on:

1. `emit_conversation_state` path
2. soft-delete orchestrator success
3. desktop Tauri `update_conversation_status` success
4. HTTP status handler success with Tauri-backed emitter (embedded desktop web; may be a unit test that the handler invokes `schedule_from_emitter`)
5. lifecycle no-live-emitter end-turn success (focused unit test of the branch that calls `schedule_from_app`)

### Integration / manual (Windows)

1. Cold start with 2 awaiting sessions → badge `2`.
2. Another root turn completes with mint → `3`.
3. Focus/view one session until clearer CAS-clears → `2`.
4. Clear all → overlay removed.
5. Delete an awaiting session → count decreases without needing another State event.
6. Server / non-Windows builds compile; badge code gated / stubs no-op.

Do not assert real taskbar pixels in CI.

## Implementation sketch (for planning)

1. Add `awaiting_reply_badge` module: count + icon + stubs + Windows sync with mutex.
2. Implement bitmap icon renderer + glyph-distinguishing unit tests.
3. Implement `schedule_from_emitter` / `schedule_from_app` + apply result handoff + last_successfully_applied rules.
4. Hook primary funnel at `emit_conversation_state`.
5. Hook mandatory secondaries: soft-delete, Tauri + HTTP status wrappers, lifecycle no-emitter paths; register AppHandle at setup; startup schedule after main window build.
6. Unit tests for count, icon, apply state machine, hook callouts where practical.
7. Manual Windows verification checklist.

## Open decisions (resolved)

| Question | Decision |
|----------|----------|
| Surface | Main window taskbar button only |
| Count vs selected | Business red-dot eligibility; selection does not exclude |
| Platform | Windows only |
| Scope | All local DB conversations |
| Architecture | Backend COUNT + overlay (Approach 1) |
| Display | 1–9 digits; ≥10 as `9+` |
| Preference toggle | Deferred |
| Concurrency | Async mutex around full sync sequence; cache only after successful apply |
| Scheduler boundary | Always-present `schedule_from_*` stubs; Windows desktop implements; lifecycle uses process AppHandle |
| Secondary hooks | Soft-delete; Tauri **and** HTTP status Upsert paths; lifecycle no-emitter end-turn + orphan reconcile — mandatory |

## Design review history

- 2026-07-24: Codex design review `REQUEST_CHANGES` (5 Important) — funnel gaps, soft-delete, status Upsert, AtomicU32 races, compile-time facade underspecified. Spec revised.
- 2026-07-24: Codex re-review `REQUEST_CHANGES` (1 Important) — embedded desktop HTTP status path must also schedule. Spec revised above.
