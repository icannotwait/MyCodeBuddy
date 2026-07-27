# Empty Folder Workspace Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the workspace sidebar from long-lived open folders that have zero live conversations, while keeping path registration/history and open-project → chat flows working.

**Architecture:** Backend owns “zero live conversations ⇒ `is_open = false`” for regular folders (startup reconcile, delete last conversation, empty import, automation cancel-before-conversation, delegation path ensure-without-open). Frontend owns draft protection: open folder always ensures the singleton new-conversation draft targets that folder; when the draft leaves a folder that still has zero conversations, call remove-from-workspace (silent). Multi-client sync uses a new `folder://changed` `close` variant so sidebars drop auto-closed rows without a full refetch.

**Tech Stack:** Rust (SeaORM, Tauri commands, Axum handlers), TypeScript/React (Zustand stores, event subscriptions), Vitest + `cargo test --features test-utils`.

**Spec:** `docs/superpowers/specs/2026-07-27-empty-folder-workspace-visibility-design.md`

## Global Constraints

- Workspace visibility only: set `is_open = false`; do **not** soft-delete history rows, delete disk paths, or `git worktree remove`.
- Apply auto-close only to `FolderKind::Regular`; skip `FolderKind::Chat` (chat-dir GC owns scratch lifecycle).
- Live conversation = row with `deleted_at` null for that `folder_id`.
- New-conversation drafts are a **client-side singleton** (`conversationId == null`, at most one tab) retargeted across folders — not per-folder draft rows.
- Auto-close is **silent** (no confirm dialog, no required toast).
- Prefer reusing `set_folder_open` / `remove_folder_from_workspace_core` patterns; emit folder events when closing from headless/backend paths so clients converge.
- No schema migration.

---

## File map

| File | Responsibility |
|------|----------------|
| `src-tauri/src/db/service/folder_service.rs` | Count live convs; close empty open regular folders (bulk + single); optional `ensure_folder` that can leave `is_open` false |
| `src-tauri/src/web/event_bridge.rs` | `FolderChange::Close { folder_id }` |
| `src-tauri/src/commands/folders.rs` | `emit_folder_close`; wire close into remove/reconcile helpers that need broadcast |
| `src-tauri/src/commands/conversations.rs` | After delete cleanup: maybe-close regular folder if zero live convs; import: close empty groups |
| `src-tauri/src/lib.rs` | Desktop startup: run empty-folder reconcile next to chat-dir GC |
| `src-tauri/src/bin/codeg_server.rs` | Server startup: same reconcile |
| `src-tauri/src/automation/engine.rs` | Cancel-before-conversation: close empty worktree folder + emit close |
| `src-tauri/src/acp/manager.rs` | Delegation path: ensure folder without forcing permanent open when no conversation yet |
| `src/lib/types.ts` | `FolderChange` union adds `close` |
| `src/contexts/app-workspace-context.tsx` | Handle `close` → drop from `folders` list |
| `src/stores/app-workspace-store.ts` | `openFolder` / open-by-id helpers may stay thin; draft orchestration in tab store / open call sites |
| `src/stores/tab-store.ts` | On draft retarget/close: maybe remove empty folder from workspace; restore draft re-opens folder |
| Call sites that open folders without draft | Ensure draft after open (sidebar, clone, dropdown, store if centralized) |

---

### Task 1: Backend — close empty open regular folders

**Files:**
- Modify: `src-tauri/src/db/service/folder_service.rs`
- Test: same file `#[cfg(test)]` module (or extend existing tests at bottom of file if present; otherwise add tests next to other folder_service tests / use `crate::db::test_helpers`)

