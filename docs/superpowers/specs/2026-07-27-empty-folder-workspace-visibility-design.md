# Empty Folder Workspace Visibility

**Date:** 2026-07-27  
**Status:** Ready for implementation (post design review)  
**Problem:** The workspace sidebar accumulates many folders with zero conversations ("暂无会话"), and users must remove them one by one.

## Goal

Keep path **registration / history** available, but stop the **workspace sidebar** from long-lived empty folders that have no conversations.

**Success criteria**

- Sidebar does not long-term show folders with zero **user-facing live** conversations unless the same client currently has the singleton new-conversation draft bound to that folder.
- Opening an empty project still works: folder appears and the singleton draft is retargeted to that folder automatically.
- Leaving an empty folder (close draft / retarget draft away / last-tab leave path) with zero live conversations removes it from the workspace automatically and silently.
- Startup reconciles existing empty open regular folders so the first open-folder list clients see is already cleaned.
- Disk paths and git worktrees are **not** deleted by this feature (workspace visibility only).
- Explicit user "remove from workspace" remains allowed for **any** open folder (including non-empty) and stays sticky until the user opens it again.

## Non-goals

- Do not change the data model that conversations require a `folder_id` FK.
- Do not delete filesystem directories or run `git worktree remove`.
- Do not soft-delete history rows for this feature (user can re-open from history).
- Do not implement timeout-based hiding.
- Do not change Chat-mode (`kind = chat`) list exclusion rules; chat folders stay out of the regular Folders section and continue to use chat-dir GC for scratch dirs.
- Do not implement cross-client draft leases in v1 (see Authority model).

---

## Core rule (auto-close eligibility — not an unqualified iff)

### Definitions

- **Live conversation (close predicate):** a conversation row with `deleted_at IS NULL` for that `folder_id`.
- **User-facing live conversation (product intent):** same as live for v1 **including** delegation children and loop rows that still reference the folder. Rationale: FK / retention still tie the folder to real sessions; an "empty" sidebar section may still mean hidden children exist. **Exception documented:** if the Folders section still looks empty because children are filtered, that is accepted v1 residual UX; do not invent a second count without product re-open.
- **Draft:** client-side **singleton** tab with `conversationId == null` (at most one per window/client). Opening a new conversation for another folder **retargets** that same tab (`folderId` changes); it is not a per-folder draft row.
- **Regular folder only:** auto-close helpers never act on `FolderKind::Chat`.

### Auto-close eligibility

An **already-open** **regular** folder is **eligible for auto-close** when it has
**zero live conversations**, plus:

| Actor | Additional draft check |
|-------|------------------------|
| **Client** (draft leave) | After the transition, this client has **no** singleton draft bound to that `folderId`. |
| **Headless backend** (delete, import, automation, startup, etc.) | **No draft evaluation** — drafts are client-only. Backend closes on live-count alone; clients with a still-bound draft apply the mid-session re-open guard only for `Close.cause = AutoEmpty`. |

When eligible, set `is_open = false`. The history row remains (`deleted_at` unchanged).

This is **not** an unqualified iff:

| Situation | Behavior |
|-----------|----------|
| ≥1 live conversation, open | Stay open (auto-close must not run) |
| 0 live, singleton draft targets folder | Stay open **on that client**; see Authority for other clients |
| 0 live, no draft on acting client | Auto-close |
| User explicitly removes from workspace | Always allowed even if live conversations exist; sticky closed until explicit re-open / open-folder / open-by-id |
| Manually closed folder still has live conversations | Remains closed until user re-opens (auto helpers never force-open solely because count > 0) |
| Parent folder with 0 live, but open worktree **children** have live conversations | **Per-folder** rule: parent may auto-close; children stay. Product accepts this. |

**Registration** (insert/upsert folder row by path) remains allowed for FK, worktree parent links, and history. Registration **must not** force permanent workspace membership without an open reason (user open + draft, **user-visible** live conversation after create, or explicit open API). Hidden/loop children that already exist keep a folder from auto-closing but do **not** force-open a newly registered closed folder.

---

## Authority model (v1: best-effort drafts)

**Chosen model: best-effort client draft protection — not server draft leases.**

| Authority | Owns |
|-----------|------|
| Backend | Live conversation count; `is_open`; startup reconcile; headless closes (delete last, import empty, automation cancel, etc.) |
| Client | Singleton draft; re-open when a mid-session `Close` arrives while local draft still targets that folder |

**Implications (explicitly weaken any absolute "iff"):**

