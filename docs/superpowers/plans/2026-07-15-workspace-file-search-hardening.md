# Workspace File Tree and Search Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in, globally persisted show-ignored file-tree mode while keeping search ignore-aware, cancellable, query-fresh, and Unicode case-insensitive.

**Architecture:** The existing workspace-state stream remains the default pruned tree source. A focused frontend hook overlays a one-shot include-ignored tree only when requested, while a separate search hook owns debounce, request identity, and stale-result suppression. Rust adds an optional tree mode plus a request-scoped cancellation registry and a four-walk semaphore shared by Tauri and Axum.

**Tech Stack:** Rust 2021, Tokio, `tokio-util::CancellationToken`, `ignore`, Tauri 2, Axum, React 19, TypeScript strict mode, Vitest, next-intl.

## Global Constraints

- `Show ignored files` is global across workspaces, persisted at `workspace:file-tree-show-ignored`, and defaults to `false`.
- Search always honors `.gitignore`, `.ignore`, `.rgignore`, Git global/exclude rules, and current hard exclusions.
- Show-ignored mode uses one-shot `get_file_tree` requests; it must not create a second workspace-state stream.
- `.git` and `__pycache__` directories, `.DS_Store` files, and symlink traversal retain current behavior.
- Search identity is a stable `searchSessionId` plus a fresh per-query `requestId`; partial, empty, or over-128-byte IDs are invalid.
- At most four blocking workspace search walks may run at once, including detached tasks winding down after cancellation.
- Every production behavior starts with a focused failing test and a confirmed RED result.
- Frontend files use no semicolons, two spaces, trailing commas, and the `@/*` alias. Rust remains compatible with desktop and `--no-default-features` server builds.

---

## File Map

- `src-tauri/src/commands/folders.rs`: tree-mode walk configuration, search registration lease, cancellation, semaphore, Unicode matching, Rust unit tests.
- `src-tauri/src/web/handlers/folders.rs`: camelCase JSON parameters and cancel handler.
- `src-tauri/src/web/router.rs`: web cancel route.
- `src-tauri/src/lib.rs`: Tauri cancel command registration.
- `src/lib/api.ts`: transport-neutral tree/search/cancel API.
- `src/lib/tauri.ts`: direct Tauri wrappers kept in parity.
- `src/lib/workspace-file-api.test.ts`: transport payload contract tests.
- `src/components/chat/composer/use-reference-search.ts`: stable reference-search session and abort-driven explicit cancellation.
- `src/components/chat/composer/use-reference-search.test.ts`: composer identity/cancellation regression tests.
- `src/hooks/use-workspace-file-search.ts`: command-dialog file search lifecycle.
- `src/hooks/use-workspace-file-search.test.ts`: fake-timer freshness and cancellation tests.
- `src/lib/file-tree-display-prefs.ts`: global preference storage and reactive hook.
- `src/lib/file-tree-display-prefs.test.ts`: default, persistence, and event synchronization tests.
- `src/hooks/use-ignored-file-tree.ts`: one-shot ignored-tree overlay, sequence fallback, and refresh coalescing.
- `src/hooks/use-ignored-file-tree.test.ts`: overlay lifecycle and error tests.
- `src/components/ui/context-menu.tsx`: native checkbox item wrapper.
- `src/components/layout/aux-panel-file-tree-tab.tsx`: overlay rendering, lazy mode generation, checkbox wiring, and toasts.
- `src/components/layout/aux-panel-file-tree-tab-source.test.ts`: minimal integration wiring assertions.
- `src/hooks/use-file-tree.test.ts`: pure ignore-name and lazy-generation helper tests if helpers remain colocated there.
- `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`: `showIgnoredFiles` and load-failure copy.

---

### Task 1: Add the Include-Ignored Tree Contract

**Files:**
- Modify: `src-tauri/src/commands/folders.rs:2771-3045,3484-3498,4914-5100`
- Modify: `src-tauri/src/web/handlers/folders.rs:252-284`

**Interfaces:**
- Produces: `build_file_tree_sync(root: PathBuf, max_depth: usize, include_ignored: bool)`
- Produces: `get_file_tree(path: String, max_depth: Option<usize>, include_ignored: Option<bool>)`
- Preserves: omitted `includeIgnored` means `false`

- [ ] **Step 1: Write the failing Rust tree-mode tests**

