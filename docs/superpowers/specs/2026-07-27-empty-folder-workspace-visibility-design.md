# Empty Folder Workspace Visibility

**Date:** 2026-07-27  
**Status:** Draft for implementation  
**Problem:** The workspace sidebar accumulates many folders with zero conversations ("暂无会话"), and users must remove them one by one.

## Goal

Keep path **registration / history** available, but stop the **workspace sidebar** from long-lived empty folders that have no conversations.

**Success criteria**

- Sidebar does not long-term show folders with zero live conversations unless the user currently has a new-conversation draft tab for that folder.
- Opening an empty project still works: folder appears and a new-conversation draft opens automatically.
- Closing the last draft for a folder with zero conversations removes it from the workspace automatically.
- Startup reconciles existing empty open folders.
- Disk paths and git worktrees are **not** deleted by this feature (workspace visibility only).

## Non-goals

- Do not change the data model that conversations require a `folder_id` FK.
- Do not delete filesystem directories or run `git worktree remove`.
- Do not remove folders from open history permanently (user can re-open from history).
- Do not implement timeout-based hiding (hard to predict).
- Do not change Chat-mode (`kind = chat`) list exclusion rules beyond existing behavior; chat folders stay out of the regular Folders section and continue to use chat-dir GC for scratch dirs.

## Core rule

A **regular** folder stays in the workspace (`is_open = true` and visible in the user-facing open list) if and only if **at least one** of:

1. It has ≥1 **live** conversation (not soft-deleted), or  
2. The client currently has a **new-conversation draft tab** bound to that `folderId`.

Otherwise the folder must be **closed in the workspace** (`is_open = false`). The row may remain in history (`deleted_at` unchanged; path still unique).

| State | Workspace / sidebar |
|-------|---------------------|
| ≥1 live conversation | Open / shown |
| 0 conversations, has draft tab for folder | Open / shown |
| 0 conversations, no draft tab | Closed / not shown |

"Registration" (insert/upsert folder row by path) remains allowed for FK, worktree parent links, and history. Registration **must not** imply permanent workspace membership without the rule above.

## User-initiated open of an empty project

When the user explicitly opens a folder path into the workspace:

1. Upsert folder and set `is_open = true` (existing `open_folder` behavior).
2. Ensure a **new-conversation draft tab** exists for that folder (create if missing).
3. Folder appears in the sidebar because of the draft.
4. First send creates a conversation → folder stays open under rule (1).
5. If the user closes the last draft for that folder and still has zero live conversations → auto-close workspace membership (`is_open = false`).

This matches product choice: *open → appear + auto draft; leave without chatting → disappear.*

## Auto-close triggers

Evaluate the core rule and set `is_open = false` when it fails, after:

1. **Conversation delete / soft-delete** that drops the folder's live conversation count to zero (and no draft exists — drafts are client-side; backend may only close when count is zero and leave draft protection to the client, or receive an explicit "no drafts" signal; see Implementation notes).
2. **Client closes the last new-conversation draft tab** for that folder while live conversation count is zero.
3. **Startup reconcile** (server and desktop): close every **regular**, open folder with zero live conversations. Drafts do not exist yet at pure backend startup, so empty open folders are closed; the client re-opens a folder only when the user opens it or restores tabs that re-create drafts / bound sessions.

Do **not** auto-close when the user merely switches tabs, collapses a section, or navigates to another workbench route while a draft still exists.

### Chat folders

- `kind = chat` folders remain excluded from the user-facing open folder list (existing split).
- Startup empty-folder reconcile **skips** `kind = chat` (scratch lifecycle is owned by chat-dir GC and conversation binding).
- Closing a chat conversation continues existing chat cleanup; out of scope to redesign here.

## System registration paths

| Source | Desired behavior |
|--------|------------------|
| **Batch / session import** | Keep `is_open = true` only for folders that end with ≥1 imported live conversation. If a path was opened solely for a group that imported zero sessions, close it (or never leave it open). |
| **Automation per-run worktree** | Open while the run has / creates a conversation. If the run is cancelled before a conversation exists, do not leave the worktree folder open. After retention prune, empty open worktree folders are covered by startup reconcile; optional follow-up: close when run settles with no conversation. Disk/worktree GC remains a separate follow-up (existing automation note). |
| **Branch / worktree navigate** | Same as user open: open folder + new draft; auto-close when draft gone and zero conversations. |
| **Delegation `add_folder` for working_dir** | May upsert the path for FK. Prefer **not** forcing long-lived workspace open unless a conversation exists or a client draft is present. Prefer: register with open only when creating/linking a conversation; otherwise leave `is_open` false or close after if zero conversations. |
| **Explicit user "Open folder"** | Always open + draft as above. |

