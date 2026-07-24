# Conversation Pop-out Window (OS-level)

Date: 2026-07-20

Status: Design approved (document review); implementation plan in progress

## Summary

Add the ability to **pop a conversation out of the main workspace into a
real desktop `WebviewWindow`**, so Windows snap / multi-monitor layout can
manage each session independently.

v1 is **menu-driven** (tab bar + sidebar context menu). Drag-out-to-detach is
explicitly deferred. Restart does **not** restore detached windows.

This is a **local desktop (Tauri) only** feature. Web / server browser mode and
**remote-desktop workspace windows** do not get pop-out; the menu entry is
hidden. Ownership handoff is **ready-gated** (open → takeover/rebind → then
remove main tab) so live ACP sessions and delegation children are not killed.

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
| Last session | Cannot pop out when the conversation is an open main tab **and** `tabs.length < 2` (would leave zero main tabs after remove) |
| Main focus after pop | Switch to **MRU** remaining tab (last activated among remaining) |
| Close detached window | No re-dock; remove from open tabs; sidebar list unchanged |
| Sidebar left-click while detached | Focus existing window |
| Detached chrome | Minimal: title bar + full single-session UI |
| Overlays | **Include** `SubAgentOverlay` + `AgentPlanOverlay` |
| Sub-agent “view session” | Keep existing dialog; no nested OS window in v1 |
| Restart | **Do not** restore detached windows |
| Platform | **Local desktop Tauri only** (`isLocalDesktop()`). Hidden in web/server mode **and** remote-desktop workspace windows. |
| Architecture | New route + `WebviewWindow` (same family as settings — no `.parent`) |
| Remote workspaces (v1) | **Out of scope.** Do not accept / do not show pop-out when `getActiveRemoteConnectionId() !== null`. |

---

## Architecture

### Approach

**Independent route + new `WebviewWindow` per conversation** (same family as
settings: independent top-level, no `.parent`).

```
Main window (workspace)
  tab / sidebar menu → popOutConversation(conversation_id)
       │
       ├─ prechecks (local desktop, not draft, not last tab if open)
       ├─ mark tab detaching (suppress unmount disconnect) if open in main
       ├─ Rust: open_conversation_window → label conversation-{id}
       │        URL: conversation?conversationId=…&folderId=…&agentType=…&operationId=…
       │
       ├─ wait for detached-ready handoff event (timeout → abort + toast)
       ├─ detachTab (MRU activate + persist with CAS; rollback on failure)
       └─ registry cache: add conversation id (non-authoritative)

Detached window (static export page)
  AppTitleBar + ConversationSessionSurface (props-driven shell)
  Claims ACP ownership; emits ready; on close → reverse rebind to main
  then idle-only residual for (label, operationId) — see keepalive design
  Shares process: DB, ACP ConnectionManager, event bus
```

**Rejected alternatives**

| Approach | Why rejected |
| --- | --- |
| In-app floating panes | No OS snap / multi-monitor ownership |
| Mirror dual-mount | Contradicts move semantics; dual input/scroll complexity |
| Open window then immediately remove main tab | Race: second webview attaches as **viewer**, main owner unmount disconnects idle sessions / idle-sweep reclaims busy ones |
| Remote-desktop pop-out in v1 | Remote ACP owner is `"web"` on the server; local `disconnect_by_owner_window("conversation-*")` cannot clean them up; label collisions vs `remote-*` windows |

### Window identity

| Item | Rule |
| --- | --- |
| Label | `conversation-{conversationId}` (positive DB id, **local only**). Draft / unbound tabs **cannot** pop out until bound to a real conversation id. |
| Focus existing | If label already exists → `unminimize` + `set_focus`; do not create a second window. |
| Parent | **No** `.parent(&main)` — independent top-level window (like settings), so it can move/minimize freely and participate in Windows snap alone. |
| Title | `{conversation title} · {agent}` (fallback untitled + id). Update on title change if cheap; else set at open. |
| Size | Default ~960×720; `min_inner_size` reasonable (e.g. 480×400). Center on open (or near main). No geometry persistence in v1. |
| Style | Same `apply_platform_window_style` / `post_window_setup` as other aux windows. |
| Capabilities | Register `conversation-*` in `src-tauri/capabilities/default.json` and `desktop.json` (same permission set as other aux windows). |