Add one fixture assertion that exercises both modes and update existing callers to state the old default explicitly:

```rust
#[test]
fn build_file_tree_can_include_ignored_entries() {
    let root = tempfile::tempdir().expect("tempdir");
    write_tree_fixture(&root.path().join(".gitignore"), "dist/\n");
    write_tree_fixture(&root.path().join("dist/bundle.js"), "bundle\n");
    write_tree_fixture(&root.path().join("src/main.ts"), "main\n");

    let hidden = build_file_tree_sync(root.path().to_path_buf(), usize::MAX, false)
        .expect("pruned tree");
    let shown = build_file_tree_sync(root.path().to_path_buf(), usize::MAX, true)
        .expect("full tree");
    let mut hidden_paths = Vec::new();
    let mut shown_paths = Vec::new();
    collect_tree_paths(&hidden, &mut hidden_paths);
    collect_tree_paths(&shown, &mut shown_paths);

    assert!(!hidden_paths.iter().any(|path| path.starts_with("dist")));
    assert!(shown_paths.iter().any(|path| path == "dist/bundle.js"));
}

#[tokio::test]
async fn get_file_tree_defaults_to_pruned_mode() {
    let root = tempfile::tempdir().expect("tempdir");
    write_tree_fixture(&root.path().join(".ignore"), "generated/\n");
    write_tree_fixture(&root.path().join("generated/out.txt"), "out\n");

    let tree = get_file_tree(
        root.path().to_string_lossy().into_owned(),
        Some(10),
        None,
    )
    .await
    .expect("tree");
    let mut paths = Vec::new();
    collect_tree_paths(&tree, &mut paths);
    assert!(!paths.iter().any(|path| path.starts_with("generated")));
}
```

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils build_file_tree_can_include_ignored_entries
cargo test --features test-utils get_file_tree_defaults_to_pruned_mode
```

Expected: Cargo rejects the extra boolean/optional argument because the new signatures do not exist yet.

- [ ] **Step 3: Implement the minimal builder and command parameters**

Parameterize the shared walker instead of duplicating tree construction:

```rust
fn workspace_walk_builder(
    root: &Path,
    max_depth: Option<usize>,
    respect_ignores: bool,
) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }
    builder
        .hidden(false)
        .follow_links(false)
        .git_ignore(respect_ignores)
        .git_global(respect_ignores)
        .git_exclude(respect_ignores)
        .ignore(respect_ignores)
        .parents(respect_ignores)
        .require_git(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                !FILE_TREE_IGNORED_DIRS.contains(&name.as_ref())
            } else {
                name != ".DS_Store"
            }
        });
    if respect_ignores {
        builder.add_custom_ignore_filename(".rgignore");
    }
    builder
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_file_tree(
    path: String,
    max_depth: Option<usize>,
    include_ignored: Option<bool>,
) -> Result<Vec<FileTreeNode>, AppCommandError> {
    let root = PathBuf::from(&path);
    let depth = max_depth.unwrap_or(usize::MAX);
    let include_ignored = include_ignored.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        build_file_tree_sync(root, depth, include_ignored)
    })
    .await
    .map_err(|error| {
        AppCommandError::io_error("File tree walk task failed")
            .with_detail(error.to_string())
    })?
}
```

In the existing `build_file_tree_sync`, add the `include_ignored: bool`
parameter and replace its inline `WalkBuilder` configuration with
`workspace_walk_builder(&root, Some(max_depth), !include_ignored)`. Preserve
the current walk loop, node map construction, parent/child assembly, sorting,
and root return statements beginning at `for result in builder.build()`.

Add `include_ignored: Option<bool>` to `GetFileTreeParams`, pass it through the
Axum handler, and update all internal Rust call sites with `None`.

- [ ] **Step 4: Run tree tests and both Rust compile modes**

Run:

```powershell
cd src-tauri
cargo test --features test-utils build_file_tree
cargo test --features test-utils get_file_tree_async
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; ignored entries appear only in the explicit include mode.

- [ ] **Step 5: Commit the backend tree contract**

```powershell
git add src-tauri/src/commands/folders.rs src-tauri/src/web/handlers/folders.rs
git commit -m "feat(files): add include-ignored tree mode"
```

---

### Task 2: Make Workspace Search Unicode-Aware, Cancellable, and Bounded

