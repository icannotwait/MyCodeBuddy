# Empty Folder Workspace Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the workspace sidebar from long-lived open folders that have zero live conversations, while keeping path registration/history and open-project → chat flows working.

**Architecture:** Backend owns headless empty-close on zero live conversations (startup barrier reconcile, delete last conversation, empty import, automation early-exit, delegation RegistrationOnly). Frontend owns singleton-draft protection and leave transitions (including last-tab replacement order). Multi-client sync uses `FolderChange::Close { folder_id, cause }` (`AutoEmpty` | `UserRemove`) with fenced open-list refetch; AutoEmpty may re-open when local draft still targets the folder; UserRemove never re-opens and disposes the draft binding. Auto-close uses a **visibility-only conditional** transport (Tauri + Axum), never the user-remove cascade.

**Tech Stack:** Rust (SeaORM, Tauri commands, Axum handlers), TypeScript/React (Zustand stores, event subscriptions), Vitest + `cargo test --features test-utils`.

**Spec:** `docs/superpowers/specs/2026-07-27-empty-folder-workspace-visibility-design.md` (post design-review R4; approved)

## Global Constraints

- Workspace visibility only: set `is_open = false`; do **not** soft-delete history rows, delete disk paths, or `git worktree remove`.
- Apply auto-close only to `FolderKind::Regular`; skip `FolderKind::Chat` (chat-dir GC owns scratch lifecycle).
- Live conversation = row with `deleted_at` null for that `folder_id` (includes hidden delegation children / loops for close predicate — design v1).
- New-conversation drafts are a **client-side singleton** (`conversationId == null`, at most one tab) retargeted across folders — not per-folder draft rows; **not** persisted across restart.
- Auto-close is **silent** (no confirm dialog, no required toast).
- **Two close paths:** (1) User remove = cascade + `Close{UserRemove}`; (2) Empty auto-close = conditional visibility-only + `Close{AutoEmpty}` — no tab wipe / no office-watch stop / no disk touch.
- Explicit user remove remains allowed for non-empty folders and is sticky until explicit re-open.
- Startup empty-open reconcile is a **readiness barrier** (not fire-and-forget like chat GC).
- Required dual-runtime conditional-close API for frontend draft leave.
- No schema migration.
- Design deferred minors: last-tab unbind preference, fence store detail, refetch volume — implement conservatively.

---

## File map

| File | Responsibility |
|------|----------------|
| `src-tauri/src/db/service/folder_service.rs` | Count live convs; conditional close (bulk returns closed ids); `ensure_folder` RegistrationOnly \| ForceOpen |
| `src-tauri/src/web/event_bridge.rs` | `FolderChange::Close { folder_id, cause }` |
| `src-tauri/src/commands/folders.rs` | `emit_folder_close(cause)`; user remove → UserRemove; conditional-close command/core; Upsert on open-by-id |
| `src-tauri/src/web/handlers/` (folders) | Axum route for conditional close (same core) |
| `src-tauri/src/commands/conversations.rs` | After delete: maybe-close + AutoEmpty; import: close empty groups + Close-wins |
| `src-tauri/src/lib.rs` | Desktop: **await** empty-folder reconcile before workspace data ready |
| `src-tauri/src/bin/codeg_server.rs` | Server: **await** reconcile before serving open-folder APIs |
| `src-tauri/src/automation/engine.rs` | All early exits before conversation: close empty worktree + AutoEmpty |
| `src-tauri/src/acp/manager.rs` + `acp/delegation/broker.rs` | RegistrationOnly ensure; ForceOpen+Upsert after user-visible conv create |
| `src/lib/types.ts` | `FolderChange` close + `FolderCloseCause` |
| `src/lib/api.ts` / transport | Conditional-close client API `{ closed: boolean }` |
| `src/contexts/app-workspace-context.tsx` | Handle close + fenced refetch + cause-aware draft guard |
| `src/stores/app-workspace-store.ts` | drop open list; generation fence on fetch; open emits path stays thin |
| `src/stores/tab-store.ts` | Draft leave → conditional close; last-tab order; retarget leave |
| `src/lib/open-folder-with-draft.ts` (or equiv) | User-open choke point |
| Call sites | Route user opens through choke point |