### Routing (static export)

Next.js is `output: "export"` — **no dynamic segments**. Use a fixed page:

- Path: `src/app/conversation/page.tsx`
- Query (local only): `conversationId`, `folderId`, `agentType`, `operationId` (required for session surface + incarnation)
- **No** `remote_connection_id` / remote window id in v1 (feature gated off for remote)

### Frontend process model

Each WebviewWindow loads its own React tree. Design constraints:

1. **Extract** a props-driven `ConversationSessionSurface` (or equivalent) from `ConversationTabView` so folder/conversation/agent are **not** resolved only via `useTabStore` row lookup. Main tab view becomes a thin wrapper that supplies props from the tab row. Detached page supplies props from query params. One ACP/composer/overlay path.
2. Detached page mounts a **minimal provider set**: session runtime, ACP connections, i18n, theme, toaster, task/alert as required by the surface — **not** full workspace sidebar/tab bar/aux panels.
3. If any ephemeral tab-store seed is still required for shared hooks, it must run in **`persistOpenedTabs: false` / detached mode** so it never hydrates or CAS-saves into the main opened-tab set.
4. Main window keeps a small **in-memory cache** of detached conversation ids → window labels. Cache is **not** authority and **not** persisted across restarts. Sidebar focus always prefers Rust `focus_conversation_window` / open-idempotent before opening a main tab.
5. Main `opened tabs` remain the source of truth for the main strip; detached sessions are **not** main tabs after successful handoff.

### Backend / ACP ownership (required protocol)

Connections already carry `owner_window_label`; main close runs
`disconnect_by_owner_window("main")`. Each WebviewWindow has an **independent**
frontend ACP store. Second webview discovery currently attaches as a **viewer**.
Owner unmount / idle sweep call `acpDisconnect` for local owners no longer in
`openTabKeys`. Therefore **Rust label rebind alone is insufficient** — both
webviews need an explicit transfer state machine.

#### Transfer identity

Every pop-out attempt generates:

| Field | Purpose |
| --- | --- |
| `operationId` | UUID; single-flight per conversation; correlates ready/ack/abort |
| `conversationId` | Positive DB id |
| `connectionId` | Live backend connection id when one exists (null if cold session) |
| `fromOwnerWindow` | Expected current Rust label (usually `"main"`) |
| `toOwnerWindow` | `conversation-{id}` |
| `ownershipGeneration` | Monotonic token written on rebind; child spawn inherits generation |

**Single-flight:** a second pop-out for the same conversation while an operation
is in flight focuses the existing attempt’s window or no-ops; it does not start
a parallel transfer.

**Rust guards:** `open_conversation_window` / rebind refuse when the **caller
window** is a remote-workspace window (label `remote-workspace-*` or equivalent
remote context). Frontend `isLocalDesktop()` is necessary but not sufficient.

#### Frontend transfer state machine

States per `(conversationId, operationId)` on main and detached:

```
Main:     Idle → Preparing → AwaitingReady → Releasing → DetachedDone
                 ↘ Aborting → Idle
Detached: Boot → Claiming → ReadyEmitted → Owning
                 ↘ Aborting → Closed
```

**Main-side release-without-disconnect (required):** after detached is ready and
before/during `detachTab`, main must:

1. Mark contextKey **`transferredOut`** (or equivalent) so:
   - unmount lifecycle **must not** `acpDisconnect`
   - idle sweep **must not** `acpDisconnect` for that connection
2. Drop local React ownership of the connection (detach subscription / remove
   from main ACP store) **without** killing the backend process.
3. Only then remove the tab from `openTabKeys` / `rawTabs`.

Without step 1–2, after tab removal the stale main owner becomes idle and the
existing sweep kills the agent even though detached owns it.

**Detached-side claim (required):**

1. Resolve conversation; discover live `connectionId` for this conversation.
2. Refuse takeover if discovered owner is remote/`"web"` or unexpected (local
   desktop only; no stealing server-owned connections).
3. Attach as **owner UI** for that `connectionId` (not a permanent viewer):
   promote/takeover API — sole controlling UI for local process.