**Files:**
- Modify: `src-tauri/src/commands/folders.rs:1-24,2771-2905,3500-3517,5053-5110`
- Modify: `src-tauri/src/web/handlers/folders.rs:266-284`
- Modify: `src-tauri/src/web/router.rs:230-240`
- Modify: `src-tauri/src/lib.rs:940-955`

**Interfaces:**
- Produces: `search_workspace_files(path: String, query: Option<String>, limit: Option<usize>, search_session_id: Option<String>, request_id: Option<String>)`
- Produces: `cancel_workspace_file_search(search_session_id: String, request_id: String) -> Result<bool, AppCommandError>`
- Produces: a four-permit search gate whose owned permit lives inside the blocking closure

- [ ] **Step 1: Write failing validation, replacement, cancellation, gate, and Unicode tests**

Add tests around public helpers rather than sleeping against real filesystem timing:

```rust
#[test]
fn workspace_search_identity_requires_a_complete_valid_pair() {
    assert!(validate_workspace_search_identity(None, None).unwrap().is_none());
    assert!(validate_workspace_search_identity(Some("session"), None).is_err());
    assert!(validate_workspace_search_identity(None, Some("request")).is_err());
    assert!(validate_workspace_search_identity(Some(""), Some("request")).is_err());
    let long = "x".repeat(129);
    assert!(validate_workspace_search_identity(Some(&long), Some("request")).is_err());
}

#[test]
fn replacement_and_delayed_cancel_cannot_cross_request_ids() {
    let first = register_workspace_search("session", "request-1").unwrap();
    let second = register_workspace_search("session", "request-2").unwrap();
    assert!(first.token().is_cancelled());
    assert!(!second.token().is_cancelled());
    assert!(!cancel_workspace_search_registration("session", "request-1"));
    assert!(!second.token().is_cancelled());
    assert!(cancel_workspace_search_registration("session", "request-2"));
    assert!(second.token().is_cancelled());
}

#[test]
fn workspace_search_matches_unicode_case() {
    let root = tempfile::tempdir().expect("tempdir");
    write_tree_fixture(&root.path().join("Ä.TXT"), "x\n");
    let token = CancellationToken::new();
    for query in ["ä", "Ä"] {
        let result = search_workspace_files_sync(
            root.path().to_path_buf(),
            query,
            10,
            &token,
        )
        .expect("search");
        assert_eq!(result.files[0].path, "Ä.TXT");
    }
}
```

For the concurrency test, pass a test semaphore into `run_workspace_search_task`, start five closures that increment `active`, block on a shared release signal, and assert `max_active == 4` before releasing them. Cancel a task waiting for a permit and assert its closure never runs.

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `cd src-tauri && cargo test --features test-utils workspace_search_`

Expected: missing identity, registration, cancel, token-aware search, and gate helpers cause compile failures.

- [ ] **Step 3: Implement request identity and the RAII registration lease**

Use pointer identity on `Arc<CancellationToken>` so reusing a request string cannot let an old lease remove a replacement:

```rust
use std::sync::{Arc, LazyLock, Mutex};
use tokio_util::sync::CancellationToken;

const WORKSPACE_FILE_SEARCH_MAX_CONCURRENT_OPS: usize = 4;
const WORKSPACE_FILE_SEARCH_ID_MAX_BYTES: usize = 128;

#[derive(Clone)]
struct WorkspaceSearchRegistration {
    request_id: String,
    token: Arc<CancellationToken>,
}

static WORKSPACE_SEARCH_REGISTRY: LazyLock<
    Mutex<HashMap<String, WorkspaceSearchRegistration>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static WORKSPACE_SEARCH_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(
        WORKSPACE_FILE_SEARCH_MAX_CONCURRENT_OPS,
    )));

struct WorkspaceSearchLease {
    session_id: Option<String>,
    request_id: Option<String>,
    token: Arc<CancellationToken>,
}

impl Drop for WorkspaceSearchLease {
    fn drop(&mut self) {
        self.token.cancel();
        let (Some(session_id), Some(request_id)) =
            (&self.session_id, &self.request_id)
        else {
            return;
        };
        let Ok(mut registry) = WORKSPACE_SEARCH_REGISTRY.lock() else {
            return;
        };
        let should_remove = registry.get(session_id).is_some_and(|current| {
            current.request_id == *request_id
                && Arc::ptr_eq(&current.token, &self.token)
        });
        if should_remove {
            registry.remove(session_id);
        }
    }
}
```