1. Backend **may** close an empty folder while another window still has a draft for it. That window's handler for `Close{cause: AutoEmpty}` must **silently re-open** (`addFolderToWorkspaceById` / open-by-id) without focus steal and without creating a second draft. `Close{cause: UserRemove}` never re-opens from a pre-existing draft.
2. Multi-window draft conflicts are a **known v1 limitation**; optional later: server-side draft leases.
3. Success criteria use "long-term" / "same client" language, not a cross-client hard invariant.

Cold start: drafts are **not** persisted in `opened_tabs` (`buildPersistItems` filters `conversationId != null`). There is **no** draft restore after process restart. Startup reconcile closing empty opens is correct and must not wait for draft restore.

---

## User-initiated open of an empty project

When the user explicitly opens a folder path (or open-by-id / worktree open that should show an empty project) into the workspace:

1. Upsert folder and set `is_open = true`.
2. Ensure the **singleton** draft is retargeted to that folder (via a single orchestration choke point — see Frontend).
3. Folder appears in the sidebar (open + draft).
4. First send creates a conversation → stays open via live count.
5. Leaving the draft (close / retarget / last-tab path) with zero live conversations → auto-close.

Product: *open → appear + auto draft; leave without chatting → disappear.*

---

## Draft leave transitions (singleton)

Any mutation that changes which folder the singleton draft protects must run an empty-folder check on the **previous** `folderId` after the transition commits.

**Leave triggers (must all be covered):**

1. **Close draft tab** (including close-other / close-all paths that remove the draft).
2. **Retarget** draft from folder A → folder B (including chat-mode / folderless retarget).
3. **Last-tab close** on a sole empty folder (special ordering — below).
4. **Detach / remote snapshot** that removes or moves the draft away from a folder.

**Do not** auto-close on mere active-tab switch among conversation tabs.

### Last-tab / replacement-draft ordering (required)

Today `closeTab` on the only tab often inserts `makeReplacementDraftTab(closingTab)`, which prefers the same `folderId`. That would prevent "leave without chat → disappear."

**Required order when closing a draft for `folderId` F with zero live conversations:**

1. Decide auto-close eligibility for F **before** inserting a replacement draft that would re-bind to F.
2. If eligible: perform **visibility-only** empty close for F (API + local drop) first.
3. Then run replacement-draft logic against the **updated** open-folder list (or empty workspace). `makeReplacementDraftTab` **must not** re-bind to a folder that was just auto-closed in this close.
4. If the workspace has no remaining folders, allow zero tabs / product-existing empty state rather than resurrecting F.

---

## Auto-close triggers (summary)

| Trigger | Who | Action |
|---------|-----|--------|
| Live count → 0 after delete/soft-delete | Backend | Conditional visibility close + emit `FolderChange::Close` |
| Draft leave (close/retarget/last-tab) with local zero live | Client | **Mandatory** visibility-only conditional close API (never user-remove cascade) + silent |
| Startup | Backend (barrier) | Bulk close empty open regular folders before first client-visible open list |
| Import group ends with zero live | Backend | Conditional close + `Close{cause:AutoEmpty}` wins over any preceding Upsert for that folder in the same batch |
| Automation cancel / fail / settle before conversation after worktree folder opened | Backend | Conditional close + `Close{cause:AutoEmpty}`; keep disk worktree |
| Delegation register working_dir without conversation yet | Backend | `ensure_folder` registration-only (preserve open if already open); open + upsert after **user-visible** conversation create |

---

## Two close paths

| Path | When | DB | Tabs / watches | Event |
|------|------|-----|----------------|-------|
| **User remove** | Explicit UI remove | `is_open=false` (unconditional for that id) | Existing cascade: delete folder tabs, bump CAS, stop office watches under folder | Emit `FolderChange::Close { folder_id, cause: UserRemove }` |
| **Empty auto-close** | Eligibility rule | Conditional: only if still open, regular, not deleted, **and** zero live conversations (single transaction / `WHERE NOT EXISTS` live) | **Must not** delete persisted tabs, stop office watches, soft-delete folder, or touch disk/worktree | Emit `FolderChange::Close { folder_id, cause: AutoEmpty }` |

Frontend draft-leave **must** call the visibility-only conditional operation (thin command wrapping `close_folder_if_no_live_conversations`). **Never** use the user-remove cascade for auto-close. Return whether closed; client drops locally on success or on event.

**TOCTOU:** the DB condition is authoritative. Client pre-checks only avoid unnecessary calls.

---

## Multi-client event contract

**Today:** `FolderChange` is upsert-only; `remove_folder_from_workspace_core` does **not** emit `folder://changed`. Local remove filters the store only.