4. Invoke Rust rebind (below); on success emit ready with `operationId`.

Must **not** spawn a second agent process for the same live conversation.

#### Rust rebind (root tree only + spawn races)

```text
rebind_connection_owner_window(
  conversation_id: i32,
  connection_id: Option<String>,  // when known
  from_owner_window: String,      // CAS expected label
  to_owner_window: String,
  operation_id: String,
) -> Result<RebindResult, …>
// RebindResult: { rebound_count, ownership_generation }
```

Rules:

1. Locate the **root** connection by `connection_id` and/or `conversation_id`
   (not “every connection with label main”).
2. CAS: only rebind if current `owner_window_label == from_owner_window`
   (or already `to_owner_window` **and** `owner_operation_id == operation_id`
   for idempotency). Reject otherwise. Reverse requires matching
   `expected_generation`.
3. Rebind the root **and** its **descendant tree only** (delegation parent→child
   edges / broker graph), never every child that happens to share `"main"`.
4. **In-flight spawn race (required concrete fence):** child spawn must not
   permanently keep a stale pre-rebind snapshot. Required mechanism:
   - Track in-flight child spawns under the parent connection id; rebind waits
     for those spawns to finish **or** marks them to adopt parent’s current
     `(label, generation, operationId)` at registration via parent-generation CAS.
   - A child becoming visible must pass parent-generation CAS; on mismatch,
     re-read parent ownership and adopt before publish.
5. Tests must cover: concurrent child spawn during rebind (barrier); unrelated
   other roots under `"main"` remain untouched.

#### Atomic handoff sequence

1. **Precheck** (main): `isLocalDesktop()`, positive id, enablement, single-flight;
   if window already exists → focus only (no transfer).
2. Create `operationId`; register ready waiter **before** open (or use Rust-held
   ack channel) so events cannot race past the listener.
3. If open main tab: enter `Preparing` — set **`detaching`** + suppress unmount
   disconnect; record `connectionId` if live.
4. Open `conversation-{id}` with query including `operationId` (+ ids).
5. Detached: Boot → Claiming (takeover + rebind) → emit
   `conversation-window://ready` `{ conversationId, operationId, connectionId? }`.
6. Main on matching ready (timeout ~15s; ignore wrong `operationId`):
   - Enter `Releasing`: **release-without-disconnect** on main ACP store.
   - `detachTab` with **awaited immediate CAS save** (not debounced fire-and-forget):
     re-check last-tab; MRU activate; if CAS fails → **compensation** (below).
   - Cache id; clear flags → `DetachedDone`.
7. Abort paths: clear flags; main remains owner if rebind never committed.

**Sidebar pop-out when not a main tab:** no `detachTab`; still operationId +
ready; if a live main connection exists for that conversation, same transfer
machine (release-without-disconnect without removing a tab).

#### Compensation / rollback order (critical)

On CAS failure or post-ready abort **after** rebind succeeded:

1. **Reverse rebind first** (`toOwnerWindow` → `fromOwnerWindow`) with CAS on
   **label + generation + operationId** for the same root tree.
2. Main **reclaims** frontend ownership (re-attach as owner; clear
   `transferredOut` for this operationId only).
3. **Then** close detached via `close_conversation_window(conversation_id, expected_operation_id)`
   (CAS: only closes if stored operationId matches — never closes a reopened
   incarnation). Close cleanup is **reverse-first + idle residual** for that
   operationId (see Close / lifecycle); reclaimed main-owned connections are
   untouched. Authoritative close rules:
   [Pop-out Close ACP Keepalive](./2026-07-24-popout-close-acp-keepalive-design.md).
4. Main tab remains; toast. **Never** close-detached-first while it still holds
   live ownership for this operationId (that would risk the session under the
   old full-disconnect path; reverse + idle residual keeps busy work alive).

If rebind never succeeded: safe to close detached for this operationId; main never released.

#### Close / lifecycle

> **Authoritative amendment:** close teardown for detached windows is defined by
> [Pop-out Close ACP Keepalive](./2026-07-24-popout-close-acp-keepalive-design.md)
> (`decide_close`, reverse-first, idle-only residual, terminal rebind, detached FE
> never bare-`acpDisconnect`). The table below is the parent summary; that design
> wins on conflict.