`register_workspace_search` must insert the new registration under one mutex lock, then cancel the replaced token outside the lock. `cancel_workspace_search_registration` must remove only a matching request ID and cancel the removed token after releasing the lock.

- [ ] **Step 4: Implement the cancellable gate and walker**

```rust
async fn run_workspace_search_task<F>(
    semaphore: Arc<Semaphore>,
    token: Arc<CancellationToken>,
    task: F,
) -> Result<WorkspaceFileSearchResult, AppCommandError>
where
    F: FnOnce(&CancellationToken) -> Result<WorkspaceFileSearchResult, AppCommandError>
        + Send
        + 'static,
{
    let permit = tokio::select! {
        _ = token.cancelled() => return Ok(WorkspaceFileSearchResult::empty()),
        permit = semaphore.acquire_owned() => permit.map_err(|error| {
            AppCommandError::task_execution_failed(error.to_string())
        })?,
    };
    if token.is_cancelled() {
        return Ok(WorkspaceFileSearchResult::empty());
    }
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        task(&token)
    })
    .await
    .map_err(|error| {
        AppCommandError::io_error("Workspace file search task failed")
            .with_detail(error.to_string())
    })?
}
```

Pass a token into `search_workspace_files_sync`, return `WorkspaceFileSearchResult::empty()` whenever it is cancelled, and change both candidate expressions from `to_ascii_lowercase()` to `to_lowercase()`.

- [ ] **Step 5: Add the public cancel command and both runtime routes**

Add optional identity fields to `SearchWorkspaceFilesParams`, add a required `CancelWorkspaceFileSearchParams`, register `/cancel_workspace_file_search`, and register the Tauri command in `src-tauri/src/lib.rs`.

```rust
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn cancel_workspace_file_search(
    search_session_id: String,
    request_id: String,
) -> Result<bool, AppCommandError> {
    let Some((session_id, request_id)) = validate_workspace_search_identity(
        Some(search_session_id.as_str()),
        Some(request_id.as_str()),
    )? else {
        unreachable!("cancel always supplies both identifiers");
    };
    Ok(cancel_workspace_search_registration(
        &session_id,
        &request_id,
    ))
}
```

- [ ] **Step 6: Run backend tests and compile all affected binaries**

Run:

```powershell
cd src-tauri
cargo test --features test-utils workspace_search_
cargo test --features test-utils search_workspace_files
cargo check
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
```

Expected: all commands exit 0; concurrency instrumentation never observes five active closures.

- [ ] **Step 7: Commit backend search hardening**

```powershell
git add src-tauri/src/commands/folders.rs src-tauri/src/web/handlers/folders.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs
git commit -m "fix(files): cancel obsolete workspace searches"
```

---

### Task 3: Expose Search Identity and Cancellation to Frontend Callers

**Files:**
- Modify: `src/lib/api.ts:2733-2774`
- Modify: `src/lib/tauri.ts:1074-1090`
- Create: `src/lib/workspace-file-api.test.ts`
- Modify: `src/components/chat/composer/use-reference-search.ts:1-400`
- Modify: `src/components/chat/composer/use-reference-search.test.ts:283-520`

**Interfaces:**
- Produces: `WorkspaceFileSearchIdentity`
- Produces: `searchWorkspaceFiles(path, query, limit, identity?)`
- Produces: `cancelWorkspaceFileSearch(identity)`
- Updates: composer search creates one stable session ID and one request ID per call

- [ ] **Step 1: Write failing API payload tests**

Mock `getTransport().call`, invoke all three APIs, and assert exact payloads:

```typescript
it("serializes tree mode and search identity", async () => {
  await getFileTree("/repo", 2, true)
  expect(call).toHaveBeenLastCalledWith("get_file_tree", {
    path: "/repo",
    maxDepth: 2,
    includeIgnored: true,
  })

  const identity = {
    searchSessionId: "session-1",
    requestId: "request-1",
  }
  await searchWorkspaceFiles("/repo", "foo", 50, identity)
  expect(call).toHaveBeenLastCalledWith("search_workspace_files", {
    path: "/repo",
    query: "foo",
    limit: 50,
    ...identity,
  })

  await cancelWorkspaceFileSearch(identity)
  expect(call).toHaveBeenLastCalledWith(
    "cancel_workspace_file_search",
    identity
  )
})
```

- [ ] **Step 2: Run the API test and confirm RED**