**Required:**

```rust
// folder://changed
// wire kind: "close"; fields snake_case (match ConversationChange style)
enum FolderCloseCause {
    /// Empty auto-close / visibility-only close.
    AutoEmpty,
    /// Explicit user "remove from workspace" (sticky).
    UserRemove,
}
FolderChange::Close {
    folder_id: i32,
    cause: FolderCloseCause,
}
```

```ts
export type FolderCloseCause = "auto_empty" | "user_remove"
export type FolderChange =
  | { kind: "upsert"; folder: FolderDetail }
  | { kind: "close"; folder_id: number; cause: FolderCloseCause }
```

**Handler rules:**

1. On `close`: drop folder from open list **locally only** (no re-API that double-closes). Branch cache: either keep all-history branch map (current `listAllFolderDetails` seed — Close need not prune) **or** prune closed ids; pick one and keep invariant consistent with `fetchFolders` (v1 default: **branch cache may include history; open list is membership**).
2. **`cause === "auto_empty"`** and singleton draft still targets `folder_id`: schedule **silent re-open** via open-by-id / add-to-workspace-by-id (no focus steal; no second draft).
3. **`cause === "user_remove"`**: **never** re-open from a pre-existing draft. Dispose the local draft binding for that folder (retarget draft to another open folder if any, or clear folderId / close draft per product empty-state rules).
4. **Connected-client ordering repair (required v1):** after applying any `Close`, start a membership refetch, with a **stale-response fence**:
   - Capture a monotonically increasing **folder-event generation** (or equivalent) when the Close is applied / before the request starts.
   - If any `Close`/`Upsert`/other folder membership event is applied before the response is committed, **discard or retry** that response (must not overwrite newer event state).
   - Serialize Close, Upsert, and refetch application through one client reconciliation path with this rule.
   - After a successful non-stale refetch, re-apply the **AutoEmpty draft re-open guard** if the draft still targets a folder missing from the open list (so re-open is not permanently stripped by an intermediate closed snapshot).
   - Reconnect still performs a full open-folder refetch with the same fence.
5. Explicit reopen APIs (`open_folder`, `open_folder_by_id`, add-to-workspace) **must emit `FolderChange::Upsert`** after successfully setting `is_open = true` so other clients converge.

**Emit Close from:** user remove success (`UserRemove`), empty auto-close success (`AutoEmpty`) on delete, import, automation, client-triggered conditional close, etc. Startup bulk close: barrier-before-serve is primary; optionally emit `AutoEmpty` Close per id for already-connected server clients.

**Import Close-wins:** if a batch emits Upsert then closes the same folder, emit `Close{AutoEmpty}` **after** so ordered clients end closed; refetch repair covers mis-order.

---

## Startup reconcile (barrier)

Do **not** copy fire-and-forget chat-dir GC as the sole guarantee.

**Required guarantee (pick primary):**

1. **Barrier (required):** empty-open reconcile completes successfully (or fails loud with retry/log) **before**:
   - Server: binding/serving routes that return open folders.
   - Desktop: first workspace open-folder fetch used to populate the sidebar (or setup gate before webview loads workspace data).
2. **Optional:** also emit Close for each closed id (harmless if no subscribers).

Failure policy: log error; do not crash process; on failure, clients may still see junk until next successful reconcile — document as degraded.

Chat-dir GC may remain background; empty-folder visibility must not.

---

## System registration paths

| Source | Desired behavior |
|--------|------------------|
| **Batch / session import** | After each group (or batch end), if live count is 0, conditional close + Close (wins over Upsert). Check authoritative live count, not only import tally. |
| **Automation per-run worktree** | On every exit after folder opened and **before** a live conversation exists (cancel, disabled agent, spawn fail, conversation insert fail, settle with no conversation): conditional close + Close. Keep disk worktree. Startup covers stragglers. |
| **Branch / worktree navigate** | Same as user open: open + draft choke point. |
| **Delegation** (`manager` **and** durable broker reserve) | `ensure_folder(path, RegistrationOnly)` for working_dir FK; after conversation create, if conversation is user-visible, ForceOpen + `emit_folder_upsert`. Cover create failure cleanup (no bare open left). |
| **Explicit user open** | Always open + draft choke point. |

---

## Frontend architecture

### Open + draft choke point

Introduce a single user-intent helper (e.g. `openFolderWithDraft` in `src/lib/` or equivalent mediator) used by:

- path open (dropdown, sidebar history, chrome controller)
- open-by-id
- worktree open / clone / project-boot that leaves an empty regular folder visible
- `WorkspaceOpenFolderListener` / switch-to-branch (may already open draft; must not break singleton)