| Event | Ownership rule |
| --- | --- |
| Close detached while `Owning` (`CloseRequested` **and** `Destroyed`) | Capture this window’s `operationId` at open. **Reverse rebind to `main`** with **label+operationId** (+ generation when present). Residual: best-effort reverse then disconnect only **idle** connections still tagged `(conversation-{id}, operationId)`, via the **shared** idle helper on **every** close-reachable site. Terminals matching stamp **rebind** to `main` (no kill on pop-out close). Emit `conversation-window://closed` `{ conversationId, operationId, abortOutcome? }` with **honest** outcomes (`Reversed` only after successful manager reverse; `ReverseUncertain` when ambiguous). Main drops cache. **No** re-dock. Busy work continues under main ownership. Detached FE never bare-disconnects. Never reverse or disconnect by label alone (label is reused on reopen). |
| Close detached during abort after reverse rebind | Reverse already reclaimed main ownership; still run idle residual for resources still tagged with the aborted `operationId`; do not kill reclaimed main-owned or busy connections. |
| Close main while detached live | Disconnect only main-owned incarnations — must **not** touch detached operationIds. Hide-to-tray unchanged. App quit tears all down. |
| Handoff timeout / claim failure before rebind | Clear detaching; toast; main remains owner; close half-open detached; close path still reverse-first when generation exists, else best-effort reverse + idle residual for the aborted operationId. |
| Stale disconnect / idle sweep vs rebind | Destructive disconnects re-validate owner+operationId under lock; frontend disconnects carry lease tokens captured at own/rebind time. Detached owner unmount always suppresses destructive `acpDisconnect` (gate + bridge for full detached lifetime). |

Must not leave **idle** orphan agents indefinitely (idle sweep). Must not kill
**busy** agents when the only UI closes; ownership returns to `main` until the
user reopens or the process idles out. Must not kill a reopened window’s
session when a prior incarnation’s delayed cleanup runs. Detached frontend
unmount always-suppresses destructive disconnect (gate + bridge); main
`shouldDisconnectOnUnmount` no longer governs detached owner teardown.

### Opened-tabs interaction

| Action | Main tabs | Detached registry cache |
| --- | --- | --- |
| Pop-out (was open tab) after ready | **`detachTab`**: remove; MRU activate; persist CAS | Add id |
| Pop-out (sidebar, not a tab) | Unchanged | Add id after ready |
| Close detached window | No re-add | Remove id (event or probe) |
| Open from sidebar (not detached) | Existing open-tab behavior | — |
| Open from sidebar (maybe detached) | Only if Rust focus returns false | Prefer `focus_conversation_window` first |
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
| Not **local** desktop (`!isLocalDesktop()`) — web/server or remote-desktop workspace | Hidden |
| Draft tab without `conversationId` | Disabled |
| Conversation already detached (Rust window exists or cache hit) | Same menu label **「弹出窗口」**; action **focuses** the existing window (no second window) |
| Conversation is an open main tab **and** `tabs.length === 1` | Disabled (“Cannot pop out the last tab”) |
| Conversation is an open main tab **and** `tabs.length > 1` | Enabled → handoff + move |
| Conversation **not** in main tabs | Enabled → open detached only (after ready) |

### After successful pop-out (from main tab)

Follow the **atomic handoff** in Architecture (ready → release-without-disconnect → detachTab). After ready:

1. Main release-without-disconnect (see ownership state machine).
2. `detachTab`: remove from main `rawTabs` (never via bare `closeTab` replacement path).
3. Set `activeTabId` to MRU among remaining:
   - Maintain a small activation ring / `lastActivatedAt` on tab activation (plan locks exact structure).
   - Fallback: previous activation order → nearest neighbor in previous order → first remaining.
4. **Awaited immediate CAS** for opened-tabs persist (flush/bypass debounce; surface failure). On failure run compensation order (reverse rebind → reclaim main → then close detached).
5. **Cross-client note (intentional):** `opened_tabs` is workspace-global. Successful CAS removal drops this conversation from the open-tab set for **all** synchronized clients (other desktops/browsers). That matches move semantics and existing tab-close sync; document in UI only if needed. v1 does not special-case multi-client.
6. Pop-out must not intentionally produce zero main tabs; existing “ensure new conversation tab” remains a safety net only.