Run: `pnpm exec vitest run src/lib/workspace-file-api.test.ts`

Expected: the new parameters and cancel export are absent.

- [ ] **Step 3: Implement transport-neutral and direct-Tauri wrappers**

```typescript
export interface WorkspaceFileSearchIdentity {
  searchSessionId: string
  requestId: string
}

export async function getFileTree(
  path: string,
  maxDepth?: number,
  includeIgnored = false
): Promise<FileTreeNode[]> {
  return getTransport().call("get_file_tree", {
    path,
    maxDepth: maxDepth ?? null,
    includeIgnored,
  })
}

export async function searchWorkspaceFiles(
  path: string,
  query = "",
  limit = 50,
  identity?: WorkspaceFileSearchIdentity
): Promise<WorkspaceFileSearchResult> {
  return getTransport().call("search_workspace_files", {
    path,
    query,
    limit,
    searchSessionId: identity?.searchSessionId ?? null,
    requestId: identity?.requestId ?? null,
  })
}

export async function cancelWorkspaceFileSearch(
  identity: WorkspaceFileSearchIdentity
): Promise<boolean> {
  return getTransport().call("cancel_workspace_file_search", identity)
}
```

Mirror these signatures in `src/lib/tauri.ts` with `invoke`.

- [ ] **Step 4: Write failing composer cancellation tests**

Extend the existing hoisted API mocks with `cancelWorkspaceFileSearch` and mock `randomUUID()` as `session-1`, `request-1`, `request-2`. Assert that repeated calls use one session, fresh requests, and signal abort cancels the matching pair.

```typescript
expect(mocks.searchWorkspaceFiles).toHaveBeenNthCalledWith(
  1,
  "/repo",
  "a",
  50,
  { searchSessionId: "session-1", requestId: "request-1" }
)
controller.abort()
expect(mocks.cancelWorkspaceFileSearch).toHaveBeenCalledWith({
  searchSessionId: "session-1",
  requestId: "request-1",
})
```

- [ ] **Step 5: Run the composer test and confirm RED**

Run: `pnpm exec vitest run src/components/chat/composer/use-reference-search.test.ts`

Expected: current calls have no identity and abort does not invoke cancellation.

- [ ] **Step 6: Implement composer request lifecycle**

Use `randomUUID()` lazily for the stable session, create a request ID per file-search call, attach a one-shot abort listener, and remove it in `finally`. Keep an active identity ref so path/enable/unmount effects can explicitly cancel the matching request. Clear the ref only when it still equals the completing identity.

- [ ] **Step 7: Run frontend API and composer tests**

Run: `pnpm exec vitest run src/lib/workspace-file-api.test.ts src/components/chat/composer/use-reference-search.test.ts`

Expected: all tests pass with no unhandled promise rejections.

- [ ] **Step 8: Commit frontend search transport wiring**

```powershell
git add src/lib/api.ts src/lib/tauri.ts src/lib/workspace-file-api.test.ts src/components/chat/composer/use-reference-search.ts src/components/chat/composer/use-reference-search.test.ts
git commit -m "fix(files): propagate workspace search cancellation"
```

---

### Task 4: Extract Query-Fresh Command-Dialog File Search

**Files:**
- Create: `src/hooks/use-workspace-file-search.ts`
- Create: `src/hooks/use-workspace-file-search.test.ts`
- Modify: `src/components/conversations/search-command-dialog.tsx:1-170,337-369`

**Interfaces:**
- Produces: `useWorkspaceFileSearch({ folderPath, query, enabled, limit, debounceMs })`
- Returns: `{ files: FlatFileEntry[]; loading: boolean }`

- [ ] **Step 1: Write failing fake-timer hook tests**

Cover stale hiding before effects settle, stable/distinct sessions, fresh request IDs, stale promise rejection, and cleanup cancellation:

```typescript
it("hides old rows immediately and cancels the old request", async () => {
  vi.useFakeTimers()
  const first = deferred<WorkspaceFileSearchResult>()
  mocks.searchWorkspaceFiles.mockReturnValueOnce(first.promise)
  const { result, rerender, unmount } = renderHook(
    ({ query }) =>
      useWorkspaceFileSearch({
        folderPath: "/repo",
        query,
        enabled: true,
        limit: 100,
        debounceMs: 200,
      }),
    { initialProps: { query: "foo" } }
  )

  await vi.advanceTimersByTimeAsync(200)
  rerender({ query: "bar" })
  expect(result.current.files).toEqual([])
  expect(result.current.loading).toBe(true)
  expect(mocks.cancelWorkspaceFileSearch).toHaveBeenCalledTimes(1)

  first.resolve({
    files: [{ name: "foo.ts", path: "foo.ts", kind: "file" }],
    truncated: false,
  })
  await Promise.resolve()
  expect(result.current.files).toEqual([])
  unmount()
})
```

