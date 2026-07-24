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
DB write that can change the awaiting-reply set
        │
        ▼
awaiting_reply_badge::sync(app, db)
  1. COUNT(*) with the rules above
  2. Compare to AtomicU32 last_shown (MAX = uninitialized)
  3. On change: render 16×16 overlay icon (or clear) → main window set_overlay_icon
```

### Module layout (Rust)

```
src-tauri/src/awaiting_reply_badge/
  mod.rs     // sync, last_count, cfg gates, main-thread hop
  count.rs   // pure COUNT query (unit-testable)
  icon.rs    // pure 16×16 RGBA renderer (unit-testable)
```

Feature / OS gates:

```rust
#[cfg(all(feature = "tauri-runtime", target_os = "windows"))]
```

Elsewhere: `sync` is a no-op (or not compiled).

### Window target

- Only `app.get_webview_window("main")` (matches existing `WebviewWindowBuilder::new(app, "main", …)`).
- Use Tauri 2 `WebviewWindow::set_overlay_icon(Option<Image>)`.
- If the window is missing or already closed: silent skip.

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

After a successful `emit_conversation_state` (global `conversation://changed` State patch), schedule badge `sync`.

This covers the common paths:

- End-turn → `pending_review` + token mint
- `clear_awaiting_reply` when `changed=true`
- Other status/token transitions that already emit State

Badge failure must never fail the emit path or any business command.

### Secondary hooks (gap fill)

Any successful write that changes the awaiting-reply **set** but does **not** emit State must call `sync` explicitly. Implementation checklist: every writer of `awaiting_reply_token` / soft-delete of counted rows. Known candidates to verify at implement time:

- Soft-delete conversation
- Forced status writes that clear the token while only emitting Upsert

Do not double-call when State emit already covers the same transition (harmless if last_count short-circuits, but prefer one schedule).

### Startup

After the main window is created (desktop setup), run `sync` once so cold-start pending sessions show a badge without waiting for a new event.

### Non-triggers (correct)

- Stale `clear_awaiting_reply` (`changed=false`, no State emit)
- Title / pin / model-only Upserts
- Server binary, non-Windows desktop, web-only runtime

## Concurrency and performance

- No polling.
- `AtomicU32` caches last applied count; equal count → no image work, no Win32 call.
- COUNT is a single aggregate over conversation rows with selective filters; expected volume is desktop-scale (hundreds–low thousands).
- 16×16 bitmap draw runs only when the count **changes** (user-scale events: turn complete, view-to-clear).
- Optional future: coalesce multi-settle bursts with a short debounce; **not required for v1**.
- Invoke `set_overlay_icon` on the main/UI thread via the project’s existing main-thread hop if called from tokio workers.

## Error handling

| Failure | Behavior |
|---------|----------|
| COUNT fails | `tracing::warn`; leave overlay unchanged |
| No main window | Silent skip |
| `set_overlay_icon` fails | `warn`; **do not** update last_count so a later sync can retry |
| Main-thread hop fails | `warn`; same as above |

Never propagate badge errors into conversation or ACP control flow.

## Frontend

**No frontend changes in v1.** Sidebar red-dot and `ConversationAwaitingReplyClearer` stay as-is. Brief divergence when a row is selected but the token is not yet cleared is expected.

## Testing

### Unit

- `count_awaiting_reply`: 0 / 1 / N; cleared token excluded; `in_progress` excluded; soft-deleted excluded; child without token excluded; root with token included.
- `render_badge_icon`: for 1, 9, and 10 (`9+`), output is 16×16 non-fully-transparent RGBA.

### Integration / manual (Windows)

1. Cold start with 2 awaiting sessions → badge `2`.
2. Another root turn completes with mint → `3`.
3. Focus/view one session until clearer CAS-clears → `2`.
4. Clear all → overlay removed.
5. Server / non-Windows builds compile; badge code gated off.

Do not assert real taskbar pixels in CI.

## Implementation sketch (for planning)

1. Add `awaiting_reply_badge` module + COUNT service helper.
2. Implement bitmap icon renderer + unit tests.
3. Implement `sync` with last_count + Windows `set_overlay_icon`.
4. Hook primary funnel at `emit_conversation_state`; audit secondary writers.
5. Startup sync after main window creation.
6. Manual Windows verification checklist.

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