### Close detached window

- User clicks OS/window close.
- Rust `on_window_event` for `conversation-*` on **both** `CloseRequested` and `Destroyed`: capture the window’s `operationId`; **reverse rebind to `main`** (label+operationId, + gen when present); residual disconnect is **idle-only** for connections still tagged **`(conversation-{id}, operationId)`** (never label alone; never force-kill busy); terminals matching stamp **rebind** to `main` (no kill). Emit `conversation-window://closed` with `{ conversationId, operationId, abortOutcome? }` so main drops registry cache. Full rules:
  [Pop-out Close ACP Keepalive](./2026-07-24-popout-close-acp-keepalive-design.md).
- **Do not** re-insert into main opened tabs.
- User can reopen later via sidebar → opens **in main** as a normal tab (unless they choose pop-out again); live main-owned connection is discovered and claimed (no second spawn).

### Sidebar left-click

| State | Behavior |
| --- | --- |
| Detached (prefer Rust focus truth) | `focus_conversation_window`; do not switch main to a new tab for that id |
| Not detached | Existing behavior (open/activate main tab) |

Optional: subtle indicator on sidebar row when detached (icon / badge). Nice-to-have; not required for MVP if focus behavior is correct.

### Window chrome (detached)

- App title bar + window controls (match other Codeg windows on Windows).
- Body: single conversation surface only.
- **No** folder sidebar, **no** multi-tab bar, **no** aux file/git panel.
- **Yes**: message list, composer, connection status, permission/question UI, plan + sub-agent overlays, export/actions already on the conversation surface if they live inside `ConversationSessionSurface`.

---

## API surface (sketch)

### Rust (tauri-runtime)

```text
open_conversation_window(
  conversation_id: i32,
  folder_id: i32,
  agent_type: AgentType,
  locale: Option<AppLocale>,
  operation_id: String,
) -> Result<(), AppCommandError>
// local only; reject if caller window is remote-workspace

focus_conversation_window(conversation_id: i32) -> Result<bool, AppCommandError>
// true if focused, false if no such window

rebind_connection_owner_window(
  conversation_id: i32,
  connection_id: Option<String>,
  from_owner_window: String,
  to_owner_window: String,
  operation_id: String,
  expected_generation: Option<u64>,
) -> Result<RebindResult, AppCommandError>
// root tree only; CAS on label + operationId; generation on reverse

close_conversation_window(
  conversation_id: i32,
  expected_operation_id: String,
) -> Result<bool, AppCommandError>
// true if closed; false if no match (reopened under different operationId)
```

- Idempotent focus if window exists.
- Builds URL with query params including `operationId`; applies platform style; focus.
- Close is OS-driven; cleanup in `on_window_event` for `conversation-*` is
  **incarnation-scoped** (`operationId` captured at open) and follows the
  keepalive close path — **not** unconditional full incarnation disconnect:
  reverse rebind to `main`, then idle-only residual for matching
  `(label, operationId)` connections; terminals rebind (no kill). API abort /
  compensation close still uses reverse-before-close; residual reap on close
  sites is the shared idle helper. Authoritative:
  [Pop-out Close ACP Keepalive](./2026-07-24-popout-close-acp-keepalive-design.md).

### Frontend

```text
popOutConversation(args: {
  conversationId: number
  folderId: number
  agentType: AgentType
}): Promise<void>
// single-flight → register ready waiter → detaching → open → wait ready(operationId)
// → release-without-disconnect → detachTab + awaited CAS → cache / compensate

canPopOutConversation(...): { enabled: boolean; reason?: string }

isConversationDetachedCache(conversationId: number): boolean
// main-window memory only — not authority

focusDetachedConversation(conversationId: number): Promise<boolean>
// always invokes Rust focus; updates cache

releaseConnectionWithoutDisconnect(contextKey | connectionId): void
// main ACP store: suppress idle/unmount kill; drop local owner UI

claimConnectionOwnership(...): Promise<void>
// detached ACP store: promote/takeover live connection as owner UI
```