- [ ] **Step 2: Run the hook test and confirm RED**

Run: `pnpm exec vitest run src/hooks/use-workspace-file-search.test.ts`

Expected: module-not-found failure proves the hook does not exist.

- [ ] **Step 3: Implement the minimal query-tagged hook**

Store `{ key, files }` where `key` includes folder and exact query. Derive visible files synchronously only when that key equals the current render key, so old rows disappear before the replacement effect runs. Use the Task 3 identity API and map hits into `FlatFileEntry` inside the hook.

- [ ] **Step 4: Replace dialog-local file request state**

Delete `filteredFiles`, `filesLoading`, `fileSearchGenRef`, and the 200 ms file-search effect from `SearchCommandDialog`. Call the hook with `enabled: open && activeTab === "files"` and keep `shouldFilter={activeTab === "conversations"}`.

- [ ] **Step 5: Run hook, dialog-adjacent, and reference tests**

Run: `pnpm exec vitest run src/hooks/use-workspace-file-search.test.ts src/components/chat/composer/use-reference-search.test.ts src/components/chat/composer/suggestion/suggestion-popup.test.tsx`

Expected: all tests pass; no old rows are exposed while a query is pending.

- [ ] **Step 6: Commit the command-dialog hook**

```powershell
git add src/hooks/use-workspace-file-search.ts src/hooks/use-workspace-file-search.test.ts src/components/conversations/search-command-dialog.tsx
git commit -m "fix(files): hide stale command search results"
```

---

### Task 5: Add the Global File-Tree Display Preference

**Files:**
- Create: `src/lib/file-tree-display-prefs.ts`
- Create: `src/lib/file-tree-display-prefs.test.ts`

**Interfaces:**
- Produces: `loadShowIgnoredFiles(): boolean`
- Produces: `saveShowIgnoredFiles(value: boolean): void`
- Produces: `useShowIgnoredFiles(): [boolean, (value: boolean) => void, boolean]`

- [ ] **Step 1: Write failing storage and synchronization tests**

Test missing/invalid values, persistence, unavailable storage, hydration, same-window custom events, and browser storage events.

```typescript
it("defaults off and synchronizes persisted changes", async () => {
  localStorage.clear()
  const first = renderHook(() => useShowIgnoredFiles())
  const second = renderHook(() => useShowIgnoredFiles())
  await waitFor(() => expect(first.result.current[2]).toBe(true))
  expect(first.result.current[0]).toBe(false)

  act(() => first.result.current[1](true))
  expect(localStorage.getItem(FILE_TREE_SHOW_IGNORED_STORAGE_KEY)).toBe("true")
  await waitFor(() => expect(second.result.current[0]).toBe(true))
})
```

- [ ] **Step 2: Run the preference test and confirm RED**

Run: `pnpm exec vitest run src/lib/file-tree-display-prefs.test.ts`

Expected: module-not-found failure.

- [ ] **Step 3: Implement the preference module**

Use the exact storage/event keys from the design, return false outside the browser, hydrate in `useEffect`, and dispatch a `CustomEvent<boolean>` after successful or attempted saves. The event handler always reloads from storage rather than trusting unvalidated event detail.

- [ ] **Step 4: Run preference tests**

Run: `pnpm exec vitest run src/lib/file-tree-display-prefs.test.ts`

Expected: all preference tests pass in jsdom.

- [ ] **Step 5: Commit the preference module**

```powershell
git add src/lib/file-tree-display-prefs.ts src/lib/file-tree-display-prefs.test.ts
git commit -m "feat(files): persist ignored-file display preference"
```

---

### Task 6: Implement the One-Shot Ignored-Tree Overlay

**Files:**
- Create: `src/hooks/use-ignored-file-tree.ts`
- Create: `src/hooks/use-ignored-file-tree.test.ts`