---

### Task 1: Backend — close empty open regular folders

**Files:**
- Modify: `src-tauri/src/db/service/folder_service.rs`
- Test: same file `#[cfg(test)]` module (or extend existing tests at bottom of file if present; otherwise add tests next to other folder_service tests / use `crate::db::test_helpers`)

**Interfaces:**
- Produces:
  - `pub async fn count_live_conversations_for_folder(conn: &DatabaseConnection, folder_id: i32) -> Result<u64, DbError>`
  - `pub async fn close_folder_if_no_live_conversations(conn: &DatabaseConnection, folder_id: i32) -> Result<bool, DbError>` — returns `true` if it flipped `is_open` from true to false; no-op for missing, chat kind, already closed, or count > 0
  - `pub async fn close_open_folders_with_no_live_conversations(conn: &DatabaseConnection) -> Result<Vec<i32>, DbError>` — bulk reconcile; returns **closed folder ids** (caller derives count)

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

    let closed = folder_service::close_open_folders_with_no_live_conversations(&db.conn)
        .await
        .expect("reconcile");
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0], empty_id);

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
    let closed = folder_service::close_open_folders_with_no_live_conversations(&db.conn)
        .await
        .expect("reconcile");
    assert!(closed.is_empty());
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

Implementation sketch — **atomic conditional UPDATE only** (no count-then-set TOCTOU):

```rust
/// Returns true only when this statement flipped is_open true→false.
pub async fn close_folder_if_no_live_conversations(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<bool, DbError> {
    // Prefer raw SQL / SeaORM update with rows_affected == 1:
    // UPDATE folder SET is_open = 0, ...
    // WHERE id = ? AND deleted_at IS NULL AND kind = 'regular' AND is_open = 1
    //   AND NOT EXISTS (
    //     SELECT 1 FROM conversation c
    //     WHERE c.folder_id = folder.id AND c.deleted_at IS NULL
    //   )
    // Return rows_affected == 1. Never call set_folder_open after a separate count.
    todo!()
}

pub async fn count_live_conversations_for_folder(...) -> Result<u64, DbError> {
    // Optional diagnostics / import decision aid — NEVER used as the close guard alone.
}

pub async fn close_open_folders_with_no_live_conversations(
    conn: &DatabaseConnection,
) -> Result<Vec<i32>, DbError> {
    // Prefer set-based UPDATE ... RETURNING id / or select candidate ids then
    // call the same atomic primitive per id (still uses WHERE NOT EXISTS, not pre-count).
}
```

Tests must assert: concurrent live insert cannot win a successful close (or equivalent race hook); already-closed / missing / chat / deleted / non-empty → `false` without changing `deleted_at`.

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

### Task 2: FolderChange::Close + dual-runtime conditional-close + fenced store

**Files:**
- Modify: `src-tauri/src/web/event_bridge.rs` (`FolderChange` + `FolderCloseCause`)
- Modify: `src-tauri/src/commands/folders.rs` (`emit_folder_close`, remove→UserRemove, open cores gain emitter + Upsert, **register Tauri command** for conditional close)
- Modify: `src-tauri/src/lib.rs` (invoke handler registration for conditional close)
- Modify: `src-tauri/src/web/handlers/folders.rs` + `src-tauri/src/web/router.rs` (Axum route same core)
- Modify: `src/lib/types.ts`, `src/lib/api.ts` / transport (`closeFolderIfEmpty` → `{ closed: boolean }`)
- Modify: `src/stores/app-workspace-store.ts` — **folder-event generation fence** on `fetchFolders` / membership apply
- Modify: `src/contexts/app-workspace-context.tsx` (handle close + fenced refetch + cause-aware draft hooks stub)
- Test: context + store fence tests; Rust emit + conditional-close core