Wire through `lib/api.ts` / transport like other window opens (`getShellTransport` / `isLocalDesktop` gate).

### Events

- `conversation-window://ready` `{ conversationId, operationId, connectionId?, ownershipGeneration? }`  
  (`ownershipGeneration` required when a live rebind ran; omitted on cold boot)
- `conversation-window://closed` `{ conversationId, operationId }`  
  (operationId always set from window open state so main can cancel the matching handoff)
- Title updates: optional later.
- Durable backend `abort_conversation_popout_operation(operation_id)` for timeout/close-before-ready when main never received ready (generation-CAS reverse if rebind committed).

---

## i18n

Add keys (all 10 locales) under something like `Folder.tabs` / `Folder.sidebar` / `ConversationPopout`:

- `popOutWindow` — menu label (also used when already detached; action focuses)  
- `cannotPopOutLastTab` — disabled reason or toast  
- `cannotPopOutDraft` — draft without id  
- `popOutDesktopOnly` — if ever shown on web  
- `popOutHandoffFailed` — ready timeout / rebind / takeover failure  

Follow existing next-intl message file layout.

---

## Error handling

| Failure | Behavior |
| --- | --- |
| Window build fails | Toast; do **not** remove main tab; clear detaching |
| Conversation missing in DB | Toast; no window |
| Ready timeout / ownership claim fails | Toast; clear detaching; main remains owner; close half-open detached if rebind never committed |
| Race: last tab closed elsewhere during pop | Re-check count before `detachTab`; abort with toast; compensation if rebind already committed |
| Persist CAS reject after ready | **Compensation order:** reverse rebind → main reclaim → then close detached; toast; no mirror |
| Focus of missing window | Fall through to normal sidebar open / open new pop-out |
| ACP rebind fails | Abort; log + toast; main remains owner; close detached |
| Stale ready (wrong `operationId`) | Ignore |

Ordering must be **open + ready → release-without-disconnect → detachTab + awaited CAS** with reverse-before-close compensation.

---

## Testing

### Unit / component

- Enablement matrix (last tab, draft, already detached, not open, remote/web hidden).
- MRU selection after `detachTab`.
- Sidebar click prefers Rust focus before opening main tab.
- Menu items render local-desktop-only.
- Detaching flag suppresses unmount disconnect.

### Ownership / handoff (required)

- Idle connection: handoff → main release-without-disconnect → tab remove does not `acpDisconnect`; idle sweep skips `transferredOut`; detached becomes owner.
- Prompting connection: same; agent continues; main unmount does not kill turn.
- Delegation children (existing): rebind updates only root descendant tree; detached close reverse + idle residual applies per connection (busy children not killed).
- In-flight child spawn during rebind: child ends on new owner (generation revalidation); unrelated main roots untouched.
- Detached close during initialization: reverse/best-effort reverse + idle residual; main ownership restored when reverse succeeds; busy never force-killed.
- Detached close after successful handoff: reverse to `main` (even when API abort would be `AlreadyComplete`); residual idle-only; busy survives under main.
- Detached FE unmount (post-ack, pending_permission, idle): zero bare `acpDisconnect`.
- Tab-save CAS rejection after ready: reverse rebind + main reclaim **before** close detached; main tab remains.
- Wrong `operationId` ready ignored; single-flight second pop focuses/no-ops.

### Integration / Rust (as feasible)

- `open_conversation_window` idempotent focus.
- `rebind_connection_owner_window` cascades to children sharing prior label.
- Close path reverse-first then idle residual for `(conversation-*, operationId)`; never force-kills busy or touches main-owned / newer-incarnation sessions.

### Manual (Windows)

- Pop two sessions to two monitors; Win+Arrow snap each.
- Pop from tab and from sidebar.
- Close detached → sidebar still lists; reopen in main.
- Only one tab → menu disabled.
- Main hide-to-tray while detached still running; quit app cleans all.
- Remote workspace window: menu hidden.

---

## Implementation phases

### Phase 1 — MVP (this design)

