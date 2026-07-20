# Conversation Pop-out Window (OS-level)

Date: 2026-07-20

Status: Design approved in brainstorming; awaiting implementation plan

## Summary

Add the ability to **pop a conversation out of the main workspace into a
real desktop `WebviewWindow`**, so Windows snap / multi-monitor layout can
manage each session independently.

v1 is **menu-driven** (tab bar + sidebar context menu). Drag-out-to-detach is
explicitly deferred. Restart does **not** restore detached windows.

This is a **desktop (Tauri) only** feature. Web / server browser mode does not
get OS windows; the menu entry is hidden (or no-ops with a short explanation).

## Problem

Current **tile mode** (`isTileMode`) only lays out multiple conversation panes
inside a **single** webview: horizontal flex, `min-w-[24rem]`, scroll. OS window
management applies to the whole Codeg window only, so multi-agent monitoring on
multiple monitors remains awkward.

## Goals

- Pop a conversation into an independent top-level OS window (true HWND).
- Full single-session chat UX in that window (messages, composer, permission /
  question dialogs, **sub-agent overlay**, **agent plan overlay**).
- Entry points: **tab context menu** and **sidebar conversation context menu**.
- Pop-out **moves** the session out of the main window tab strip (no mirror).
- Refuse pop-out when the main window would be left with **zero** open tabs
  (including the “only a new draft tab” case).
- After pop-out, main window activates the **most recently used** remaining tab.
- Closing the detached window **does not re-dock**; session leaves the open-tab
  set and remains available from the sidebar.
- Sidebar left-click on an already-detached conversation **focuses** that window
  (no second instance).
- No restart restore of detached windows in v1.

## Non-goals (v1)

- Drag tab/pane outside the main window to detach (can reuse the same API later).
- Persisting / restoring detached windows across app restarts.
- Mirroring the same conversation in main + detached window simultaneously.
- Popping out aux panels (file tree, git, terminal) or whole workspaces.
- FancyZones-specific integration (OS snap is enough).
- Server/web multi-window (browser tabs only if ever needed; not in v1).
- Changing in-app tile mode behavior beyond “detached sessions are not tiles”.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| What pops out | Conversation / chat panel only |
| Trigger (v1) | Context menu only (tab + sidebar) |
| Detach semantics | **Move** out of main tabs (not mirror) |
| Last session | Cannot pop out if main would have fewer than 2 open tabs after remove |
| Main focus after pop | Switch to **MRU** remaining tab (last activated among remaining) |
| Close detached window | No re-dock; remove from open tabs; sidebar list unchanged |
| Sidebar left-click while detached | Focus existing window |
| Detached chrome | Minimal: title bar + full single-session UI |
| Overlays | **Include** `SubAgentOverlay` + `AgentPlanOverlay` |
| Sub-agent “view session” | Keep existing dialog; no nested OS window in v1 |
| Restart | **Do not** restore detached windows |
| Platform | Desktop Tauri only |
| Architecture | New route + `WebviewWindow` (same pattern as settings/commit) |

---

## Architecture

### Approach

**Independent route + new `WebviewWindow` per conversation** (same family as
`open_settings_window` / `open_commit_window`).

```
Main window (workspace)
  tab / sidebar menu → open_conversation_window(conversation_id, …)
       │
       ├─ remove tab from main opened-tabs (if present)
       ├─ activate MRU remaining tab
       └─ Rust: WebviewWindowBuilder → label conversation-{id}
              URL: conversation?conversationId=…&folderId=…&…

Detached window (static export page)
  AppTitleBar + ConversationTabView (or thin equivalent shell)
  Shares process: DB, ACP ConnectionManager, event bus
```

**Rejected alternatives**

| Approach | Why rejected |
| --- | --- |
| In-app floating panes | No OS snap / multi-monitor ownership |
| Mirror dual-mount | Contradicts move semantics; dual input/scroll complexity |

### Window identity

| Item | Rule |
| --- | --- |
| Label | `conversation-{conversationId}` (positive DB id). Draft / unbound tabs **cannot** pop out until bound to a real conversation id. |
| Focus existing | If label already exists → `unminimize` + `set_focus`; do not create a second window. |
| Parent | **No** `.parent(&main)` — independent top-level window (like settings), so it can move/minimize freely and participate in Windows snap alone. |
| Title | `{conversation title} · {agent}` (fallback untitled + id). Update on title change if cheap; else set at open. |
| Size | Default ~960×720; `min_inner_size` reasonable (e.g. 480×400). Center on open (or near main). No geometry persistence in v1. |
| Style | Same `apply_platform_window_style` / `post_window_setup` as other aux windows. |

### Routing (static export)

Next.js is `output: "export"` — **no dynamic segments**. Use a fixed page:

- Path: `src/app/conversation/page.tsx` (or equivalent static segment)
- Query: `conversationId`, `folderId`, optional `agentType`, remote context params consistent with other windows (`remote_connection_id` / remote window id if already used by commit/settings)

### Frontend process model

