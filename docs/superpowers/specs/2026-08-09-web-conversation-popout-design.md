# Web conversation pop-out (lightweight multi-open)

**Date:** 2026-08-09  
**Status:** Approved for implementation planning  
**Related:** desktop pop-out (`2026-07-20-conversation-popout-window-design.md` and follow-ons)

## Problem

Desktop Codeg can pop a conversation into its own window via a full ownership
handoff (open → ready → rebind → release main tab → complete). The browser
**server** build currently hides or rejects pop-out:

- Menu gated on `isLocalDesktop()` (sidebar card, tab bar).
- `/conversation` hard-fails with `localDesktopOnly` when not local desktop.
- Web has no `open_conversation_window` HTTP path; shell APIs are Tauri-only.

Users want server/web behavior closer to **settings**: open the conversation in
a **new browser tab**, without removing the main workspace tab.

## Goals

1. On pure web (`!isDesktop()`), show **弹出窗口 / Pop out window** for real
   conversations (not drafts).
2. Click opens `/conversation?...&mode=web` via `window.open` (same gesture
   stack as settings’ `window.open`).
3. Main workspace **keeps** the conversation tab; no detach, no transfer fence.
4. New tab can stream and send: prefer attach to an existing server ACP
   connection (viewer/co-control); otherwise cold connect.
5. Local-desktop handoff path remains unchanged.
6. Unit tests cover enablement, URL build, query parse, and web bootstrap not
   calling rebind/ready/commit-ack.

## Non-goals

- Porting desktop handoff (`operationId`, rebind, commit-ack, reverse on close).
- Changing **remote-desktop** (Tauri + remote workspace) pop-out; remains off.
- Forcing exclusive write ownership between main and pop-out tabs.
- Auto-restoring a closed browser tab into the main workspace.
- Backend `open_conversation_window` web stub (path is built entirely on the
  client, like a pure navigation; settings keeps its path API for historical
  reasons—this feature does not need it).

## Product decisions

| Decision | Choice |
|----------|--------|
| Main tab after pop-out | **Keep** (settings-like multi-open) |
| Implementation style | Lightweight `window.open` + enable `/conversation` web path |
| Ownership protocol | **None** for web; reuse existing multi-client viewer model |
| Remote desktop | Still unsupported this iteration |

## Current architecture (relevant facts)

- **Settings (web):** `openSettingsWindow` → HTTP returns `{ path }` →
  `window.open(result.path, name)`.
- **Desktop pop-out:** complex orchestration in `src/lib/conversation-popout.ts`
  plus Rust `commands/conversation_popout.rs` and `/conversation` bootstrap
  (rebind, ready event, commit-ack).
- **Web ACP connect:** `web/handlers/acp.rs` always uses owner label `"web"`.
- **Multi-client:** `acp_find_connection_for_conversation` + frontend
  `isViewer` already support a non-owning co-controller that must not
  `acpDisconnect` the shared agent on teardown.
- **Detached UI shell:** `src/app/conversation/_components/detached-shell.tsx`
  seeds memory-only tab/folder state without `TabProvider` hydrate/save.

## Design

### Enablement

Extend `canPopOutConversation` (and menu visibility) so:

| Runtime | Show menu | Enable when |
|---------|-----------|-------------|
| Local desktop | Yes | Existing rules: not draft; not last open main tab |
| Pure web | Yes | Not draft only (`last_tab` does **not** apply) |
| Remote desktop | No | — |

Enablement reasons (explicit):

```ts
type PopOutDisableReason = "not_supported" | "draft" | "last_tab"
```

- `not_supported`: remote desktop (or any non–local-desktop, non–pure-web host).
- `draft`: missing/invalid conversation id.
- `last_tab`: **desktop only** when the conversation is an open main tab and
  `mainTabCount < 2`.
- Pure web never returns `last_tab` or `not_supported` for a normal browser
  session; only `draft` can disable.

Menu visibility: `showPopOut = isLocalDesktop() || !isDesktop()`  
(equivalent: local desktop **or** browser web; not remote desktop).

Note: existing code uses reason `not_local_desktop`. Implementation may rename
to `not_supported` or map both in UI copy; do not leave web classified as
`not_local_desktop` when it is actually supported.

### Orchestration branch

`popOutConversation`:

```
if pure web:
  url = buildWebConversationPopoutUrl({ conversationId, folderId, agentType })
  win = window.open(url, `conversation-${conversationId}`)
  if (!win) toast popup-blocked
  return  // no transfer fence, no openConversationWindow, no detach
else:
  existing desktop path
```

`buildWebConversationPopoutUrl` must produce a path that works with static
export base path conventions used elsewhere (relative `/conversation?...` like
settings paths).