1. Capabilities + Rust `open_conversation_window` / `focus_conversation_window` / `rebind_connection_owner_window` + close cleanup.  
2. Props-driven `ConversationSessionSurface` + static `conversation` page + minimal providers.  
3. Frontend ownership takeover + detaching suppress + ready event.  
4. `detachTab` + MRU + `popOutConversation` orchestration + registry cache.  
5. Tab + sidebar context menus + sidebar left-click focus.  
6. i18n + `isLocalDesktop` gating.  
7. Tests: enablement / MRU / focus routing / ownership handoff cases.

### Phase 2 — Later (out of scope)

- Drag-out detach reusing the same API.  
- Restart restore of detached set + geometry.  
- Sidebar detached badge.  
- Live title sync / badge for agent status on taskbar.  
- Remote-desktop multi-window pop-out (needs remote ownership model).

---

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Memory: N webviews | Accept for small N; document; no restore spam on boot |
| Provider tree incomplete on detached page | Checklist against ConversationTabView dependencies; smoke test send + stream + overlays |
| ACP owner disconnect kills wrong session | Atomic handoff + detaching flag + cascade rebind + tests |
| Pop-out close kills running agent | Reverse-first close + idle-only residual + detached FE suppress; see keepalive design |
| Tab remove before window ready | Ready-gated detachTab + rollback |
| Viewer attach on second webview | Explicit ownership takeover before remove |
| Static export query-only routing | Follow commit/settings window pattern |
| Capability missing for new labels | Add `conversation-*` to capability manifests |

---

## Success criteria

1. User can pop a non-last conversation from tab or sidebar menu into a real OS window (local desktop).  
2. Windows snap / independent move works per conversation window.  
3. Plan + sub-agent overlays appear when the session has that data.  
4. Last main tab cannot be popped; MRU tab activates after pop.  
5. Close detached does not re-dock; sidebar can reopen in main.  
6. Second activation focuses the existing window.  
7. App restart does not recreate detached windows.  
8. Web build and remote-desktop workspace do not expose a broken control.  
9. Live/idle ACP sessions and delegation children survive handoff; detached close reverse-rebinds to `main` and does not kill busy agents; idle leftovers are reaped by idle residual / idle sweep (not indefinite idle orphans).

---

## Open implementation notes (not product open questions)

These are plan-time choices, not unresolved product decisions:

- Exact MRU data structure (ring buffer in tab-store vs `lastActivatedAt` on tabs).  
- Exact ready-channel (Tauri event vs Rust-held ack); must still be operationId-correlated and registered before open.  
- Exact ACP store APIs for release-without-disconnect / claim-as-owner (names may differ; semantics fixed above).  
- Rebind serialization primitive (lock vs ownership_generation revalidation loop) — both satisfy the race requirement.  
- Precise default window size and whether to open centered vs offset from main.

**Resolved by this revision (were review blockers):**

- Full transfer state machine: main release-without-disconnect, detached claim, reverse-before-close compensation.  
- Root-tree-only rebind + in-flight child spawn race + unrelated-root isolation.  
- operationId single-flight ready correlation.  
- Awaited immediate CAS for detachTab (not debounced).  
- Rust caller-window guard + frontend `isLocalDesktop`.  
- Remote workspaces out of v1.  
- Props-driven session surface / non-persisting detached tab mode.  
- Capability labels `conversation-*`.  
- Intentional workspace-global opened_tabs sync on move.

---

## References

- Tile mode: `src/stores/tab-store.ts` (`isTileMode`), `conversation-detail-panel.tsx` (`canTile`)  
- Tab menu: `src/components/tabs/tab-item.tsx`  
- Multi-window: `src-tauri/src/commands/windows.rs` (`open_settings_window`, etc.)  
- Overlays: `src/components/chat/sub-agent-overlay.tsx`, `agent-plan-overlay.tsx`  
- ACP ownership: `owner_window_label`, `disconnect_by_owner_window` in `src-tauri/src/acp/manager.rs`  
- Lifecycle unmount: `src/hooks/use-connection-lifecycle.ts`  
- Platform gate: `isLocalDesktop()` in `src/lib/platform.ts`  
- Capabilities: `src-tauri/capabilities/default.json`, `desktop.json`  