**Interfaces:**
- Produces:
  - `pub async fn count_live_conversations_for_folder(conn: &DatabaseConnection, folder_id: i32) -> Result<u64, DbError>`
  - `pub async fn close_folder_if_no_live_conversations(conn: &DatabaseConnection, folder_id: i32) -> Result<bool, DbError>` — returns `true` if it flipped `is_open` from true to false; no-op for missing, chat kind, already closed, or count > 0
  - `pub async fn close_open_folders_with_no_live_conversations(conn: &DatabaseConnection) -> Result<usize, DbError>` — bulk reconcile; returns number of folders closed

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn close_open_folders_with_no_live_conversations_closes_empty_regular() {
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;

    let db = fresh_in_memory_db().await;
    let empty_id = seed_folder(&db, "/tmp/codeg-empty-open").await;
    let kept_id = seed_folder(&db, "/tmp/codeg-kept-open").await;
    seed_conversation(&db, kept_id, AgentType::ClaudeCode).await;

    let n = folder_service::close_open_folders_with_no_live_conversations(&db.conn)
        .await
        .expect("reconcile");
    assert_eq!(n, 1);

    let open = folder_service::list_open_folders(&db.conn).await.expect("list");
    let open_ids: Vec<i32> = open.iter().map(|f| f.id).collect();
    assert!(!open_ids.contains(&empty_id));
    assert!(open_ids.contains(&kept_id));
}

#[tokio::test]
async fn close_open_folders_skips_chat_kind() {
    use crate::db::test_helpers::fresh_in_memory_db;
    let db = fresh_in_memory_db().await;
    let chat = folder_service::add_chat_folder(&db.conn, "/tmp/codeg-chat-scratch/x")
        .await
        .expect("chat folder");
    // ensure is_open true (add_chat_folder already opens)
    let n = folder_service::close_open_folders_with_no_live_conversations(&db.conn)
        .await
        .expect("reconcile");
    assert_eq!(n, 0);
    let still = folder_service::get_folder_by_id(&db.conn, chat.id)
        .await
        .expect("get")
        .expect("exists");
    // chat still exists and was not soft-deleted; open flag may remain true
    // (list_open_folder_details excludes chat regardless)
    let _ = still;
}