Each WebviewWindow loads its own React tree. Design constraints:

1. **Reuse** `ConversationTabView` (or extract a `ConversationSessionSurface` if the tab view is too coupled to main tab chrome) so overlays, composer, and ACP wiring stay one path.
2. Detached page mounts a **minimal provider set**: enough for session runtime, ACP events, i18n, theme, toaster, credentials if needed — **not** the full workspace sidebar/tab bar.
3. Main window keeps a small **in-memory registry** of detached conversation ids → window labels (and optional last-focused timestamps). Registry is **not** persisted across restarts.
4. Main `opened tabs` remain the source of truth for the main strip; detached sessions are **not** main tabs.

### Backend / ACP ownership

Connections already carry `owner_window_label`; main close runs
`disconnect_by_owner_window("main")`.

| Event | Ownership rule |
| --- | --- |
| Pop-out of a live session | Re-bind that connection’s `owner_window_label` to the detached window label **or** ensure the detached window is the sole UI owner and cleanup on its close targets only that session. |
| Close detached window | Disconnect ACP (and terminals) owned by that window label — same spirit as closing a main tab (`acpDisconnect`). Conversation row remains in DB. |
| Close main while detached windows live | Do **not** disconnect sessions owned by detached labels. Main hide-to-tray behavior unchanged. Quitting the app still tears everything down. |

If re-binding owner labels is invasive, v1 may instead: keep process-global connections and on detached close explicitly disconnect by conversation/session id (preferred cleanup key must be documented in the plan). **Must not** leave orphan agent processes after the only UI for that session is closed.

### Opened-tabs interaction

| Action | Main tabs | Detached registry |
| --- | --- | --- |
| Pop-out (was open tab) | Remove that tab; persist opened tabs as today | Add id |
| Pop-out (sidebar, not a tab) | Unchanged | Add id + open window |
| Close detached window | No re-add | Remove id |
| Open from sidebar (not detached) | Existing open-tab behavior | — |
| Open from sidebar (detached) | No new main tab | Focus window |
| Restart app | Hydrate main tabs only | Empty |

**Draft / provisional tabs** (`conversationId == null`): menu item disabled with reason (need a real session first).

**Tile mode:** Detached conversations are absent from the main tile row (they are not in `tabs`). No special tile API changes beyond natural list membership.

---

## UX

### Entry points

1. **Tab bar** — tab context menu item: “Pop out window” / 「弹出窗口」  
   (alongside existing tile / close items in `TabItem`).
2. **Sidebar** — conversation row context menu on `SidebarConversationCard`
   (already has rename/delete/status menus): same action.  
   Folder-header menu stays as-is; this is **per conversation**.

Both call one client helper, e.g. `popOutConversation(conversationId)`.

### Enablement rules

| Condition | Menu |
| --- | --- |
| Not desktop / not local multi-window capable | Hidden |
| Draft tab without `conversationId` | Disabled |
| Conversation already detached | Same menu label **「弹出窗口」**; action **focuses** the existing window (no second window) |
| Conversation is an open main tab **and** `tabs.length === 1` | Disabled (“Cannot pop out the last tab”) |
| Conversation is an open main tab **and** `tabs.length > 1` | Enabled → move + open |
| Conversation **not** in main tabs | Enabled → open detached only |

### After successful pop-out (from main tab)

1. Open or focus `conversation-{id}` window.
2. Remove tab from main `rawTabs` / persist.
3. Set `activeTabId` to MRU among remaining:
   - Prefer an explicit MRU stack if present; else last non-closed activation order; else nearest neighbor in previous order; else first remaining.
4. If main has zero tabs after a bug, existing “ensure new conversation tab” effect still applies — pop-out must not intentionally produce zero tabs.

### Close detached window

- User clicks OS/window close.
- Rust `on_window_event` CloseRequested for `conversation-*`: cleanup connections/terminals for that label; emit a frontend event (or rely on main polling window list) so main clears detached registry.
- **Do not** re-insert into main opened tabs.
- User can reopen later via sidebar → opens **in main** as a normal tab (unless they choose pop-out again).

### Sidebar left-click

| State | Behavior |
| --- | --- |
| Detached | Focus detached window; do not switch main to a new tab for that id |
| Not detached | Existing behavior (open/activate main tab) |

Optional: subtle indicator on sidebar row when detached (icon / badge). Nice-to-have; not required for MVP if focus behavior is correct.

### Window chrome (detached)

- App title bar + window controls (match other Codeg windows on Windows).
- Body: single conversation surface only.
- **No** folder sidebar, **no** multi-tab bar, **no** aux file/git panel.
- **Yes**: message list, composer, connection status, permission/question UI, plan + sub-agent overlays, export/actions already on the conversation surface if they live inside `ConversationTabView`.

---

## API surface (sketch)

### Rust (tauri-runtime)

```text
open_conversation_window(
  conversation_id: i32,
  folder_id: i32,
  agent_type: Option<String>,
  locale: Option<AppLocale>,
  remote_connection_id: Option<i32>,
) -> Result<(), AppCommandError>
```