**Interfaces:**
- Consumes: `getFileTree(rootPath, 2, true)`, `useShowIgnoredFiles()`, workspace `seq`, fallback tree, and `subscribeEnvelopes`
- Produces: `{ tree, showIgnored, setShowIgnored, restored, loading, refresh, treeGeneration }`
- Produces: `shouldRefreshIgnoredTree(kind, changedPaths)` pure helper

- [ ] **Step 1: Write failing pure event-policy tests**

```typescript
expect(shouldRefreshIgnoredTree("create", ["src/new.ts"])).toBe(true)
expect(shouldRefreshIgnoredTree("remove", ["src/old.ts"])).toBe(true)
expect(shouldRefreshIgnoredTree("modify", ["src/main.ts"])).toBe(false)
expect(shouldRefreshIgnoredTree("modify", [".gitignore"])).toBe(true)
expect(shouldRefreshIgnoredTree("modify", [])).toBe(true)
```

- [ ] **Step 2: Write failing hook lifecycle tests**

With a controllable envelope subscriber and deferred `getFileTree` promises, prove:

- default mode performs zero overlay requests;
- enabling makes one `includeIgnored=true` request and baselines the current seq;
- create/remove/ignore/sweep events coalesce to one active plus one queued request;
- ordinary modify events do not refresh;
- a seq advance without a matching envelope forces one refresh;
- old-folder/mode responses are discarded;
- initial failure persists false and reports `enable` once;
- background failure retains the last successful tree without calling the user-error callback;
- manual failure reports `manual` once.

- [ ] **Step 3: Run overlay tests and confirm RED**

Run: `pnpm exec vitest run src/hooks/use-ignored-file-tree.test.ts`

Expected: module-not-found failure.

- [ ] **Step 4: Implement the overlay state machine**

Use refs for generation, active request, queued refresh, last envelope seq, and seq baseline. `runRefresh(reason)` must return the active promise when already running while setting exactly one queued follow-up. In `finally`, run the queued refresh only if folder, mode, and generation remain current.

- [ ] **Step 5: Run overlay and preference tests**

Run: `pnpm exec vitest run src/hooks/use-ignored-file-tree.test.ts src/lib/file-tree-display-prefs.test.ts`

Expected: all tests pass with deterministic fake timers and deferred promises.

- [ ] **Step 6: Commit the overlay hook**

```powershell
git add src/hooks/use-ignored-file-tree.ts src/hooks/use-ignored-file-tree.test.ts
git commit -m "feat(files): load ignored tree on demand"
```

---

### Task 7: Wire the File-Tree UI and Guard Lazy Requests by Mode

**Files:**
- Modify: `src/components/ui/context-menu.tsx:1-155`
- Modify: `src/components/layout/aux-panel-file-tree-tab.tsx:70-105,877-1360,2050-2330`
- Modify: `src/components/layout/aux-panel-file-tree-tab-source.test.ts:169-182`
- Modify: `src/hooks/use-file-tree.test.ts`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`

**Interfaces:**
- Consumes: Task 6 overlay tree and `treeGeneration`
- Produces: `ContextMenuCheckboxItem`
- Changes: lazy in-flight state from `Set<string>` to `Map<string, number>`

- [ ] **Step 1: Write failing checkbox and lazy-generation tests**

Add the minimal source assertion for two checkbox usages, plus pure helper tests for generation-safe completion:

```typescript
expect(auxTreeSource.match(/<ContextMenuCheckboxItem/g)).toHaveLength(2)
expect(auxTreeSource).toContain('t("showIgnoredFiles")')