**Interfaces:**
- Consumes: Task 1 close helpers (optional at emit call sites later)
- Produces:
  - Rust:

```rust
#[serde(rename_all = "snake_case")]
pub enum FolderCloseCause { AutoEmpty, UserRemove }

// FolderChange tag kind = "close"
Close { folder_id: i32, cause: FolderCloseCause }
```

```ts
export type FolderCloseCause = "auto_empty" | "user_remove"
export type FolderChange =
  | { kind: "upsert"; folder: FolderDetail }
  | { kind: "close"; folder_id: number; cause: FolderCloseCause }
```

  - `pub(crate) fn emit_folder_close(emitter: &EventEmitter, folder_id: i32, cause: FolderCloseCause)`
  - **Required** command/handler: `close_folder_if_empty_core` → Tauri + Axum + TS `{ closed: boolean }`; emit AutoEmpty only when flipped.
  - User `remove_folder_from_workspace_core` must emit `Close{UserRemove}` after success.
  - Open/open-by-id paths that set `is_open=true` must emit Upsert (if not already).

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
  emitFolder({ kind: "close", folder_id: 12, cause: "auto_empty" })
  await waitFor(() => {
    expect(
      useAppWorkspaceStore.getState().folders.some((f) => f.id === 12)
    ).toBe(false)
  })
})
```

Also add: AutoEmpty + draft still targeting 12 → re-open; UserRemove + draft → no re-open / draft disposed.

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
        cause: FolderCloseCause,
    },
}

pub(crate) fn emit_folder_close(
    emitter: &EventEmitter,
    folder_id: i32,
    cause: FolderCloseCause,
) {
    crate::web::event_bridge::emit_event(
        emitter,
        crate::web::event_bridge::FOLDER_CHANGED_EVENT,
        crate::web::event_bridge::FolderChange::Close { folder_id, cause },
    );
}
```

Call `emit_folder_close(..., UserRemove)` at the end of `remove_folder_from_workspace_core`. Auto-close paths use `AutoEmpty`.

**Fenced membership (store — required before handler is correct):**

- Monotonic `folderEventGeneration` (or latest-request id) advanced on every Close/Upsert apply, user open, reconnect, and before each open-list refetch.
- `fetchFolders` captures generation at start; **discard** response if generation advanced before commit (do not blindly replace). Retry optional.
- Serialize Close / Upsert / refetch application through one path.
- After non-stale refetch, re-apply AutoEmpty draft re-open guard if draft still targets a missing folder.
- Tests (deferred promises): Close during in-flight refetch; Upsert during in-flight refetch; stale Close after newer open; overlapping refetches; reconnect fence; AutoEmpty guard after closed snapshot.

**Close handler:**

```ts
// 1) local drop open membership only (no re-API close). v1: do NOT prune all-history branch cache.
// 2) bump folderEventGeneration
// 3) cause=user_remove → tab-store dispose/retarget draft (Task 5) — never re-open
// 4) cause=auto_empty + draft targets → schedule silent re-open
// 5) fenced refetch; re-apply AutoEmpty guard after non-stale
```

```ts
dropFolderFromOpenList: (folderId: number) => {
  set({ folders: get().folders.filter((f) => f.id !== folderId) })
  // keep branches (all-history default per design)
},
```

**Dual-runtime steps (explicit):**

1. `close_folder_if_empty_core(conn, emitter, folder_id) -> bool` — atomic service + emit AutoEmpty if true.
2. Tauri `#[tauri::command]` + register in `lib.rs`.
3. Axum handler + router path (mirror other folder routes).
4. TS `api.closeFolderIfEmpty(folderId): Promise<{ closed: boolean }>`.
5. Open/open-by-id/worktree open wrappers pass `emitter` and emit Upsert when membership becomes open.

Cause-aware draft dispose/re-open may stub-call tab store; full last-tab semantics complete in Task 5.

- [ ] **Step 4: Surface tests (required — not only core)**