## Architecture

### Backend

- **`folder_service`**: helper e.g. `close_open_folders_with_no_live_conversations(conn) -> usize` counting live conversations per `folder_id` (regular kind only, `is_open = true`, `deleted_at` null).
- **Startup** (desktop + `codeg-server`): call reconcile once after DB ready (alongside existing chat-dir GC pattern).
- **Conversation delete path**: after successful delete that may zero out a folder, call targeted "maybe close folder if zero live conversations".
- **Import**: after each folder group (or batch end), if that folder has zero live conversations, set `is_open = false`.
- **Automation**: on cancel-before-conversation and preferably on settle with no conversation, close the worktree folder in workspace if still empty.
- **Delegation path registration**: avoid `is_open = true` unless a conversation is created; if `add_folder` always sets open today, either add `add_folder` option / `ensure_folder` that does not open, or close immediately when no conversation is bound.

Events: when a folder is auto-closed, emit the same workspace/folder change signal clients already use for `remove_folder_from_workspace` so sidebars drop the row without refresh races.

### Frontend

- **Open folder success path**: always ensure a new-conversation draft tab for that folder (if the open-folder UX does not already).
- **Draft tab close**: if closing the last draft for `folderId` and the store has zero live conversations for that folder, call `removeFolderFromWorkspace(folderId)` (or a dedicated "maybe close empty" API).
- **Folder remove confirmation**: user-initiated remove unchanged; auto-close should be silent (no confirm dialog) with optional low-noise toast only if product wants feedback — default **silent**.
- **Restore tabs on load**: if a restored draft references a folder that startup closed, either re-open that folder when restoring the draft, or drop the orphan draft. Prefer **re-open folder when restoring a draft tab** so the open+draft invariant holds.

### Draft vs backend knowledge

Drafts are client-only. Backend startup reconcile will close empty open folders even if a previous session had drafts (those tabs may restore afterward). Order on client bootstrap:

1. Load open folders (post-server reconcile).  
2. Restore tabs.  
3. For any restored draft whose folder is not open, `open_folder_by_id` / re-open, then keep draft.

This avoids "draft without folder" and "empty folder without draft" after restart.

## Data / API surface

Prefer reusing:

- `set_folder_open(id, false)` / `remove_folder_from_workspace`
- Existing folder change events

Add only if needed:

- `reconcile_empty_open_folders` command (also used at startup internally)
- Optional `ensure_folder_path(path, open: bool)` for delegation/import to register without opening

No schema migration required if `is_open` already exists.

## Testing

- Unit: reconcile closes regular open folders with zero live conversations; leaves folders with ≥1; skips chat kind; skips already closed.
- Unit/integration: delete last conversation closes folder when no client draft (backend).
- Frontend: close last draft for empty folder calls remove-from-workspace; open empty folder creates draft.
- Import: zero-import group does not leave open empty folder.
- Automation (if covered by existing tests): cancel before conversation does not leave open worktree folder.
- Regression: open project → send message → folder remains; folder with conversations never auto-closed solely by tab switch.

## Rollout / risk

- **Risk:** User opens folder, looks at files without keeping a draft, folder disappears. Mitigation: open always creates a draft; file workspace can still use history open. If file-only browsing without draft is required later, add an explicit "keep in workspace" pin — **out of scope** unless requested.
- **Risk:** Multi-window drafts — window A has draft, window B closes last local draft and auto-closes folder. Mitigation: auto-close on draft close should prefer server-side "zero conversations" only when the client knows it is the last draft **or** only close from the draft-close path when local draft count hits zero and accept multi-window edge case; better: **backend close only on zero conversations**, frontend draft-close calls remove only when local drafts for that folder are zero — multi-window may re-open on activity. Document as known v1 limitation; optional later: server-side draft leases.
- **Risk:** Startup closes folder user had open for file tree only. Accept per product decision (draft required for empty membership).

## Implementation sketch (ordered)

1. Backend `close_open_folders_with_no_live_conversations` + unit tests.  
2. Wire startup reconcile (desktop + server).  
3. Conversation delete → maybe-close folder.  
4. Import / automation / delegation open semantics.  
5. Frontend: open → ensure draft; draft close → maybe remove; tab restore → re-open if needed.  
6. Manual QA checklist on worktree switch and automation cancel.

## Product decisions (locked)

- Visibility model: **workspace does not hang empty folders**; history registration OK.  
- Empty project open: **appear + auto new draft**; leave without chat → auto leave workspace.  
- Existing junk: **startup reconcile** closes empty open regular folders.