const inFlight = new Map([["src", 2]])
finishLazyLoad(inFlight, "src", 1)
expect(inFlight.get("src")).toBe(2)
finishLazyLoad(inFlight, "src", 2)
expect(inFlight.has("src")).toBe(false)
```

- [ ] **Step 2: Run focused UI tests and confirm RED**

Run: `pnpm exec vitest run src/components/layout/aux-panel-file-tree-tab-source.test.ts src/hooks/use-file-tree.test.ts`

Expected: checkbox export/usages and generation helper are missing.

- [ ] **Step 3: Add `ContextMenuCheckboxItem`**

Mirror `DropdownMenuCheckboxItem` with `ContextMenuPrimitive.CheckboxItem`, `ItemIndicator`, and Lucide `CheckIcon`; export it from `context-menu.tsx`.

- [ ] **Step 4: Integrate the overlay tree**

Call `useIgnoredFileTree` with `workspaceState.tree`, `workspaceState.seq`, and `workspaceState.subscribeEnvelopes`. Replace the source-tree effect dependency with the hook's returned tree. Gate ignore-preview reads on `showIgnored` and show one translated toast for `enable`/`manual` errors.

- [ ] **Step 5: Make lazy loads generation-safe**

Capture `{ rootPath, showIgnored, treeGeneration }` before `getFileTree(joinFsPath(...), 1, showIgnored)`. Before applying children, verify all captured values remain current. Store the generation in `lazyLoadingDirPathsRef`; in `finally`, delete only when the map still contains that generation. On every mode/folder generation transition, clear cached children, increment the generation, and reload expanded directories.

- [ ] **Step 6: Add both checkbox menu items and translations**

Place one checkbox in the root node menu and one in the background menu:

```tsx
<ContextMenuCheckboxItem
  checked={showIgnored}
  onCheckedChange={(checked) => setShowIgnored(checked === true)}
>
  {t("showIgnoredFiles")}
</ContextMenuCheckboxItem>
```

Add `showIgnoredFiles` and `toasts.loadIgnoredFilesFailed` to all ten locale files with natural translations.

- [ ] **Step 7: Run focused file-tree and overlay tests**

Run:

```powershell
pnpm exec vitest run src/components/layout/aux-panel-file-tree-tab-source.test.ts src/hooks/use-file-tree.test.ts src/hooks/use-ignored-file-tree.test.ts src/lib/file-tree-display-prefs.test.ts
```

Expected: all tests pass; mode toggles cannot apply old lazy responses.

- [ ] **Step 8: Commit file-tree integration**

```powershell
git add src/components/ui/context-menu.tsx src/components/layout/aux-panel-file-tree-tab.tsx src/components/layout/aux-panel-file-tree-tab-source.test.ts src/hooks/use-file-tree.test.ts src/i18n/messages
git commit -m "feat(files): add show-ignored tree option"
```

---

### Task 8: Run the Full Verification Matrix and Review the Final Diff

**Files:**
- Modify only files required by failures directly caused by Tasks 1-7.

**Interfaces:**
- Verifies all acceptance criteria from the approved design.

- [ ] **Step 1: Run all focused frontend tests**

```powershell
pnpm exec vitest run src/lib/workspace-file-api.test.ts src/lib/file-tree-display-prefs.test.ts src/hooks/use-workspace-file-search.test.ts src/hooks/use-ignored-file-tree.test.ts src/hooks/use-file-tree.test.ts src/components/chat/composer/use-reference-search.test.ts src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/layout/aux-panel-file-tree-tab-source.test.ts
```

Expected: all listed files pass with zero failed tests.

- [ ] **Step 2: Run full frontend validation**

```powershell
pnpm test
pnpm build
pnpm eslint .
```

Expected: tests, static export, TypeScript, and ESLint exit 0. If Windows `core.autocrlf=true` causes repository-wide Prettier CRLF diagnostics, verify Git blobs and `git diff --check` separately and report that environmental limitation without rewriting unrelated files.

- [ ] **Step 3: Run Rust desktop validation**

```powershell
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: all commands exit 0 with no new warnings.

- [ ] **Step 4: Run Rust server and MCP validation**

```powershell
cd src-tauri
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: all commands exit 0.

- [ ] **Step 5: Inspect behavior-specific evidence**

Confirm from tests and code that:

- default file-tree mode does not issue an include-ignored request;
- enabling displays an ignored fixture and mode-off removes it;
- root and lazy stale responses carry generation guards;
- cancel commands include both IDs and delayed cancel is a no-op;
- the semaphore permit is owned by the blocking closure;
- command-dialog rows are query-tagged;
- `Ä.TXT` matches `ä`.

- [ ] **Step 6: Review repository state**

Run:

```powershell
git diff --check
git status --short
git diff --stat d7df476e..HEAD
```

Expected: no whitespace errors, no generated build artifacts staged, and only scoped implementation/test/i18n files changed. Preserve the pre-existing untracked files.

- [ ] **Step 7: Commit any verification-only corrections**

Only when Step 2-5 required a scoped correction, inspect `git status --short`
and stage each correction using its exact literal path from the File Map. Do
not use a broad add command. Then run:

```powershell
git diff --cached --check
git commit -m "test(files): complete workspace search hardening"
```