- Idempotent focus if window exists.
- Builds URL with query params; applies platform style; focus.

Optional companion:

```text
focus_conversation_window(conversation_id: i32) -> Result<bool, …>
// true if focused, false if no such window
```

Close is OS-driven; cleanup in `on_window_event` by label prefix `conversation-`.

### Frontend

```text
popOutConversation(conversationId: number): Promise<void>
// orchestrates: prechecks → open_conversation_window → close main tab if needed → MRU switch → registry

isConversationDetached(conversationId: number): boolean
// main-window memory only

focusDetachedConversation(conversationId: number): Promise<boolean>
```

Wire through `lib/api.ts` / transport like other window opens (`getShellTransport` / desktop-only).

### Events

- Prefer: main listens for window destroyed / custom `conversation-window://closed` with `{ conversationId }` to drop registry entry.
- Title updates: optional later.

---

## i18n

Add keys (all 10 locales) under something like `Folder.tabs` / `Folder.sidebar` / `ConversationPopout`:

- `popOutWindow` — menu label (also used when already detached; action focuses)  
- `cannotPopOutLastTab` — disabled reason or toast  
- `cannotPopOutDraft` — draft without id  
- `popOutDesktopOnly` — if ever shown on web  

Follow existing next-intl message file layout.

---

## Error handling

| Failure | Behavior |
| --- | --- |
| Window build fails | Toast; do **not** remove main tab |
| Conversation missing in DB | Toast; no window |
| Race: last tab closed elsewhere during pop | Re-check count before remove; abort with toast |
| Focus of missing window | Fall through to open or normal sidebar open |
| ACP rebind fails | Prefer abort pop-out rather than orphan UI without session; log + toast |

Ordering must be **open window success → then remove main tab** (or transactional compensate: if remove fails after open, keep window and registry consistent).

---

## Testing

### Unit / component

- Enablement matrix (last tab, draft, already detached, not open).
- MRU selection after remove.
- Sidebar click routes to focus when registry has id.
- Menu items render desktop-only.

### Integration / Rust (as feasible)

- `open_conversation_window` idempotent focus.
- Close path cleans owner resources for `conversation-*` labels without touching main-owned sessions.

### Manual (Windows)

- Pop two sessions to two monitors; Win+Arrow snap each.
- Pop from tab and from sidebar.
- Close detached → sidebar still lists; reopen in main.
- Only one tab → menu disabled.
- Main hide-to-tray while detached still running; quit app cleans all.

---

## Implementation phases

### Phase 1 — MVP (this design)

1. Rust `open_conversation_window` + close cleanup.  
2. Static `conversation` page + minimal providers + session surface.  
3. `popOutConversation` orchestration + detached registry.  
4. Tab + sidebar context menus.  
5. Sidebar left-click focus.  
6. i18n + desktop gating.  
7. Tests for enablement / MRU / focus routing.

### Phase 2 — Later (out of scope)

- Drag-out detach reusing the same API.  
- Restart restore of detached set + geometry.  
- Sidebar detached badge.  
- Live title sync / badge for agent status on taskbar.

---

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Memory: N webviews | Accept for small N; document; no restore spam on boot |
| Provider tree incomplete on detached page | Checklist against ConversationTabView dependencies; smoke test send + stream + overlays |
| ACP owner disconnect kills wrong session | Explicit ownership rules + tests for multi-window close |
| Tab remove before window open | Strict ordering + compensate |
| Static export query-only routing | Follow commit/settings window pattern |

---

## Success criteria

1. User can pop a non-last conversation from tab or sidebar menu into a real OS window.  
2. Windows snap / independent move works per conversation window.  
3. Plan + sub-agent overlays appear when the session has that data.  
4. Last main tab cannot be popped; MRU tab activates after pop.  
5. Close detached does not re-dock; sidebar can reopen in main.  
6. Second activation focuses the existing window.  
7. App restart does not recreate detached windows.  
8. Web build does not expose a broken control.

---

## Open implementation notes (not product open questions)

These are plan-time choices, not unresolved product decisions:

- Exact MRU data structure (ring buffer in tab-store vs `lastActivatedAt` on tabs).  
- Whether to extract `ConversationSessionSurface` vs reuse `ConversationTabView` with props.  
- ACP owner rebind API vs disconnect-by-conversation-id on window close.  
- Precise default window size and whether to open centered vs offset from main.

---

## References

- Tile mode: `src/stores/tab-store.ts` (`isTileMode`), `conversation-detail-panel.tsx` (`canTile`)  
- Tab menu: `src/components/tabs/tab-item.tsx`  
- Multi-window: `src-tauri/src/commands/windows.rs` (`open_settings_window`, etc.)  
- Overlays: `src/components/chat/sub-agent-overlay.tsx`, `agent-plan-overlay.tsx`  
- ACP ownership: `owner_window_label`, `disconnect_by_owner_window` in `src-tauri/src/acp/manager.rs`  
)