Keep low-level `openFolder` store/API available for **system** registration without draft.

Avoid circular imports: mediator imports both stores' `getState`, or tab-store listens to workspace events.

### Draft protection

- After every leave transition: if previous folder has zero live in store and no draft remains, call the **required** conditional empty-close transport API.
- On `folder://changed` close with `cause === auto_empty` while draft targets folder: re-open silently (after/within fenced refetch — see event handler).
- On `cause === user_remove`: never re-open; dispose/retarget draft.
- Cold start: no draft restore path.

### Silent auto-close

No confirm dialog, no required toast. User-initiated remove keeps existing confirm UX if any.

---

## Data / API surface

| API | Purpose |
|-----|---------|
| `count_live_conversations_for_folder` | Shared count |
| `close_folder_if_no_live_conversations` | Single-folder conditional close; returns `bool` flipped; emit Close only when true |
| `close_open_folders_with_no_live_conversations` | Bulk; returns **closed folder ids** (caller derives count) |
| `ensure_folder(path, RegistrationOnly \| ForceOpen)` | See write semantics below |
| `emit_folder_close(folder_id, cause)` | Broadcast `FolderChange::Close` |
| **Required** conditional-close transport | Same core on **both** runtimes: Tauri command + Axum route; TS API returns `{ closed: boolean }` (or project-equivalent). Frontend draft-leave **must** call this — never user-remove cascade. |

**`ensure_folder` / registration write semantics (not a bare destructive boolean):**

| Mode | Existing live row | New or revived row |
|------|-------------------|--------------------|
| **RegistrationOnly** | **Preserve** `is_open` (never force closed; never force open) | Insert/revive with `is_open = false`; do **not** treat as user open (`last_opened_at` only if product already updates on any touch — prefer **not** masquerading as user open) |
| **ForceOpen** (explicit user open / open-by-id) | Set `is_open = true`, update open timestamps as today, emit Upsert | Same |

No schema migration.

---

## Testing (minimum)

- Reconcile: empty regular closed; with live kept; chat skipped; already closed idempotent.
- Conditional close does not force-open; does not soft-delete; no disk/worktree delete.
- Delete last conversation → close + Close event (no draft).
- Explicit remove non-empty folder still works and emits `Close{UserRemove}`; remote client with draft for F does **not** re-open; draft disposed/retargeted.
- Auto-close does not delete persisted tabs / stop office watches / soft-delete / disk.
- Frontend: open empty → draft ensured; retarget A→B closes A when A empty; last-tab close on sole empty folder closes F and does not rebind replacement to F.
- Close handler drops locally without re-API; `AutoEmpty` re-opens when draft still targets; after Close, fenced refetch (stale response discarded); Upsert-in-flight-vs-refetch test.
- Conditional-close Tauri + Axum + TS transport returns closed boolean.
- RegistrationOnly preserve open on existing row; new row stays closed.
- Import zero sessions → not left open; Close after Upsert.
- Automation cancel/fail before conversation → not left open.
- Startup barrier: open list after ready has no empty regular opens (integration or sequencing test).
- Stale Close after newer open: refetch repair restores open membership.

---

## Rollout / residual risks

- Multi-window drafts: best-effort; re-open guard same-window only.
- Transient flicker on delete-last + draft re-open: accept or suppress drop when local draft present before applying Close.
- Parent auto-close with open children: accepted per-folder rule.
- File-tree-only without draft: unsupported; open always creates draft; pin later out of scope.
- Live count includes hidden children: may leave visually sparse folders open — accepted v1.

## Implementation sketch (ordered)

1. Backend conditional close helpers + tests (return closed ids for bulk).  
2. `FolderChange::Close` + emit + frontend handler (+ re-open guard).  
3. Startup reconcile **with barrier** (desktop + server).  
4. Delete last conversation → maybe-close + emit.  
5. Frontend draft leave / last-tab order / open+draft choke point.  
6. Import / automation all early-exit paths / delegation ensure-without-open both paths.  
7. Manual QA checklist.

## Product decisions (locked)

- Visibility: **no long-lived empty regular folders** without same-client draft; history registration OK.  
- Empty project open: **appear + auto singleton draft**; leave without chat → auto leave.  
- Explicit user close: always allowed; sticky.  
- Existing junk: **startup reconcile with readiness barrier**.  
- v1 draft authority: **best-effort client**, not leases.  
- Parent vs worktree children: **per-folder** live count.  
- Multi-client Close event: **required new wire contract**.