#[tokio::test]
async fn close_folder_if_no_live_conversations_is_idempotent() {
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    let db = fresh_in_memory_db().await;
    let id = seed_folder(&db, "/tmp/codeg-once").await;
    assert!(folder_service::close_folder_if_no_live_conversations(&db.conn, id)
        .await
        .unwrap());
    assert!(!folder_service::close_folder_if_no_live_conversations(&db.conn, id)
        .await
        .unwrap());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run (from `src-tauri/`):

```powershell
cargo test --features test-utils close_open_folders_with_no_live_conversations_closes_empty_regular -- --nocapture
```

Expected: FAIL (function not found or link error).

- [ ] **Step 3: Implement helpers**

Implementation sketch (place near other folder open helpers in `folder_service.rs`):

```rust
pub async fn count_live_conversations_for_folder(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<u64, DbError> {
    use crate::db::entities::conversation;
    Ok(conversation::Entity::find()
        .filter(conversation::Column::FolderId.eq(folder_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .count(conn)
        .await?)
}

pub async fn close_folder_if_no_live_conversations(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<bool, DbError> {
    let row = folder::Entity::find_by_id(folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(conn)
        .await?;
    let Some(row) = row else { return Ok(false) };
    if row.kind != FolderKind::Regular || !row.is_open {
        return Ok(false);
    }
    if count_live_conversations_for_folder(conn, folder_id).await? > 0 {
        return Ok(false);
    }
    set_folder_open(conn, folder_id, false).await?;
    Ok(true)
}

pub async fn close_open_folders_with_no_live_conversations(
    conn: &DatabaseConnection,
) -> Result<usize, DbError> {
    let open = list_open_folder_details(conn).await?; // regular + is_open only
    let mut closed = 0usize;
    for f in open {
        if close_folder_if_no_live_conversations(conn, f.id).await? {
            closed += 1;
        }
    }
    Ok(closed)
}
```

Use SeaORM `count` (import `sea_orm::PaginatorTrait` if required by project edition).

- [ ] **Step 4: Run tests to verify they pass**

```powershell
cargo test --features test-utils close_open_folders -- --nocapture
cargo test --features test-utils close_folder_if_no_live -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/db/service/folder_service.rs
git commit -m "feat(folders): close open regular folders with no live conversations"
```

---

### Task 2: FolderChange::Close + emit helpers

**Files:**
- Modify: `src-tauri/src/web/event_bridge.rs` (`FolderChange` enum)
- Modify: `src-tauri/src/commands/folders.rs` (`emit_folder_close`, optionally enhance remove path)
- Modify: `src/lib/types.ts` (`FolderChange` type)
- Modify: `src/contexts/app-workspace-context.tsx` (handle close)
- Test: `src/contexts/app-workspace-context.test.tsx` (extend folder://changed suite)
- Test: existing Rust test `emit_folder_upsert_broadcasts_on_folder_channel` pattern in `folders.rs` — add close emit test

**Interfaces:**
- Consumes: Task 1 close helpers (optional at emit call sites later)
- Produces:
  - Rust: `FolderChange::Close { folder_id: i32 }` with serde `kind: "close"`
  - TS: `{ kind: "close"; folder_id: number }` (camelCase if project serializes camelCase — **match existing Upsert field naming**. Today Upsert uses `folder` nested object; check wire format. Prefer `folder_id` snake in Rust + serde rename if frontend already uses camelCase via a global rename. Inspect a live payload or existing serde attrs on sibling events. If Upsert arrives as `{ kind: "upsert", folder: {...} }` with camelCase `folderId` inside FolderDetail, use `#[serde(rename_all = "camelCase")]` only if the enum already does — **FolderChange currently has no rename_all on the enum; tag is snake_case kind**. Nested FolderDetail likely uses camelCase via its own derives. For Close, use `folder_id` in Rust and map in TS as `folder_id` **or** `folderId` consistently with other events — grep `conversation://` delete payload. Prefer:

```rust
Close { folder_id: i32 }
```

```ts
export type FolderChange =
  | { kind: "upsert"; folder: FolderDetail }
  | { kind: "close"; folder_id: number }
```

If the transport camelCases all JSON keys, TypeScript may need `folderId`. Align with how other `{ id }` events deserialize in this app (check `ConversationChange` delete).

  - `pub(crate) fn emit_folder_close(emitter: &EventEmitter, folder_id: i32)`

- [ ] **Step 1: Write failing frontend test**

In `app-workspace-context.test.tsx`, after upsert tests:

```ts
it("removes a folder from the open list on folder://changed close", async () => {
  // seed folder 12 open via upsert, then emit close
  emitFolder({ kind: "upsert", folder: makeFolder({ id: 12 }) })
  await waitFor(() => {
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(true)
  })
  emitFolder({ kind: "close", folder_id: 12 })
  await waitFor(() => {
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(false)
  })
})
```

(Adjust property name if wire format uses `folderId`.)

- [ ] **Step 2: Run frontend test — expect fail**

```powershell
pnpm exec vitest run src/contexts/app-workspace-context.test.tsx -t "folder://changed close"
```

- [ ] **Step 3: Implement enum + emit + handler**

Rust:

```rust
pub enum FolderChange {
    Upsert {
        folder: Box<crate::models::FolderDetail>,
    },
    /// Workspace membership closed (`is_open = false`); row may remain in history.
    Close {
        folder_id: i32,
    },
}

pub(crate) fn emit_folder_close(emitter: &EventEmitter, folder_id: i32) {
    crate::web::event_bridge::emit_event(
        emitter,
        crate::web::event_bridge::FOLDER_CHANGED_EVENT,
        crate::web::event_bridge::FolderChange::Close { folder_id },
    );
}
```

Also call `emit_folder_close` at the end of `remove_folder_from_workspace_core` after successful `set_folder_open(..., false)` so remote clients drop the row (local store already filters; idempotent).

Frontend handler:

```ts
if (change.kind === "upsert") { /* existing */ }
else if (change.kind === "close") {
  const store = useAppWorkspaceStore.getState()
  // Prefer a small store method if cleaner; inline filter is OK:
  void store.removeFolderFromWorkspaceLocal?.(change.folder_id)
}
```

**Do not** call the API again from the event handler (that would double-close). Only update local state:

```ts
// app-workspace-store.ts
dropFolderFromOpenList: (folderId: number) => {
  const { folders, branches } = get()
  const patch: Partial<AppWorkspaceStoreState> = {
    folders: folders.filter((f) => f.id !== folderId),
  }
  if (branches.has(folderId)) {
    const next = new Map(branches)
    next.delete(folderId)
    patch.branches = next
  }
  set(patch)
},
```

`removeFolderFromWorkspace` keeps API call + local drop; event handler uses `dropFolderFromOpenList` only.

- [ ] **Step 4: Run tests**

```powershell
pnpm exec vitest run src/contexts/app-workspace-context.test.tsx
cargo test --features test-utils emit_folder -- --nocapture
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/web/event_bridge.rs src-tauri/src/commands/folders.rs src/lib/types.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx src/stores/app-workspace-store.ts
git commit -m "feat(folders): broadcast workspace close on folder://changed"
```

---

### Task 3: Startup reconcile (desktop + server)

**Files:**
- Modify: `src-tauri/src/lib.rs` (desktop setup, near chat-dir GC ~line 536)
- Modify: `src-tauri/src/bin/codeg_server.rs` (near existing `gc_orphan_chat_dirs_core` ~line 232)
- Optional thin wrapper in `commands/folders.rs`: `pub async fn reconcile_empty_open_folders_core(conn) -> Result<usize, AppCommandError>`

**Interfaces:**
- Consumes: `folder_service::close_open_folders_with_no_live_conversations`
- Produces: startup side effect only (no new public API required)

- [ ] **Step 1: Wire desktop startup**

Spawn the same style async task as chat-dir GC:

```rust
{
    let conn = app.state::<db::AppDatabase>().conn.clone();
    tauri::async_runtime::spawn(async move {
        match folder_service::close_open_folders_with_no_live_conversations(&conn).await {
            Ok(n) if n > 0 => tracing::info!(
                "[folders] empty-open reconcile: closed {n} folder(s)"
            ),
            Ok(_) => {}
            Err(err) => tracing::error!("[folders] empty-open reconcile failed: {err}"),
        }
    });
}
```

Import `folder_service` via existing module paths (`crate::db::service::folder_service`).

- [ ] **Step 2: Wire server startup** (same call + log after DB ready, next to chat-dir GC).

- [ ] **Step 3: Manual sanity**

```powershell
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: compile OK.

Note: startup reconcile does not emit per-folder close events (no clients connected yet). Clients load open list after reconcile — correct by design.

- [ ] **Step 4: Commit**

```powershell
git add src-tauri/src/lib.rs src-tauri/src/bin/codeg_server.rs
git commit -m "feat(folders): reconcile empty open folders at startup"
```

---

### Task 4: Delete last conversation → maybe-close folder

**Files:**
- Modify: `src-tauri/src/commands/conversations.rs` (`delete_conversation_with_cleanup_core`)
- Test: `src-tauri/src/commands/conversations.rs` tests (or integration)

**Interfaces:**
- Consumes: `close_folder_if_no_live_conversations`, `emit_folder_close`
- Produces: after delete, if regular folder now has 0 live convs → close + emit

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn delete_last_conversation_closes_empty_regular_folder() {
    // seed folder + one conversation
    // delete_conversation_with_cleanup_core with a channel emitter
    // assert folder is_open false / not in list_open_folders
    // assert FolderChange::Close received (if using WebOnly broadcaster like other emit tests)
}
```

Mirror patterns in `delete_conversation_with_cleanup_core` existing tests and `emit_folder_upsert_broadcasts_on_folder_channel`.

- [ ] **Step 2: Run test — expect fail**

- [ ] **Step 3: Implement in `delete_conversation_with_cleanup_core`**

After chat-folder cleanup:

```rust
if let Some(folder_id) = folder_id {
    cleanup_chat_folder_for_deleted_conversation(conn, folder_id).await;
    match folder_service::close_folder_if_no_live_conversations(conn, folder_id).await {
        Ok(true) => crate::commands::folders::emit_folder_close(emitter, folder_id),
        Ok(false) => {}
        Err(e) => tracing::error!(
            "[conversations] empty-folder close after delete failed (folder {folder_id}): {e}"
        ),
    }
}
```

**Draft protection is client-only:** backend may close while a draft still targets the folder. Spec accepts this on pure backend paths; frontend Task 5 re-opens if a draft is retargeted back / restore. Prefer order: if user still has a draft on that folder in the same window, Task 5’s draft-close is the primary empty-path closer; backend close on delete is still required for “delete last chat and leave” without draft.

If product wants “delete last conversation but keep draft folder open”: frontend must **re-open** the folder when it still has a singleton draft pointing at `folderId` after receiving `close`. Add that guard in Task 5.

- [ ] **Step 4: Tests pass + commit**

```powershell
cargo test --features test-utils delete_last_conversation_closes -- --nocapture
git add src-tauri/src/commands/conversations.rs
git commit -m "feat(conversations): close empty regular folder after last delete"
```

---

### Task 5: Frontend draft protection + open ensures draft + restore re-open

**Files:**
- Modify: `src/stores/tab-store.ts` (`closeTab`, `openNewConversationTab` retarget path)
- Modify: `src/stores/app-workspace-store.ts` (`openFolder`, `openWorktreeFolder`, `addFolderToWorkspaceById` — ensure draft via tab store without circular import hell)
- Possibly: `src/components/conversations/sidebar-conversation-list.tsx`, `src/components/layout/new-folder-dropdown.tsx`, `src/components/layout/clone-dialog.tsx` if draft ensure is not centralized
- Test: `src/stores/tab-store` tests if present; else add focused unit tests with mocked `removeFolderFromWorkspace`

**Interfaces:**
- Consumes: `useAppWorkspaceStore.getState().removeFolderFromWorkspace` / conversations list for count
- Produces: invariant open empty folder ⇔ singleton draft targets it (or ≥1 live conversation)

**Draft singleton rules (existing):** `openNewConversationTab` reuses the single `conversationId == null` tab and retargets `folderId`.

- [ ] **Step 1: Helper (tab-store or small util)**

```ts
function maybeCloseEmptyFolder(folderId: number) {
  const ws = useAppWorkspaceStore.getState()
  const live = ws.conversations.filter(
    (c) => c.folder_id === folderId && /* not deleted — store only has live */
  )
  // If store uses a different field for soft-deleted, skip those.
  if (live.length > 0) return
  if (!ws.folders.some((f) => f.id === folderId)) return
  const draftsPointingHere = useTabStore
    .getState()
    .rawTabs.filter((t) => t.conversationId == null && t.folderId === folderId)
  if (draftsPointingHere.length > 0) return
  void ws.removeFolderFromWorkspace(folderId)
}
```

Call when:

1. `closeTab` removes a draft tab (`conversationId == null`) — after state update, `maybeCloseEmptyFolder(closingTab.folderId)`.
2. `openNewConversationTab` **retargets** draft from `oldFolderId` to `newFolderId` — after update, `maybeCloseEmptyFolder(oldFolderId)`.
3. **Do not** call when merely switching active tab among conversation tabs.

- [ ] **Step 2: Open folder ensures draft**

Centralize in store methods after successful open:

```ts
openFolder: async (path) => {
  const detail = await apiOpenFolder(path)
  // upsert + branch + refresh (existing)
  // Ensure draft — import tab store getState carefully (existing cross-store pattern in tab-store)
  useTabStore.getState().openNewConversationTab(detail.id, detail.path, {
    folderDefaultAgent: detail.default_agent_type,
    folderRecentAgent: detail.last_agent_type,
  })
  return detail
},
```

Same for `openWorktreeFolder` and `addFolderToWorkspaceById` if they leave the user on an empty folder without a draft. `use-switch-to-branch` already opens a draft — ensure no double-broken state (singleton retarget is fine).

Avoid circular import: if `app-workspace-store` cannot import `tab-store`, keep draft ensure at call sites (sidebar, dropdown, clone) **and** `WorkspaceOpenFolderListener` (already opens draft). Prefer a tiny mediator function in e.g. `src/lib/open-folder-with-draft.ts` both can call.

- [ ] **Step 3: On `folder://changed` close while draft still targets folder**

In app-workspace-context close handler **or** tab-store subscription:

```ts
// After dropFolderFromOpenList:
const draft = useTabStore.getState().rawTabs.find(
  (t) => t.conversationId == null && t.folderId === closedId
)
if (draft) {
  void useAppWorkspaceStore.getState().addFolderToWorkspaceById(closedId)
}
```

Prevents backend delete-last-conversation close from yanking a folder the user is still drafting in.

- [ ] **Step 4: Tab restore**

Where tabs are restored on load (search `rawTabs` hydration / `loadTabs` / remote tabs sync in `tab-store.ts`): for each restored draft with `conversationId == null`, if folder not in `folders`, `addFolderToWorkspaceById(folderId)`.

- [ ] **Step 5: Tests**

- Retarget draft A→B with A empty → `removeFolderFromWorkspace(A)` called.
- Close draft with empty folder → remove called.
- Close draft with folder that has conversations → remove **not** called.
- Open folder invokes draft open (mock).

- [ ] **Step 6: Commit**

```powershell
git add src/stores/tab-store.ts src/stores/app-workspace-store.ts src/lib/open-folder-with-draft.ts src/contexts/app-workspace-context.tsx src/**/*.test.ts*
git commit -m "feat(workspace): draft-guard empty folder open membership"
```

---

### Task 6: Import — do not leave empty open folders

**Files:**
- Modify: `src-tauri/src/commands/conversations.rs` batch import loop (~after `import_summaries_resilient`)
- Test: extend `batch_import_*` tests in same file

**Interfaces:**
- Consumes: `close_folder_if_no_live_conversations`, `emit_folder_close`

- [ ] **Step 1: Failing test**

Import path that creates/opens a folder but imports zero sessions (all skipped/failed) → folder not open afterward.

- [ ] **Step 2: After each successful `add_folder` + import tally**

```rust
if tally.imported + tally.updated == 0 {
    if folder_service::close_folder_if_no_live_conversations(conn, folder_id).await? {
        emit_folder_close(emitter, folder_id);
        // do not emit upsert for an empty open; if upsert already emitted, emit close after
    }
} else {
    // existing upsert emit
}
```

Order carefully: existing code emits upsert after every touch. Prefer: upsert only when staying open with conversations; if empty, close + emit close (and skip upsert or upsert then close — close must win for clients).

- [ ] **Step 3: Tests pass + commit**

```powershell
cargo test --features test-utils batch_import -- --nocapture
git commit -m "fix(import): close folders left empty after import"
```

---

### Task 7: Automation cancel-before-conversation + optional settle

**Files:**
- Modify: `src-tauri/src/automation/engine.rs` (cancel path ~lines 429–444 where worktree already minted)
- Test: automation tests if any cover cancel; else unit-test close helper invocation via extracting a small fn

**Interfaces:**
- Consumes: `close_folder_if_no_live_conversations`, `emit_folder_close`, `EventEmitter` on engine

- [ ] **Step 1: On cancel-before-spawn after worktree folder created**

```rust
if let Some(wt_id) = cwd.worktree_folder_id {
    let _ = automation_service::attach_run_runtime(/* ... */).await;
    if let Ok(true) =
        folder_service::close_folder_if_no_live_conversations(&self.db.conn, wt_id).await
    {
        emit_folder_close(&self.emitter, wt_id);
    }
    return Ok(());
}
```

- [ ] **Step 2: Prefer also on settle paths where conversation never created** (grep attach_run_runtime / failed launch). Keep disk worktree (non-goal). Startup reconcile covers stragglers.

- [ ] **Step 3: Commit**

```powershell
git commit -m "fix(automation): close empty per-run worktree folder when cancelled early"
```

---

### Task 8: Delegation ensure folder without forcing open

**Files:**
- Modify: `src-tauri/src/db/service/folder_service.rs` — `ensure_folder_path(conn, path, open: bool)` or `add_folder_unopened`
- Modify: `src-tauri/src/acp/manager.rs` (~5862) to use ensure that does **not** set `is_open = true` when only deriving `folder_id` for send; after conversation is created, folder can be opened if product needs sidebar placement (conversation upsert already needs folder known — `emit_folder_upsert` if conversation is visible)

**Interfaces:**
- Produces: `pub async fn ensure_folder(conn, path, open: bool) -> Result<FolderHistoryEntry, DbError>`
  - `open == true` → current `add_folder` behavior
  - `open == false` → insert with `is_open = false`, or on existing row **do not force open** (leave `is_open` as-is unless reopening deleted)

- [ ] **Step 1: Tests for ensure_folder open false**

- New path → row exists, `is_open == false`, not in `list_open_folders`
- Existing open path → stays open
- Existing closed path → stays closed

- [ ] **Step 2: Implement by generalizing `add_folder_inner`**

Avoid duplicating UNIQUE recovery logic — add an `OpenWrite { ForceOpen, PreserveOpen, ForceClosed }` or boolean `open_on_write`.

- [ ] **Step 3: Manager uses `ensure_folder(..., false)` then create/link conversation; if conversation is user-visible and folder should appear, `set_folder_open(true)` + `emit_folder_upsert` when conversation is created (send success path). If conversation is always created in same call path, open after create is fine.

- [ ] **Step 4: Commit**

```powershell
git commit -m "fix(delegation): register working_dir folders without bare open"
```

---

### Task 9: Verification sweep

**Files:** none new

- [ ] **Step 1: Backend**

```powershell
cd src-tauri
cargo test --features test-utils close_open_folders close_folder_if_no_live delete_last_conversation batch_import emit_folder
cargo clippy --all-targets --features test-utils -- -D warnings
```

- [ ] **Step 2: Frontend**

```powershell
pnpm exec vitest run src/contexts/app-workspace-context.test.tsx src/stores
pnpm eslint src/stores/tab-store.ts src/stores/app-workspace-store.ts src/contexts/app-workspace-context.tsx src/lib/types.ts
```

- [ ] **Step 3: Manual QA checklist**

1. Cold start with previously empty open folders → they are gone from sidebar.  
2. Open empty project → folder appears + new draft.  
3. Close draft without sending → folder leaves workspace.  
4. Open empty project, send message, close draft → folder remains.  
5. Delete last conversation in a folder (no draft) → folder leaves.  
6. Delete last conversation while draft still on that folder → folder stays (re-open guard).  
7. Import zero sessions for a path → no empty open folder.  
8. Automation cancel before conversation → worktree folder not stuck open.  
9. Chat mode folders still hidden; chat GC unchanged.

- [ ] **Step 4: Final commit** only if verification fixes were needed.

---

## Spec coverage checklist

| Spec requirement | Task |
|------------------|------|
| Core rule: open iff live convs or draft | 1, 4, 5 |
| Open empty project → appear + draft | 5 |
| Close last draft + zero convs → leave workspace | 5 |
| Startup reconcile | 3 |
| Skip chat kind | 1 |
| Import empty groups | 6 |
| Automation cancel-before-conversation | 7 |
| Branch/worktree = open + draft | 5 (`openWorktreeFolder` + switch hook) |
| Delegation no bare open | 8 |
| No disk/worktree delete | Global constraints |
| Multi-client close signal | 2 |
| Silent auto-close | 5, 4 |
| Draft restore re-open | 5 |

## Known v1 limitations (do not expand scope)

- Multi-window: two windows with drafts on different empty folders — singleton draft is per-window client; backend close on delete may race; re-open-on-draft guard mitigates same-window cases only.
- File-tree-only browsing without a draft is unsupported by product decision (open always creates draft).

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — execute tasks in this session with checkpoints  

Which approach?