- Tauri: command registration / wrapper test (or existing command-surface pattern) for conditional-close name + `{ closed }`.
- Axum: integration via `build_router` pattern (`src-tauri/tests/api_integration.rs` style) — route, request body, HTTP status, `{ "closed": true|false }`, DB `is_open`, emit AutoEmpty **only** on flip.
- TS: transport/api test — command name, payload, typed `{ closed: boolean }` for true and false.
- Core: emit once only on successful flip; no emit when non-empty/already-closed.

```powershell
pnpm exec vitest run src/contexts/app-workspace-context.test.tsx src/stores src/lib
cargo test --features test-utils emit_folder -- --nocapture
cargo test --features test-utils --test api_integration -- --nocapture
```

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/web/event_bridge.rs src-tauri/src/commands/folders.rs src-tauri/src/lib.rs src-tauri/src/web/handlers/folders.rs src-tauri/src/web/router.rs src/lib/types.ts src/lib/api.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx src/stores/app-workspace-store.ts
git commit -m "feat(folders): broadcast workspace close on folder://changed"
```

---

### Task 3: Startup reconcile with readiness barrier (desktop + server)

**Files:**
- Modify: `src-tauri/src/lib.rs` (desktop setup — **await** before workspace data / first open-folder fetch, not fire-and-forget like chat GC)
- Modify: `src-tauri/src/bin/codeg_server.rs` (**await** after DB ready, **before** serving routes that return open folders)
- Thin wrapper in `commands/folders.rs` if useful: `reconcile_empty_open_folders_core`

**Interfaces:**
- Consumes: `folder_service::close_open_folders_with_no_live_conversations` → `Vec<i32>`
- Produces: startup side effect; optional emit AutoEmpty Close if clients may already be connected (server)

- [ ] **Step 1: Desktop — exact placement**

In `src-tauri/src/lib.rs`, immediately after DB readiness `block_on` path (~339–345 area) and **before** main webview creation (~901+), run:

```rust
// block_on / sequential await — NOT spawn-and-forget like chat-dir GC
match folder_service::close_open_folders_with_no_live_conversations(&conn).await {
    Ok(ids) if !ids.is_empty() => tracing::info!("[folders] empty-open reconcile: closed {}", ids.len()),
    Ok(_) => {}
    Err(err) => tracing::error!("[folders] empty-open reconcile failed: {err}"), // degrade; do not crash
}
```

- [ ] **Step 2: Server — exact placement**

In `codeg_server.rs`, after DB init (~191–194) and **before** AppState/router/listener bind (~591–602), `await` the same reconcile. Chat GC may stay background.

- [ ] **Step 3: Sequencing proof**

Prefer a unit/integration test that reconcile helper runs to completion and subsequent `list_open_folder_details` has no empty regular opens. `cargo check` alone is insufficient for the barrier claim.

```powershell
cargo check
cargo check --no-default-features --bin codeg-server
```

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
        Ok(true) => crate::commands::folders::emit_folder_close(
            emitter,
            folder_id,
            FolderCloseCause::AutoEmpty,
        ),
        Ok(false) => {}
        Err(e) => tracing::error!(
            "[conversations] empty-folder close after delete failed (folder {folder_id}): {e}"
        ),
    }
}
```

**Draft protection is client-only:** backend closes on zero live count. Frontend Task 5/2 handler re-opens only on `cause=auto_empty` when singleton draft still targets the folder (fenced). No cold-start draft restore.

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
- Test: `src/stores/tab-store` tests if present; else add focused unit tests with mocked conditional-close API

**Interfaces:**
- Consumes: conditional-close transport + conversations list for count
- Produces: invariant open empty folder ⇔ singleton draft targets it (or ≥1 live conversation)

**Draft singleton rules (existing):** `openNewConversationTab` reuses the single `conversationId == null` tab and retargets `folderId`.

- [ ] **Step 1: Helper (tab-store or small util)**