Window **name** `conversation-{id}`: best-effort focus/reuse when the user
clicks pop-out again. Browsers differ; double-open is acceptable.

### URL contract

```
/conversation?conversationId={id}&folderId={fid}&agentType={agent}&mode=web
```

- `mode=web` is required for the lightweight bootstrap path.
- No `operationId` on web.
- Desktop continues to require `operationId` and must **not** send `mode=web`.

### Query parsing

Update `parseConversationPopoutQuery` (or add a sibling parser) to return a
discriminated result:

```ts
type ParsedConversationRoute =
  | { kind: "desktop"; conversationId; folderId; agentType; operationId }
  | { kind: "web"; conversationId; folderId; agentType }
```

Validation:

- Shared: positive conversationId/folderId, known agentType.
- Desktop: non-empty operationId; reject `mode=web` on local desktop open path
  if mixed (desktop builder never sets it).
- Web: `mode=web`; operationId optional/ignored.

### `/conversation` page bootstrap

Replace the blanket `if (!localDesktop) setError(localDesktopOnly)`.

**Web path (`parsed.kind === "web"`):**

1. Load conversation + folder metadata (same APIs as desktop).
2. `seedDetachedFolder` / `seedDetachedConversationSummary` /
   `seedDetachedSessionTab`.
3. Set bootstrap ready **without** rebind, claim, ready emit, or commit-ack.
4. Mount `ConversationSessionSurface` with the seeded tab; normal connect /
   find-connection / viewer logic inside the surface applies.
5. Do **not** call Tauri `emit` / `rebindConnectionOwnerWindow`.
6. Chrome: use browser-native title bar; avoid requiring Tauri window controls
   (existing `AppTitleBar` should already degrade on web—verify, fix only if
   broken).

**Desktop path:** unchanged handoff sequence.

**Unsupported:** remote desktop, or web without `mode=web` when someone hits a
desktop-shaped URL → clear error (not silent half-handoff).

### Connection / lifecycle semantics

- Prefer existing live connection via `acp_find_connection_for_conversation`.
- Viewer rules already document: stream + co-control; teardown detaches only.
- Idle sweep: detached shell already registers open tab keys so the surface’s
  open context is not reaped incorrectly—reuse `DetachedOpenTabKeysRegistrar`.
- Closing the browser tab: only that page’s JS tears down; main workspace
  connection state is independent.

### i18n

- Keep `localDesktopOnly` for truly unsupported routes (e.g. remote / wrong mode).
- No success toast on web open (the new tab is the feedback).
- **Required:** `popOutPopupBlocked` (or equivalent) when `window.open` returns
  null; do not reuse `popOutHandoffFailed` for popup blocking.

### Testing

| Area | Cases |
|------|--------|
| Enablement | web draft disabled; web real id enabled; web ignores last_tab; desktop last_tab still blocked; remote hidden |
| URL builder | query keys + mode=web + window name |
| Parser | web ok; desktop ok; missing mode on web-shaped incomplete query fails; invalid ids fail |
| Orchestration | web path never calls openConversationWindow / markTransferringOut / detach; calls window.open once |
| Page bootstrap | web branch sets ready without rebind/emit (unit or light component test) |
| Regression | existing desktop pop-out tests still pass |

### Rollout / risk

| Risk | Mitigation |
|------|------------|
| Popup blockers | Open synchronously from click handler; toast if null |
| Dual-tab local store drift | Server events are source of truth for stream; acceptable for v1 |
| Accidental handoff on web | Require `mode=web`; never emit Tauri events on web branch |
| Static export base path | Match settings/other `window.open` path style |
| Auth cookie / token on new tab | Same origin; existing web auth applies to second tab |

## Implementation outline (for planning)

1. Parser + URL builder + enablement (TDD).
2. `popOutConversation` web branch + menu visibility (`sidebar-conversation-card`,
   `tab-bar`).
3. `/conversation/page.tsx` web bootstrap path.
4. i18n for popup blocked (if not already present).
5. Tests + manual smoke on server build in browser.

## Acceptance criteria

1. Server UI shows pop-out for non-draft conversations; draft disabled.
2. Click opens a new tab with the conversation surface; main tab remains.
3. If main already has a live agent for that conversation, the new tab follows
   the stream and can send (viewer/shared connection).
4. If no live connection, the new tab can cold-start a session.
5. Second click prefers the same window name reuse when the browser allows it.
6. Local desktop pop-out behavior is unchanged (handoff + tab detach).
7. Automated tests cover the matrix above for enablement and web branch isolation.

## Out of scope follow-ups (optional later)

- Soft “opened externally” badge on the main tab.
- Exclusive writer lock between tabs.
- Web path that closes/hides the main tab (desktop-like).
- Remote-desktop multi-window parity.