```ts
**Last-tab ordering (synchronous local first — required):**

`closeTab` is currently synchronous and immediately calls `makeReplacementDraftTab`. Do **not** wait for network before replacement selection.

```ts
// When closing a draft for folder F with zero local live conversations:
// 1. Remove draft tab from state (as today).
// 2. Optimistically drop F from open list (local) BEFORE makeReplacementDraftTab.
// 3. Select replacement draft against UPDATED folders (must not rebind F).
// 4. Fire apiCloseFolderIfEmpty(F) and apply result table below.
// Tests: control deferred API promise; while in flight, replacement must not bind F.
```

**Draft-leave API result table (all leave paths, including last-tab):**

| Result | Client action |
|--------|----------------|
| `closed: true` | Idempotently drop F from open list (if not already); optional fenced refetch. |
| `closed: false` | F non-empty / already closed / concurrent change — **fenced membership refetch**; do **not** recreate a draft solely because close was false. |
| transport/error | Fenced reconciliation; if non-stale result says F still open **and** leave predicate still holds (zero local live, no draft on F), **retry** conditional close once; **never** re-open F just to compensate. |

Last-tab: keep F excluded during replacement selection; restore membership only when authoritative open list requires it (e.g. live conv appeared); must not rebind replacement draft to F.

```ts
async function maybeCloseEmptyFolder(folderId: number) {
  // pre-check only; call apiCloseFolderIfEmpty — NEVER user-remove cascade
  // apply result table above for true / false / error
}
```

Tests: `closed:true` with suppressed event still drops; `false`; rejected request; stale completion after newer folder event.

Call when:

1. Draft close / last-tab path (order above).
2. Retarget A→B after commit → maybeCloseEmptyFolder(A).
3. close-other/all, chat retarget, detach paths that leave a folder without draft.
4. **Do not** on mere active-tab switch.

- [ ] **Step 2: User-intent open+draft choke point (not low-level store)**

Add `src/lib/open-folder-with-draft.ts` (or equivalent mediator):

```ts
export async function openFolderWithDraft(path: string) {
  const detail = await useAppWorkspaceStore.getState().openFolder(path) // silent membership
  useTabStore.getState().openNewConversationTab(detail.id, detail.path, { ... })
  return detail
}
```

**User-intent call sites** (must use mediator): sidebar history open, new-folder-dropdown, workspace chrome open, clone/project-boot that leaves empty regular folder, WorkspaceOpenFolderListener, use-switch-to-branch (if not already correct).

**Keep silent (no draft):** low-level `openFolder` / `openWorktreeFolder` / `addFolderToWorkspaceById` used by deep-link, pet-focus, system registration. Tests: user open ensures one draft; deep-link membership open does **not** create/focus draft.

- [ ] **Step 3: On `folder://changed` close (cause-aware)** — implemented primarily in Task 2 handler; Task 5 ensures tab-store dispose/retarget for `user_remove` and last-tab order.

- [ ] **Step 4: Cold start**

Drafts are **not** in `opened_tabs`. **Do not** invent draft restore-on-load. Startup barrier + no draft is enough.

- [ ] **Step 5: Tests**

- Retarget draft A→B with A empty → conditional-close API for A called (not user-remove cascade).
- Close draft with empty folder → conditional-close called.
- Close draft with folder that has conversations → not called.
- Last-tab close sole empty folder → closes F; replacement draft does not rebind F.
- Open folder invokes draft open (mock).
- AutoEmpty close with draft → re-open; UserRemove with draft → no re-open.

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
// Prefer authoritative live count, not only tally.
if folder_service::count_live_conversations_for_folder(conn, folder_id).await? == 0 {
    if folder_service::close_folder_if_no_live_conversations(conn, folder_id).await? {
        emit_folder_close(emitter, folder_id, FolderCloseCause::AutoEmpty);
        // Close must be emitted AFTER any Upsert for this folder in the same batch
    }
} else {
    // existing upsert emit for open-with-conversations
}
```

- [ ] **Step 3: Tests pass + commit**

```powershell
cargo test --features test-utils batch_import -- --nocapture
git commit -m "fix(import): close folders left empty after import"
```

---

### Task 7: Automation pre-conversation exits → empty-close

**Files:**
- Modify: `src-tauri/src/automation/engine.rs` after `resolve_cwd` opens worktree (~395–484)
- Extract small helper e.g. `close_empty_worktree_folder_if_needed(db, emitter, worktree_folder_id)`

**Interfaces:**
- After folder opened and **before** live conversation: every exit path uses the helper (launch-input fail, disabled/missing agent, concurrent cancel, spawn fail, conversation insert fail, settle with no conversation).

- [ ] **Step 1: Helper**

```rust
async fn close_empty_worktree_folder_if_needed(...) {
    if let Some(wt_id) = worktree_folder_id {
        if close_folder_if_no_live_conversations(conn, wt_id).await? {
            emit_folder_close(emitter, wt_id, FolderCloseCause::AutoEmpty);
        }
    }
}
```

- [ ] **Step 2: Call from every pre-conversation exit** (single boundary preferred). Keep disk worktree.

- [ ] **Step 3: Tests** — inject cancel / agent fail / spawn fail / insert fail; assert `is_open=false`, disk worktree remains, AutoEmpty emitted when flipped.

```powershell
git commit -m "fix(automation): close empty per-run worktree folder on early exits"
```

---

### Task 8: Delegation ensure folder without forcing open

**Files:**
- Modify: `src-tauri/src/db/service/folder_service.rs` — `ensure_folder(conn, path, mode: RegistrationOnly | ForceOpen)`
- Modify: `src-tauri/src/acp/manager.rs` (~5862) **and** `src-tauri/src/acp/delegation/broker.rs` reserve path (~5539)

**Interfaces:**
- `RegistrationOnly`: existing live row **preserve** `is_open`; new/revived row `is_open = false` (do not masquerade as user open timestamps if avoidable)
- `ForceOpen`: current `add_folder` open behavior + Upsert when used from open commands

- [ ] **Step 1: Tests for RegistrationOnly**

- New path → `is_open == false`
- Existing open → **preserve open**
- Existing closed → stay closed
- Soft-deleted/revived → revive **closed** (not user-open timestamps masquerade)
- Concurrent unique-path race still recovers (existing UNIQUE recovery)
- Hidden delegation child create (manager + broker) does **not** ForceOpen
- Only an explicitly **user-visible top-level** conversation path (if any on these call sites) ForceOpen+Upsert — if both sites only create hidden children, document **no ForceOpen on child create**; open only when product already surfaces the folder another way

- [ ] **Step 2: Implement by generalizing `add_folder_inner`** with `RegistrationOnly | ForceOpen`.

- [ ] **Step 3:** Manager + broker → RegistrationOnly; never ForceOpen solely because a hidden child conversation row exists.

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
# Cargo accepts one TESTNAME filter per invocation — use full suite or separate runs
cargo test --features test-utils
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

Negative side-effect tests (must exist from Tasks 1–2): AutoEmpty does not delete tabs/stop watches/soft-delete/disk; UserRemove still cascades and emits UserRemove for non-empty.

- [ ] **Step 2: Frontend**

```powershell
pnpm test
pnpm eslint src/stores/tab-store.ts src/stores/app-workspace-store.ts src/contexts/app-workspace-context.tsx src/lib/types.ts src/lib/api.ts
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
| Mid-session AutoEmpty re-open (not cold draft restore) | 2, 5 |
| Atomic conditional close | 1 |
| Dual-runtime conditional-close API | 2 |
| Fenced refetch | 2 |
| Startup barrier placement | 3 |
| User-intent open+draft choke point | 5 |

## Known v1 limitations (do not expand scope)

- Multi-window: drafts are per-window; backend AutoEmpty close may race; same-window re-open guard only; UserRemove is sticky across clients.
- File-tree-only browsing without a draft is unsupported (open always creates draft).
- Live count includes hidden children — may leave visually sparse folders open.
- Parent folder with only worktree-child conversations may auto-close (per-folder rule).

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-empty-folder-workspace-visibility.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — execute tasks in this session with checkpoints  

Which approach?
