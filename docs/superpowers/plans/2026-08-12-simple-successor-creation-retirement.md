# Simple Successor Creation Retirement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove automatic archived-workflow-to-Simple successor creation while preserving the old Tauri command and authenticated HTTP route as exact, side-effect-free retirement errors.

**Architecture:** Archived manifest workflows remain read-only history and retain their wire-compatible navigation fields, but those fields always state that no successor exists and none can be created. The compatibility command core is a pure, zero-argument error constructor; ordinary new conversations and locator-only Simple workflows remain independent. The unpublished successor source link and bootstrap schema are removed from the clean-install migration chain instead of being migrated forward.

**Tech Stack:** Rust 2021, Tauri 2, Axum, SeaORM/SQLite, Next.js 16 static export, React 19, TypeScript strict, Vitest, next-intl, pnpm.

## Global Constraints

- The approved design is `docs/superpowers/specs/2026-08-12-simple-successor-creation-retirement-design.md`; it supersedes only the automatic successor portions of the 2026-08-11 design and plan.
- Keep `continue_archived_workflow_in_simple` registered in both the production Tauri command registry and the authenticated server router.
- The exact direct-call error code is `simple_successor_creation_retired`.
- The exact direct-call error message is `Automatic Simple successor creation is retired; create a new conversation and use a new Design.`
- Authenticated HTTP calls return `409 Conflict`; unauthenticated HTTP calls still return the existing `401` before the handler.
- The shared interface is exactly `continue_archived_workflow_in_simple_core() -> Result<(), AppCommandError>` and has no database, emitter, path, conversation, token, or connection dependency.
- The Tauri compatibility command declares no operation-specific parameters. The Axum handler declares no JSON extractor and no `AppState` extractor.
- Keep `ArchivedWorkflowNavigationSnapshot.source_conversation_id` and `plan_rel_path`; always project `successor_conversation_id: null` and `can_create_simple_successor: false`.
- `ArchivedWorkflowNavigationSnapshot.successor_conversation_id` must remain present on serialized archived snapshots when retired; remove any `skip_serializing_if` behavior that would omit it instead of emitting JSON `null`.
- Remove only `SimpleWorkflowLocatorSnapshot.source_conversation_id`; do not confuse it with the archived snapshot field of the same name.
- Keep the `workflow_v2_retired` code and change its exact message to `This workflow is archived and read-only. Create a new conversation and use a new Design.`
- Do not add an archive-specific new-conversation shortcut, callout, copied locator, inferred identity link, bootstrap prompt, or automatic navigation.
- Preserve ordinary Simple registration, locator updates, Plan/progress projection, delegation, review, recovery, and execution.
- Preserve `completion_protocol.v2_successor` and `delegation_workflows.legacy_source_workflow_id`; they are unrelated compatibility surfaces.
- Rewrite the unpublished clean-install migrations. Do not add a forward migration, data-copy rebuild, runtime cleanup, or compatibility view for development successor rows.
- Preserve commit `16684d6d`, all `.codex-tmp-*` paths, `.task-runtimes/`, and every unrelated user-owned worktree change. Never stash, reset, clean, or include them in task commits.
- Run Cargo commands serially with `RUST_MIN_STACK=16777216` and 30-60 second waits for full tests or Clippy. Never overlap Cargo processes.
- Do not run `pnpm build` in the main worktree. Production build evidence comes from a clean detached worktree at the reviewed implementation commit.
- Every task commit uses the exact pathspec listed in that task. Before committing, inspect `git diff --cached --name-status` and reject any unrelated path.

## File And Responsibility Map

- `src-tauri/src/commands/simple_workflow.rs`: retain only the pure retired command core, the parameterless Tauri wrapper, and shared archived-workflow test fixtures; delete the creator/bootstrap engine and its tests.
- `src-tauri/src/web/handlers/simple_workflow.rs`: parameterless authenticated compatibility handler and raw-body/auth parity tests.
- `src-tauri/src/app_error.rs`, `src-tauri/src/web/handlers/error.rs`: stable retirement code serialization and HTTP 409 mapping.
- `src-tauri/src/commands/acp.rs`, `src-tauri/src/web/handlers/acp.rs`: remove post-connect bootstrap admission hooks without changing ordinary connection setup.
- `src/components/chat/sub-agent-overlay.tsx`: archived history and Plan access only; no successor state, request, or navigation control.
- `src/lib/api.ts`, `src/lib/tauri.ts`, `src/lib/types.ts`: remove current-client successor helpers and success DTO while retaining archived compatibility fields.
- `src/i18n/messages/*.json`: remove only the three successor interaction keys in all ten locales.
- `src-tauri/src/acp/delegation/workflow/dto.rs`: remove the Simple source field while keeping archived compatibility fields.
- `src-tauri/src/acp/delegation/workflow/project.rs`: make archived successor values constant and remove source-link/Plan-eligibility reads.
- `src-tauri/src/acp/delegation/workflow/error.rs` and related ACP error mappings: retain the archived write fence with constant retired navigation and the new guidance message.
- `src-tauri/src/acp/delegation/workflow/simple.rs`: locator-only Simple registration and mode resolution; remove successor eligibility and source-aware registration.
- `src-tauri/src/db/migration/m20260811_000001_simple_workflows.rs`: final clean-install locator-only schema and schema tests.
- `src-tauri/src/db/migration/m20260812_000001_simple_successor_bootstraps.rs`: delete completely and unregister.
- `src-tauri/src/db/entities/simple_workflow.rs`: parent/Plan/progress/timestamps only.
- `src-tauri/src/db/entities/simple_successor_bootstrap.rs`: delete completely and unexport.
- `.superpowers/sdd/2026-08-12-simple-successor-creation-retirement/verification-report.md`: final, evidence-only matrix report created after all implementation commits.

## Superseded Work Mapping

- The old plan's Task 6, **Create or reopen an idempotent Simple successor**, is retired in full. Tasks 1, 3, and 4 below replace it with a stable rejection surface, constant archived metadata, and deletion of its runtime/schema.
- The successor-specific part of old Task 7, **Render archived and Simple workflows**, is replaced by Task 2. Its archived history, Plan opening, graph, reports, and child navigation coverage remains; only creation/open-successor interaction is removed.
- The `source_workflow_id` portion of old Task 1 is replaced by Task 4's locator-only schema. Ordinary Simple parent identity, Plan path, progress path, cascades, registration, and updates remain.
- Old Task 5's archived mutation fence remains authoritative except for its obsolete successor guidance and metadata. Task 3 changes only that message and the constant successor fields.
- Old Tasks 2-4 and Task 8 remain authoritative wherever they concern Plan/progress parsing, projection/reconciliation, MCP Simple registration, Skill behavior, delegation, recovery, review, or verification rather than automatic successors.

---

### Task 1: Replace The Successor Runtime With Stable Rejection Surfaces

**Files:**
- Modify: `src-tauri/src/app_error.rs`
- Modify: `src-tauri/src/web/handlers/error.rs`
- Modify: `src-tauri/src/commands/simple_workflow.rs`
- Modify: `src-tauri/src/web/handlers/simple_workflow.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `AppCommandError::new(AppErrorCode, message)` and the existing authenticated route/command registries.
- Produces: `pub const SIMPLE_SUCCESSOR_CREATION_RETIRED_MESSAGE: &str` and `pub fn continue_archived_workflow_in_simple_core() -> Result<(), AppCommandError>`.
- Produces: parameterless Tauri and Axum wrappers returning `Result<(), AppCommandError>`.
- Preserves: `commands::simple_workflow::test_support::{archived_manifest, seed_archived_workflow, seed_bound_child}` for archived projection/fence tests in later tasks.

- [ ] **Step 1: Rewrite command and transport tests to state the retirement contract**

In `src-tauri/src/commands/simple_workflow.rs`, replace the successor creation/replay/bootstrap tests with a focused contract test. Calling the core with no arguments is intentional: it makes the dependency shape part of the compile-time contract.

```rust
#[test]
fn simple_successor_creation_retired_core_is_exact_and_state_free() {
    let error = continue_archived_workflow_in_simple_core().unwrap_err();
    assert_eq!(error.code, AppErrorCode::SimpleSuccessorCreationRetired);
    assert_eq!(
        error.message,
        "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
    );
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        serde_json::json!({
            "code": "simple_successor_creation_retired",
            "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
        })
    );
}
```

Under `tauri-runtime`, add a direct wrapper parity test and rename the registry test in `src-tauri/src/lib.rs` so its purpose remains explicit:

```rust
#[test]
fn simple_successor_creation_retired_tauri_wrapper_matches_core() {
    let wrapper = continue_archived_workflow_in_simple().unwrap_err();
    let core = continue_archived_workflow_in_simple_core().unwrap_err();
    assert_eq!(wrapper.code, core.code);
    assert_eq!(wrapper.message, core.message);
    assert_eq!(serde_json::to_value(wrapper).unwrap(), serde_json::to_value(core).unwrap());
}

#[test]
fn production_tauri_registry_contains_retired_simple_successor_command() {
    assert!(super::production_tauri_command_paths().contains(
        &"crate :: commands :: simple_workflow :: continue_archived_workflow_in_simple"
    ));
}
```

Also add a real MockRuntime IPC test under
`#[cfg(all(feature = "tauri-runtime", feature = "test-utils"))]`. Register the
production command itself, invoke its stable command name, and send the complete
stale-client payload plus an unrelated field. The test must deserialize the
error callback and prove unknown legacy arguments are ignored rather than
validated:

```rust
#[test]
fn simple_successor_creation_retired_tauri_ipc_ignores_stale_arguments() {
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY,
    };
    use tauri::webview::InvokeRequest;

    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            crate::commands::simple_workflow::continue_archived_workflow_in_simple
        ])
        .build(mock_context(noop_assets()))
        .expect("mock app");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("webview");
    let local_url = if cfg!(any(windows, target_os = "android")) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };

    let value = get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: "continue_archived_workflow_in_simple".into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: local_url.parse().unwrap(),
            body: InvokeBody::from(serde_json::json!({
                "sourceConversationId": -1,
                "clientRequestToken": "",
                "extra": { "malformed": true }
            })),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .expect_err("retired command must reject");
    assert_eq!(
        value,
        serde_json::json!({
            "code": "simple_successor_creation_retired",
            "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
        })
    );
}
```

In `src-tauri/src/web/handlers/simple_workflow.rs`, replace the JSON-success/domain-error tests with raw-body tests. The helper must use `RequestBuilder::body` rather than `.json`, so malformed JSON genuinely reaches the parameterless handler.

```rust
async fn call_raw(
    state: Arc<AppState>,
    static_dir: &std::path::Path,
    body: &str,
    content_type: Option<&str>,
    auth: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let app = build_router(
        state,
        "secret".into(),
        static_dir.to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind HTTP test listener");
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://{addr}/api/continue_archived_workflow_in_simple"))
        .body(body.to_owned());
    if let Some(content_type) = content_type {
        request = request.header("Content-Type", content_type);
    }
    if let Some(auth) = auth {
        request = request.header("Authorization", auth);
    }
    let response = request.send().await.expect("HTTP response");
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
    let value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    handle.abort();
    (status, value)
}

#[tokio::test]
async fn simple_successor_creation_retired_http_ignores_every_authenticated_body() {
    let workspace = tempfile::tempdir().unwrap();
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let ordinary = seed_conversation(&db, folder, AgentType::Codex).await;
    let simple = seed_conversation(&db, folder, AgentType::Codex).await;
    register_simple_workflow(&db.conn, simple, "docs/plan.md", None)
        .await
        .unwrap();
    let archived = seed_conversation(&db, folder, AgentType::Codex).await;
    let archived_child = seed_conversation(&db, folder, AgentType::Codex).await;
    seed_archived_workflow(
        &db,
        archived,
        "workflow-http-retired-successor",
        "docs/missing-plan.md",
        None,
        2,
        CompletionProtocolMode::V2Enforce,
    )
    .await;
    seed_bound_child(
        &db,
        archived,
        archived_child,
        "workflow-http-retired-successor",
    )
    .await;
    let deleted = seed_conversation(&db, folder, AgentType::Codex).await;
    conversation_service::soft_delete(&db.conn, deleted).await.unwrap();
    let missing = deleted + 1_000_000;
    let state = Arc::new(AppState::new_for_test(db, workspace.path().to_path_buf()));

    let oversized_token = "x".repeat(257);
    let bodies = vec![
        String::new(),
        "{".into(),
        "null".into(),
        serde_json::json!({}).to_string(),
        serde_json::json!({
            "sourceConversationId": "wrong",
            "clientRequestToken": false,
        })
        .to_string(),
        serde_json::json!({
            "sourceConversationId": 0,
            "clientRequestToken": "",
        })
        .to_string(),
        serde_json::json!({
            "sourceConversationId": -1,
            "clientRequestToken": oversized_token,
            "extra": true,
        })
        .to_string(),
        serde_json::json!({ "sourceConversationId": ordinary, "clientRequestToken": "ordinary" }).to_string(),
        serde_json::json!({ "sourceConversationId": simple, "clientRequestToken": "simple" }).to_string(),
        serde_json::json!({ "sourceConversationId": archived, "clientRequestToken": "archived" }).to_string(),
        serde_json::json!({ "sourceConversationId": archived_child, "clientRequestToken": "archived-child" }).to_string(),
        serde_json::json!({ "sourceConversationId": deleted, "clientRequestToken": "deleted" }).to_string(),
        serde_json::json!({ "sourceConversationId": missing, "clientRequestToken": "missing" }).to_string(),
        // Intentional replay of the archived body: the second call must remain
        // a no-op retirement conflict with no additional side effects.
        serde_json::json!({ "sourceConversationId": archived, "clientRequestToken": "archived" }).to_string(),
    ];

    for body in &bodies {
        let (status, value) = call_raw(
            state.clone(),
            workspace.path(),
            body,
            Some("application/json"),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            value,
            serde_json::json!({
                "code": "simple_successor_creation_retired",
                "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
            })
        );
    }
}
```

Keep a separate unauthenticated test using malformed body `"{"` and assert `401`. Define and use this concrete state snapshot; import the listed entities plus `EntityTrait`, `PaginatorTrait`, `QueryOrder`, and `QuerySelect`:

```rust
#[derive(Debug, PartialEq, Eq)]
struct SideEffectSnapshot {
    conversations: u64,
    simple_workflows: u64,
    simple_successor_bootstraps: u64,
    delegation_workflows: u64,
    delegation_runs: u64,
    attention_requests: u64,
    recovery_authorizations: u64,
    auto_title_jobs: u64,
    message_counts: Vec<(i32, i32)>,
}

async fn side_effect_snapshot(db: &AppDatabase) -> SideEffectSnapshot {
    SideEffectSnapshot {
        conversations: conversation::Entity::find().count(&db.conn).await.unwrap(),
        simple_workflows: simple_workflow::Entity::find().count(&db.conn).await.unwrap(),
        simple_successor_bootstraps: simple_successor_bootstrap::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        delegation_workflows: delegation_workflow::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        delegation_runs: delegation_task_run::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        attention_requests: delegation_attention_request::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        recovery_authorizations: recovery_authorization::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        auto_title_jobs: auto_title_job::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        message_counts: conversation::Entity::find()
            .select_only()
            .columns([conversation::Column::Id, conversation::Column::MessageCount])
            .order_by_asc(conversation::Column::Id)
            .into_tuple::<(i32, i32)>()
            .all(&db.conn)
            .await
            .unwrap(),
    }
}
```

Capture `let before = side_effect_snapshot(&state.db).await` and subscribe to `state.event_broadcaster` before the authenticated loop. After every body has been exercised, assert `side_effect_snapshot(&state.db).await == before` and `receiver.try_recv()` returns `tokio::sync::broadcast::error::TryRecvError::Empty`. These counts cover conversations/descriptors/workflows/bootstrap/transcript counters/delegation/authorization/title work during RED; Task 4 removes only the bootstrap field/query from the final snapshot because schema absence gets its own migration assertion.

- [ ] **Step 2: Run the new tests and capture RED**

Run from `src-tauri/`, serially:

```powershell
$env:RUST_MIN_STACK = '16777216'
cargo test --lib --features test-utils simple_successor_creation_retired -- --nocapture
```

Expected: FAIL to compile because `AppErrorCode::SimpleSuccessorCreationRetired` does not exist and the current core/wrappers still require state, IDs, and tokens; if compilation reaches the HTTP assertions, the current creator returns success or source-specific errors instead of the retirement conflict.

- [ ] **Step 3: Implement the minimal pure retirement core and wrappers**

Add the new error variant, remove the three dead variants, and lock its wire value in `stable_completion_protocol_codes_serialize_as_snake_case`:

```rust
/// The removed archived-to-Simple successor operation was invoked.
SimpleSuccessorCreationRetired,
```

Replace the production portion of `commands/simple_workflow.rs` with the pure contract below. Delete `SimpleSuccessorResult`, `SimpleBootstrapPromptSink`, token/path validation, archived source loading, creation/replay/retry/rollback/concurrency code, bootstrap construction/storage/admission, test controls, and their successor-only tests. Retain the `#[cfg(test)] test_support` fixture module unchanged except for imports made dead by the deletion.

```rust
use crate::app_error::{AppCommandError, AppErrorCode};

pub const SIMPLE_SUCCESSOR_CREATION_RETIRED_MESSAGE: &str =
    "Automatic Simple successor creation is retired; create a new conversation and use a new Design.";

pub fn continue_archived_workflow_in_simple_core() -> Result<(), AppCommandError> {
    Err(AppCommandError::new(
        AppErrorCode::SimpleSuccessorCreationRetired,
        SIMPLE_SUCCESSOR_CREATION_RETIRED_MESSAGE,
    ))
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub fn continue_archived_workflow_in_simple() -> Result<(), AppCommandError> {
    continue_archived_workflow_in_simple_core()
}
```

Make the Axum handler body/state independent:

```rust
use crate::app_error::AppCommandError;
use crate::commands::simple_workflow::continue_archived_workflow_in_simple_core;

pub async fn continue_archived_workflow_in_simple() -> Result<(), AppCommandError> {
    continue_archived_workflow_in_simple_core()
}
```

Map `SimpleSuccessorCreationRetired` to `StatusCode::CONFLICT` in `web/handlers/error.rs`. Remove the old successor-code match arms. Keep the route in `web/router.rs` untouched.

Delete the `admit_simple_successor_bootstrap_after_connect(...)` call after successful connection setup in both `commands/acp.rs` and `web/handlers/acp.rs`. Do not change spawn arguments, connection ownership checks, failure mapping, or ordinary prompt admission.

- [ ] **Step 4: Run focused GREEN checks**

Run from `src-tauri/`, serially:

```powershell
cargo test --lib --features test-utils simple_successor_creation_retired -- --nocapture
cargo test --lib --features test-utils production_tauri_registry_contains_retired_simple_successor_command -- --nocapture
cargo test --lib --features test-utils stable_completion_protocol_errors_map_to_expected_http_status -- --nocapture
cargo check --features test-utils
```

Expected: all tests PASS, desktop check exits 0, authenticated raw bodies all produce exact 409 errors, unauthenticated input remains 401, and count/event assertions show no side effects.

- [ ] **Step 5: Inspect and commit only the runtime retirement change**

```powershell
git diff --check -- src-tauri/src/app_error.rs src-tauri/src/web/handlers/error.rs src-tauri/src/commands/simple_workflow.rs src-tauri/src/web/handlers/simple_workflow.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/lib.rs
git add -- src-tauri/src/app_error.rs src-tauri/src/web/handlers/error.rs src-tauri/src/commands/simple_workflow.rs src-tauri/src/web/handlers/simple_workflow.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/lib.rs
git diff --cached --name-status
git commit -m "refactor(workflow): retire automatic simple successors"
```

Expected staged paths: exactly the seven files listed above.

---

### Task 2: Remove Current-Client Successor Reachability

**Files:**
- Modify: `src/components/chat/sub-agent-overlay.tsx`
- Modify: `src/components/chat/sub-agent-overlay.test.tsx`
- Modify: `src/components/chat/workflow-overlay.test.tsx`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/workflow-types.test.ts`
- Modify: `src/lib/workflow-graph-store.test.ts`
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
- Consumes: archived `source_conversation_id`, `plan_rel_path`, `successor_conversation_id`, and `can_create_simple_successor` as read-compatible input.
- Produces: archived rendering with history and `Open Plan` only; compatibility successor values are intentionally ignored.
- Removes: `continueArchivedWorkflowInSimple` from both frontend transport modules and `SimpleSuccessorResult` from TypeScript.
- Removes: `SimpleWorkflowLocatorSnapshot.source_conversation_id`; archived `source_conversation_id` remains required.

- [ ] **Step 1: Replace interaction tests with absence tests**

In both overlay test files, remove request-token, pending, rejection, unmount, source-change, and existing-successor navigation cases. Replace them with table-driven compatibility-input tests that deliberately feed both old values and retired values:

```tsx
it.each([
  {
    successor_conversation_id: 84,
    can_create_simple_successor: false,
  },
  {
    successor_conversation_id: null,
    can_create_simple_successor: true,
  },
])(
  "keeps archived history visible without a successor action",
  async (compatibilityValues) => {
    const snapshot = archivedSnapshot()
    snapshot.archived = {
      ...snapshot.archived!,
      ...compatibilityValues,
    }
    const onOpenRootConversation = vi.fn()

    renderWithIntl(
      <SubAgentOverlay
        delegations={[]}
        workflowGraph={snapshot}
        workspaceRootPath="D:\\Repo"
        onOpenRootConversation={onOpenRootConversation}
      />
    )

    expect(screen.getByTestId("workflow-archived-banner")).toBeVisible()
    expect(screen.getByRole("button", { name: "Open Plan" })).toBeVisible()
    expect(
      screen.queryByRole("button", { name: "Continue in Simple" })
    ).toBeNull()
    expect(
      screen.queryByRole("button", { name: "Open Simple successor" })
    ).toBeNull()
    expect(onOpenRootConversation).not.toHaveBeenCalled()
  }
)
```

Keep the existing assertions that archived reports, graph nodes, child navigation, and Plan opening work. Change `simpleGraph()`, `workflow-types.test.ts`, and the `simpleSnapshot()` fixture in `workflow-graph-store.test.ts` so a Simple locator contains only `plan_rel_path` and `progress_rel_path`. Keep archived compatibility fields in the archived fixture, but expect `null` and `false` in the canonical shape.

- [ ] **Step 2: Run focused frontend tests and capture RED**

Run from the repository root:

```powershell
pnpm exec vitest run src/components/chat/sub-agent-overlay.test.tsx src/components/chat/workflow-overlay.test.tsx src/lib/api.test.ts src/lib/workflow-types.test.ts src/lib/workflow-graph-store.test.ts
```

Expected: FAIL because the current archived banner still renders `Continue in Simple` or `Open Simple successor` for the supplied compatibility values.

- [ ] **Step 3: Remove the UI state machine and frontend request surface**

In `ArchivedWorkflowBanner`, retain only `useTranslations`, `useOpenLinkOrFile`, the archived null guard, `openPlan`, and the archived history markup. Delete successor normalization, `canCreate`, `actionScopeKey`, pending/error refs and effects, request-token generation, API calls, successor navigation, the action button, loader, arrow icon, and error alert. Do not add replacement copy or a new-conversation control.

Remove `conversationId` and `onOpenRootConversation` from the internal
`ArchivedWorkflowBanner` props and from its call site because they are used only
by the deleted successor state machine. Keep those props on the outer overlay
and `WorkflowGraphPanel`, where ordinary child/root history navigation still
uses them.

Remove imports that are successor-only: `continueArchivedWorkflowInSimple`, `toErrorMessage` if no other use remains, `randomUUID` if no other use remains, and successor-only `ArrowRightIcon`/`Loader2Icon`. Keep React hooks still used elsewhere in the large overlay module.

Delete the local `useIsomorphicLayoutEffect` alias after its three
successor-only uses are gone. Retain `useRef` and `useState`, which the rest of
the overlay still uses.

Delete these exports and their imports/tests:

```ts
// Remove from src/lib/api.ts and src/lib/tauri.ts:
continueArchivedWorkflowInSimple

// Remove from src/lib/types.ts:
SimpleSuccessorResult
SimpleWorkflowLocatorSnapshot.source_conversation_id
```

Keep this archived type shape:

```ts
export interface ArchivedWorkflowNavigationSnapshot {
  source_conversation_id: number
  plan_rel_path?: string | null
  successor_conversation_id: number | null
  can_create_simple_successor: boolean
}
```

Remove only `archivedContinue`, `archivedContinuing`, and `archivedOpenSuccessor` from all ten locale JSON files.

- [ ] **Step 4: Run focused GREEN and formatting checks**

```powershell
pnpm exec vitest run src/components/chat/sub-agent-overlay.test.tsx src/components/chat/workflow-overlay.test.tsx src/lib/api.test.ts src/lib/workflow-types.test.ts src/lib/workflow-graph-store.test.ts
pnpm exec prettier --check src/components/chat/sub-agent-overlay.tsx src/components/chat/sub-agent-overlay.test.tsx src/components/chat/workflow-overlay.test.tsx src/lib/api.ts src/lib/api.test.ts src/lib/tauri.ts src/lib/types.ts src/lib/workflow-types.test.ts src/lib/workflow-graph-store.test.ts src/i18n/messages/*.json
```

Expected: focused tests PASS and Prettier exits 0. Archived history/Plan navigation remains covered, while neither old compatibility input can render or invoke a successor action.

- [ ] **Step 5: Inspect and commit only the frontend removal**

```powershell
git diff --check -- src/components/chat/sub-agent-overlay.tsx src/components/chat/sub-agent-overlay.test.tsx src/components/chat/workflow-overlay.test.tsx src/lib/api.ts src/lib/api.test.ts src/lib/tauri.ts src/lib/types.ts src/lib/workflow-types.test.ts src/lib/workflow-graph-store.test.ts src/i18n/messages
git add -- src/components/chat/sub-agent-overlay.tsx src/components/chat/sub-agent-overlay.test.tsx src/components/chat/workflow-overlay.test.tsx src/lib/api.ts src/lib/api.test.ts src/lib/tauri.ts src/lib/types.ts src/lib/workflow-types.test.ts src/lib/workflow-graph-store.test.ts src/i18n/messages/ar.json src/i18n/messages/de.json src/i18n/messages/en.json src/i18n/messages/es.json src/i18n/messages/fr.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/pt.json src/i18n/messages/zh-CN.json src/i18n/messages/zh-TW.json
git diff --cached --name-status
git commit -m "refactor(workflow): remove archived successor UI"
```

Expected staged paths: exactly the nine TypeScript/TSX files and ten locale files listed above.

---

### Task 3: Make Archived Projection And Mutation Metadata Permanently Retired

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/dto.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/error.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/commands/workflow_completion.rs`

**Interfaces:**
- Consumes: archived workflow header/root resolution and the existing `WorkflowV2Retired` structured navigation fields.
- Produces: `SimpleWorkflowLocatorSnapshot { plan_rel_path, progress_rel_path }` with no source field.
- Produces: every archived read/fence metadata instance with `successor_conversation_id: None` and `can_create_simple_successor: false`.
- Produces: `WorkflowStoreError::workflow_v2_retired_with_navigation(source_conversation_id: i32) -> Self`; the constructor fills compatibility successor fields with `None/false` and accepts no values for them.
- Preserves: archived root/child resolution, identity-corruption detection, history projection, and the `workflow_v2_retired` code.

- [ ] **Step 1: Rewrite DTO, projection, and fence tests for constant retirement metadata**

Update the DTO wire test so the canonical Simple object has exactly two locators and the archived object serializes literal retired values. Remove `skip_serializing_if = "Option::is_none"` from `ArchivedWorkflowNavigationSnapshot.successor_conversation_id`; keep `#[serde(default)]` if backward deserialization compatibility is desired. Compare the complete archived object, not indexed lookups that cannot distinguish a missing field from JSON null:

```rust
simple: Some(SimpleWorkflowLocatorSnapshot {
    plan_rel_path: "docs/superpowers/plans/plan.md".into(),
    progress_rel_path: ".superpowers/sdd/42/progress.md".into(),
}),

archived: Some(ArchivedWorkflowNavigationSnapshot {
    source_conversation_id: 7,
    plan_rel_path: Some("docs/superpowers/plans/plan.md".into()),
    successor_conversation_id: None,
    can_create_simple_successor: false,
}),

assert_eq!(
    archived_json["archived"],
    serde_json::json!({
        "source_conversation_id": 7,
        "plan_rel_path": "docs/superpowers/plans/plan.md",
        "successor_conversation_id": null,
        "can_create_simple_successor": false,
    })
);
```

Rename the archived projection eligibility test to `simple_projection_archived_always_reports_retired_successor_fields`. Keep eligible, missing, oversized, and invalid-UTF8 Plan cases, but assert the same result for all four:

```rust
assert_eq!(archived.successor_conversation_id, None);
assert!(!archived.can_create_simple_successor);
```

Retain a deliberately source-linked Simple descriptor in one RED fixture until Task 4 removes the column; assert it is ignored by archived projection. In the fence tests, resolve both archived root and bound child and assert:

```rust
assert_eq!(error.to_string(), WORKFLOW_V2_RETIRED_MESSAGE);
assert_eq!(error.source_conversation_id(), Some(root));
assert_eq!(error.successor_conversation_id(), None);
assert_eq!(error.can_create_simple_successor(), Some(false));
```

Update `commands/workflow_completion.rs`, `acp/error.rs`, listener assertions, `acp/manager.rs` prompt-fence assertions, `workflow/store.rs` publication-fence assertions, and `acp/delegation/broker.rs` retirement-report conversion assertions to the new exact message and to omit successor navigation while retaining source ID and `false` availability. Synthetic serialization/conversion tests may still instantiate the compatibility structure directly, but their canonical archived-retirement examples must use `None/false` so test fixtures never imply a supported successor.

- [ ] **Step 2: Run the changed tests and capture RED**

Run from `src-tauri/`, serially:

```powershell
$env:RUST_MIN_STACK = '16777216'
cargo test --lib --features test-utils simple_and_archived_snapshots_have_stable_navigation_wire_shapes -- --nocapture
cargo test --lib --features test-utils simple_projection_archived -- --nocapture
cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
```

Expected: FAIL because the current Simple DTO still emits `source_conversation_id`, eligible archived Plans advertise creation, linked descriptors advertise a successor, and the old message still says to continue in a Simple successor.

- [ ] **Step 3: Remove successor inference while preserving archived identity checks**

Remove `source_conversation_id` from the Rust `SimpleWorkflowLocatorSnapshot` and construct Simple snapshots directly from the two safe locators. Delete the `descriptor.source_workflow_id` lookup from `project_simple_mode`.

In archived projection, delete the `simple_workflow::Column::SourceWorkflowId` query and archived Plan eligibility call. Construct navigation exactly as follows:

```rust
let archived = ArchivedWorkflowNavigationSnapshot {
    source_conversation_id: header.parent_conversation_id,
    plan_rel_path: Some(normalized.plan_target_rel_path.clone()),
    successor_conversation_id: None,
    can_create_simple_successor: false,
};
```

In `workflow/error.rs`, retain the query that detects a conflicting Simple descriptor on the archived parent. Delete only source-to-successor lookup and Plan eligibility. `ArchivedWorkflowNavigation` no longer needs stored workflow or successor IDs. Every archived root, bound child, publication retirement, writable guard, and fallback path uses the one-argument constructor below, which supplies `None/false` internally.

Tighten the constructor so callers cannot supply those constants incorrectly:

```rust
pub const fn workflow_v2_retired_with_navigation(
    source_conversation_id: i32,
) -> Self {
    Self::WorkflowV2Retired {
        source_conversation_id: Some(source_conversation_id),
        successor_conversation_id: None,
        can_create_simple_successor: false,
    }
}
```

Update every caller in `workflow/error.rs`, `workflow/store.rs`, and
`acp/delegation/listener.rs` from the old three-argument form to this one-argument
form. Update the broker and completion-command conversion tests to construct
the canonical error with the same one-argument helper.

When `workflow_v2_retired_for_conversation` or
`workflow_v2_publication_retired_for_conversation` has no archived navigation,
return `workflow_v2_retired_with_navigation(conversation_id)`
directly. Delete their fallback `simple_workflow` queries entirely; retired
metadata must not infer or advertise Simple identity even for an ordinary or
already-Simple conversation.

Set the shared constant to:

```rust
pub const WORKFLOW_V2_RETIRED_MESSAGE: &str =
    "This workflow is archived and read-only. Create a new conversation and use a new Design.";
```

Replace the same literal in the `#[error(...)]` attributes in `acp/delegation/store.rs`, `acp/delegation/types.rs`, `acp/error.rs`, and `workflow/error.rs`. Update literal assertions in `acp/delegation/listener.rs` and `commands/workflow_completion.rs`; do not change error codes or unrelated navigation fields.

- [ ] **Step 4: Run focused GREEN checks**

```powershell
cargo test --lib --features test-utils simple_and_archived_snapshots_have_stable_navigation_wire_shapes -- --nocapture
cargo test --lib --features test-utils simple_projection_archived -- --nocapture
cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
cargo test --lib --features test-utils completion_entry_guard_preserves_retirement_navigation -- --nocapture
cargo test --lib --features test-utils workflow_v2_retired_serializes_structured_navigation -- --nocapture
```

Expected: all tests PASS. Archived root and child retain history/source navigation, every successor field is retired, no Plan state changes availability, and the mutation code remains `workflow_v2_retired` with the new message.

- [ ] **Step 5: Inspect and commit only projection/fence changes**

```powershell
git diff --check -- src-tauri/src/acp/delegation/workflow/dto.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/error.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/commands/workflow_completion.rs
git add -- src-tauri/src/acp/delegation/workflow/dto.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/error.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/commands/workflow_completion.rs
git diff --cached --name-status
git commit -m "refactor(workflow): retire archived successor metadata"
```

Expected staged paths: exactly the eleven Rust files listed above.

---

### Task 4: Return Simple Persistence To Locator-Only Identity

**Files:**
- Modify: `src-tauri/src/db/migration/m20260811_000001_simple_workflows.rs`
- Delete: `src-tauri/src/db/migration/m20260812_000001_simple_successor_bootstraps.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/simple_workflow.rs`
- Delete: `src-tauri/src/db/entities/simple_successor_bootstrap.rs`
- Modify: `src-tauri/src/db/entities/mod.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/simple.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/web/handlers/simple_workflow.rs`

**Interfaces:**
- Consumes: ordinary `register_simple_workflow(conn, parent_conversation_id, plan_rel_path, progress_rel_path)` callers unchanged.
- Produces: `register_simple_workflow_txn(conn, parent_conversation_id, plan_rel_path, progress_rel_path)` with no source parameter.
- Produces: clean-install `simple_workflows(parent_conversation_id, plan_rel_path, progress_rel_path, created_at, updated_at)` and no bootstrap table.
- Removes: `register_simple_workflow_with_source`, successor Plan eligibility helpers, source-specific `SimpleWorkflowError` variants, the source relation, and bootstrap entity/migration registration.

- [ ] **Step 1: Rewrite migration and store tests for the final schema**

Replace the source-link migration test with schema introspection plus the surviving parent cascade. Assert the complete `PRAGMA table_info` rows (`name`, `type`, `notnull`, `pk`), not only column names:

```rust
#[derive(Debug, PartialEq, Eq)]
struct ColumnInfo {
    name: String,
    col_type: String,
    notnull: i64,
    pk: i64,
}

#[tokio::test]
async fn simple_workflow_migration_is_locator_only_and_has_no_bootstrap_schema() {
    let db = fresh_in_memory_db().await;
    let columns = db
        .conn
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA table_info('simple_workflows')".into(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.try_get::<String>("", "name").unwrap(),
            col_type: row.try_get::<String>("", "type").unwrap(),
            notnull: row.try_get::<i64>("", "notnull").unwrap(),
            pk: row.try_get::<i64>("", "pk").unwrap(),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        columns,
        vec![
            ColumnInfo {
                name: "parent_conversation_id".into(),
                col_type: "INTEGER".into(),
                notnull: 1,
                pk: 1,
            },
            ColumnInfo {
                name: "plan_rel_path".into(),
                col_type: "TEXT".into(),
                notnull: 1,
                pk: 0,
            },
            ColumnInfo {
                name: "progress_rel_path".into(),
                col_type: "TEXT".into(),
                notnull: 1,
                pk: 0,
            },
            ColumnInfo {
                name: "created_at".into(),
                col_type: "TEXT".into(),
                notnull: 1,
                pk: 0,
            },
            ColumnInfo {
                name: "updated_at".into(),
                col_type: "TEXT".into(),
                notnull: 1,
                pk: 0,
            },
        ]
    );
    assert!(!columns.iter().any(|column| column.name == "source_workflow_id"));

    let bootstrap_count: i64 = db
        .conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM sqlite_master WHERE name = 'simple_successor_bootstraps'"
                .into(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "count")
        .unwrap();
    assert_eq!(bootstrap_count, 0);
}
```

Extend the same test to inspect `PRAGMA foreign_key_list('simple_workflows')`: there is exactly one foreign key, targeting `conversation(id)` with `ON DELETE CASCADE`. Inspect `sqlite_master` and assert no `idx_simple_workflows_source`, `idx_simple_successor_bootstrap_successor`, or `idx_simple_successor_bootstrap_source` exists. Retain a real parent-delete assertion that its Simple descriptor is removed.

In `workflow/simple.rs`, keep `simple_workflow_store_normalizes_and_updates_locators_idempotently`, the archived/corrupt mode tests, and ordinary mode resolution. Delete the source-link recreation test and source-specific expectations. Update construction fixtures in project/error/listener tests to omit the source field.

In the Task 1 HTTP no-side-effect helper, remove `simple_successor_bootstraps` from the final table count list because absence is now asserted by the migration test.

- [ ] **Step 2: Run schema/store tests and capture RED**

Run from `src-tauri/`, serially:

```powershell
$env:RUST_MIN_STACK = '16777216'
cargo test --lib --features test-utils simple_workflow_migration_is_locator_only_and_has_no_bootstrap_schema -- --nocapture
cargo test --lib --features test-utils simple_workflow_store -- --nocapture
```

Expected: the migration test FAILS because `source_workflow_id`, its index/foreign key, and `simple_successor_bootstraps` still exist. Store compilation or assertions also fail until the source-aware interface and fixtures are removed.

- [ ] **Step 3: Rewrite the clean-install schema and SeaORM entities**

Make the migration's `up` create exactly this table and no source index:

```sql
CREATE TABLE simple_workflows (
  parent_conversation_id INTEGER PRIMARY KEY NOT NULL,
  plan_rel_path TEXT NOT NULL,
  progress_rel_path TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(parent_conversation_id)
    REFERENCES conversation(id) ON DELETE CASCADE
)
```

Delete `m20260812_000001_simple_successor_bootstraps.rs`, its `mod` declaration, and its `Migrator::migrations()` entry. Delete `entities/simple_successor_bootstrap.rs` and its module export. Remove `source_workflow_id` and the `SourceWorkflow` relation from `entities/simple_workflow.rs`; keep the `ParentConversation` relation and cascade.

- [ ] **Step 4: Simplify registration and every surviving fixture**

Delete these successor-only exports and implementations from `workflow/simple.rs` and `workflow/mod.rs`:

```text
MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES
normalize_simple_successor_plan_locator
eligible_simple_successor_plan
archived_workflow_simple_successor_plan_eligible
register_simple_workflow_with_source
SimpleWorkflowError::SourceWorkflowNotFound
SimpleWorkflowError::SourceWorkflowMismatch
```

Use this transaction interface:

```rust
pub(crate) async fn register_simple_workflow_txn<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    plan_rel_path: &str,
    progress_rel_path: Option<&str>,
) -> Result<SimpleWorkflowRegistration, SimpleWorkflowError>
```

The insert model is exactly:

```rust
simple_workflow::ActiveModel {
    parent_conversation_id: Set(parent_conversation_id),
    plan_rel_path: Set(plan_rel_path),
    progress_rel_path: Set(progress_rel_path),
    created_at: Set(now),
    updated_at: Set(now),
}
```

Keep normalization, default progress path, idempotent no-op detection, locator update, parent existence/root checks, archived mode conflict, and corrupt dual-identity detection unchanged. Remove `source_workflow_id` fields from remaining `ActiveModel` fixtures in `project.rs`, `error.rs`, and `listener.rs` without changing their test purpose.

- [ ] **Step 5: Run focused GREEN plus migration regressions**

```powershell
cargo test --lib --features test-utils simple_workflow_migration_is_locator_only_and_has_no_bootstrap_schema -- --nocapture
cargo test --lib --features test-utils simple_workflow_store -- --nocapture
cargo test --lib --features test-utils simple_projection -- --nocapture
cargo test --test delegation_workflows_migration --features test-utils -- --nocapture
cargo test --test completion_protocol_migrations --features test-utils -- --nocapture
cargo check --features test-utils
```

Expected: all focused tests and both migration integration targets PASS, desktop check exits 0, parent deletion still cascades, locator updates remain idempotent, and unrelated completion-protocol schema remains intact.

- [ ] **Step 6: Run a scoped forbidden-symbol gate before committing**

From the repository root:

```powershell
$forbidden = rg -n '\bsource_workflow_id\b|simple_successor_bootstrap|register_simple_workflow_with_source|eligible_simple_successor_plan|normalize_simple_successor_plan_locator' src-tauri/src/commands/simple_workflow.rs src-tauri/src/acp/delegation/workflow/simple.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/db/entities src-tauri/src/db/migration
if ($LASTEXITCODE -eq 0) { $forbidden; throw 'successor persistence symbols remain' }
if ($LASTEXITCODE -ne 1) { throw 'rg failed while checking successor persistence symbols' }

rg -n 'legacy_source_workflow_id' src-tauri/src/db/entities/delegation_workflow.rs src-tauri/src/db/migration
rg -n 'v2_successor' src-tauri/src src/lib/types.ts
```

Expected: the forbidden scan returns no matches (exit 1), while both preservation scans return matches (exit 0).

- [ ] **Step 7: Inspect and commit only locator/schema changes**

```powershell
git diff --check -- src-tauri/src/db/migration/m20260811_000001_simple_workflows.rs src-tauri/src/db/migration/m20260812_000001_simple_successor_bootstraps.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/simple_workflow.rs src-tauri/src/db/entities/simple_successor_bootstrap.rs src-tauri/src/db/entities/mod.rs src-tauri/src/acp/delegation/workflow/simple.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/web/handlers/simple_workflow.rs
git add -- src-tauri/src/db/migration/m20260811_000001_simple_workflows.rs src-tauri/src/db/migration/m20260812_000001_simple_successor_bootstraps.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/simple_workflow.rs src-tauri/src/db/entities/simple_successor_bootstrap.rs src-tauri/src/db/entities/mod.rs src-tauri/src/acp/delegation/workflow/simple.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/web/handlers/simple_workflow.rs
git diff --cached --name-status
git commit -m "refactor(workflow): remove simple successor persistence"
```

Expected staged paths: exactly twelve entries in `git diff --cached --name-status` — ten modified files plus two deleted files (`m20260812_000001_simple_successor_bootstraps.rs` and `entities/simple_successor_bootstrap.rs`).

---

### Task 5: Verify The Retired Product Boundary And Record Evidence

**Files:**
- Create: `.superpowers/sdd/2026-08-12-simple-successor-creation-retirement/verification-report.md`

**Interfaces:**
- Consumes: the four reviewed implementation commits from Tasks 1-4.
- Produces: one auditable report containing the reviewed commit, exact commands, exit codes, test counts, static-scan outcomes, detached-build commit, and remaining risks.
- Changes no production, test, schema, locale, or configuration file.

This is an evidence task, not another behavior change. Its RED evidence is the four recorded pre-implementation failures from Tasks 1-4; do not manufacture a new failure by weakening or reverting the implementation.

Use this checked-command helper for every native `pnpm`, `cargo`, `git`, and `rg` invocation in Task 5. Capture `$LASTEXITCODE` immediately; PowerShell does not throw on a nonzero native exit by itself. Never let a later command overwrite an unrecorded code.

```powershell
function Assert-NativeExit {
  param(
    [Parameter(Mandatory)][int]$Code,
    [Parameter(Mandatory)][string]$Label,
    [int[]]$Allowed = @(0)
  )
  if ($Allowed -notcontains $Code) {
    throw "$Label failed with exit code $Code (allowed: $($Allowed -join ', '))"
  }
}
```

- [ ] **Step 1: Run repository-wide static contract scans**

From the repository root:

```powershell
$forbiddenFrontend = rg -n 'continueArchivedWorkflowInSimple|SimpleSuccessorResult|archivedContinue|archivedContinuing|archivedOpenSuccessor' src
Assert-NativeExit -Code $LASTEXITCODE -Label 'frontend successor forbidden scan' -Allowed @(1)

$forbiddenRust = rg -n 'SimpleBootstrapPromptSink|admit_pending_simple_successor_bootstrap|admit_simple_successor_bootstrap_after_connect|register_simple_workflow_with_source|eligible_simple_successor_plan|normalize_simple_successor_plan_locator|simple_successor_bootstraps' src-tauri/src
Assert-NativeExit -Code $LASTEXITCODE -Label 'Rust successor forbidden scan' -Allowed @(1)

$continueMatches = rg -n 'continue_archived_workflow_in_simple' src-tauri/src/commands/simple_workflow.rs src-tauri/src/web/handlers/simple_workflow.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs
Assert-NativeExit -Code $LASTEXITCODE -Label 'continue_archived_workflow_in_simple preservation scan'
$retiredMatches = rg -n 'simple_successor_creation_retired|Automatic Simple successor creation is retired; create a new conversation and use a new Design\.' src-tauri/src
Assert-NativeExit -Code $LASTEXITCODE -Label 'retirement contract preservation scan'
$archivedMatches = rg -n 'successor_conversation_id|can_create_simple_successor' src-tauri/src/acp/delegation/workflow/dto.rs src/lib/types.ts
Assert-NativeExit -Code $LASTEXITCODE -Label 'archived compatibility field scan'
$legacyMatches = rg -n 'legacy_source_workflow_id' src-tauri/src/db/entities/delegation_workflow.rs src-tauri/src/db/migration
Assert-NativeExit -Code $LASTEXITCODE -Label 'legacy_source_workflow_id preservation scan'
$v2SuccessorMatches = rg -n 'v2_successor' src-tauri/src src/lib/types.ts
Assert-NativeExit -Code $LASTEXITCODE -Label 'v2_successor preservation scan'
```

Expected: both forbidden scans exit 1 with no matches; every positive preservation/contract scan exits 0 and has matches. Review every stored positive match (`$continueMatches`, `$retiredMatches`, `$archivedMatches`, `$legacyMatches`, `$v2SuccessorMatches`) and confirm it belongs to the stable rejection API, archived compatibility DTO, or explicitly preserved unrelated identity. A missing positive match or a non-0/1 `rg` error blocks the report.

- [ ] **Step 2: Run full frontend verification**

```powershell
pnpm test
$pnpmTestCode = $LASTEXITCODE
Assert-NativeExit -Code $pnpmTestCode -Label 'pnpm test'
pnpm eslint .
$pnpmEslintCode = $LASTEXITCODE
Assert-NativeExit -Code $pnpmEslintCode -Label 'pnpm eslint .'
```

Expected: both commands exit 0. Record Vitest's exact passed-file/test totals, ESLint's warning count, and both captured exit codes in the report; any new error or warning in an owned file blocks completion.

- [ ] **Step 3: Run the full Rust matrix serially**

From `src-tauri/`:

```powershell
$env:RUST_MIN_STACK = '16777216'
cargo test --features test-utils
$desktopTestCode = $LASTEXITCODE
Assert-NativeExit -Code $desktopTestCode -Label 'desktop cargo test'
cargo test --no-default-features --features server --bin codeg-server --lib
$serverTestCode = $LASTEXITCODE
Assert-NativeExit -Code $serverTestCode -Label 'server cargo test'
cargo clippy --all-targets --features test-utils -- -D warnings
$desktopClippyCode = $LASTEXITCODE
Assert-NativeExit -Code $desktopClippyCode -Label 'desktop cargo clippy'
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
$serverClippyCode = $LASTEXITCODE
Assert-NativeExit -Code $serverClippyCode -Label 'server cargo clippy'
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
$mcpClippyCode = $LASTEXITCODE
Assert-NativeExit -Code $mcpClippyCode -Label 'mcp cargo clippy'
Remove-Item Env:RUST_MIN_STACK
```

Expected: all five commands exit 0. Record each captured exit code, exact desktop/server test totals, ignored counts, and zero-warning Clippy outcomes. Run one command at a time and wait 30-60 seconds between status checks; do not launch another Cargo process until the preceding process terminates.

- [ ] **Step 4: Build the frontend from a clean detached worktree**

Run from the repository root after Tasks 1-4 are committed. Resolve and validate the exact temporary target before adding or removing it. The parent of a drive-root checkout such as `D:\MyCodeBuddy` is `D:\`; do not concatenate another separator onto an already-terminated root, and do not use `StartsWith($parent + DirectorySeparatorChar)` (that prefix becomes `D:\\` and rejects the required `D:\MyCodeBuddy-build-...` path).

```powershell
$reviewedCommit = git rev-parse HEAD
Assert-NativeExit -Code $LASTEXITCODE -Label 'git rev-parse HEAD'
$repoRoot = (git rev-parse --show-toplevel).Trim()
Assert-NativeExit -Code $LASTEXITCODE -Label 'git rev-parse --show-toplevel'
$repoParent = Split-Path -Parent $repoRoot
$buildWorktree = Join-Path $repoParent ("MyCodeBuddy-build-" + $reviewedCommit.Substring(0, 12))
$resolvedParent = [System.IO.Path]::GetFullPath($repoParent)
$resolvedBuild = [System.IO.Path]::GetFullPath($buildWorktree)
$relativeToParent = [System.IO.Path]::GetRelativePath($resolvedParent, $resolvedBuild)
if (
  [string]::IsNullOrWhiteSpace($relativeToParent) -or
  [System.IO.Path]::IsPathRooted($relativeToParent) -or
  $relativeToParent -eq '.' -or
  $relativeToParent.StartsWith('..')
) {
  throw "detached build path escaped the repository parent: parent=$resolvedParent build=$resolvedBuild relative=$relativeToParent"
}
if (Test-Path -LiteralPath $resolvedBuild) {
  throw "detached build path already exists: $resolvedBuild"
}

$installCode = $null
$buildCode = $null
$removeCode = $null
$primaryFailure = $null
git worktree add --detach $resolvedBuild $reviewedCommit
Assert-NativeExit -Code $LASTEXITCODE -Label 'git worktree add'
try {
  pnpm --dir $resolvedBuild install --frozen-lockfile
  $installCode = $LASTEXITCODE
  if ($installCode -ne 0) {
    throw "detached pnpm install failed with exit code $installCode"
  }
  pnpm --dir $resolvedBuild build
  $buildCode = $LASTEXITCODE
  if ($buildCode -ne 0) {
    throw "detached pnpm build failed with exit code $buildCode"
  }
} catch {
  $primaryFailure = $_
} finally {
  git worktree remove --force $resolvedBuild
  $removeCode = $LASTEXITCODE
}
$failures = @()
if ($null -ne $primaryFailure) {
  $failures += [string]$primaryFailure.Exception.Message
}
if ($null -eq $removeCode -or $removeCode -ne 0) {
  $failures += "git worktree remove failed with exit code $removeCode"
}
if ($failures.Count -gt 0) {
  throw ($failures -join '; ')
}
```

Expected: `$installCode` and `$buildCode` are 0 at `$reviewedCommit`; all static pages complete; `$removeCode` is 0. Cleanup always runs and is always checked, even when install or build fails. If both a primary command and cleanup fail, the thrown message contains both failures. Record the containment result as accepted plus `$relativeToParent`, and record the three numeric codes. Do not write `$resolvedParent` or `$resolvedBuild` into the verification report; those absolute temporary paths may appear only in operator-facing throw text. Confirm `git worktree list` no longer contains the validated temporary target. Do not delete or modify any other worktree.

- [ ] **Step 5: Write the verification report from observed evidence**

Create the report with these exact sections and only claims supported by terminal output:

```markdown
# Simple Successor Creation Retirement Verification Report

## Reviewed State

Record the branch, full reviewed implementation commit, clean tracked status, and the four Task commit IDs.

## Static Contract

Record both zero-match forbidden scans and all positive compatibility-preservation scans.

## Frontend

Record `pnpm test`, `pnpm eslint .`, and detached `pnpm build` with exact exit codes and totals. For the detached build, record `$relativeToParent`, whether containment was accepted, and `$installCode`/`$buildCode`/`$removeCode`. Omit absolute temporary paths.

## Rust

Record each serial Cargo command, exact exit code, passed/failed/ignored totals, and Clippy warning result.

## Preserved Scope

Confirm `continue_archived_workflow_in_simple` remains registered, archived successor fields remain `null`/`false`, and `legacy_source_workflow_id` plus `v2_successor` remain present.

## Remaining Risks

State concrete residual risks supported by the runs. If every required gate is green, state that no delivery blocker remains; do not erase pre-existing unrelated warnings or untracked files.
```

Use `apply_patch` to create the report. Do not include raw environment secrets, absolute temporary paths (`$resolvedParent`, `$resolvedBuild`, or any other checkout/worktree absolute), or unrelated worktree inventory beyond the preservation statement. Detached-build containment evidence is `$relativeToParent` plus accepted/rejected only.

- [ ] **Step 6: Perform final diff and tracked-clean checks**

```powershell
git diff --check c2954121..HEAD
Assert-NativeExit -Code $LASTEXITCODE -Label 'git diff --check c2954121..HEAD'
git status --short --branch
Assert-NativeExit -Code $LASTEXITCODE -Label 'git status --short --branch'
git log --oneline --decorate -8
Assert-NativeExit -Code $LASTEXITCODE -Label 'git log --oneline --decorate -8'
git status --short --ignored=matching -- .superpowers/sdd/2026-08-12-simple-successor-creation-retirement/verification-report.md
Assert-NativeExit -Code $LASTEXITCODE -Label 'path-scoped ignored status'
```

Expected before staging the report: `git diff --check` exits 0 and no tracked implementation diff remains. The path-scoped ignored-status command shows only `!! .superpowers/sdd/2026-08-12-simple-successor-creation-retirement/` because the repository intentionally ignores new SDD artifacts. Existing `.codex-tmp-*` and `.task-runtimes/` entries remain untouched and untracked.

- [ ] **Step 7: Commit only the evidence report**

```powershell
git add -f -- .superpowers/sdd/2026-08-12-simple-successor-creation-retirement/verification-report.md
Assert-NativeExit -Code $LASTEXITCODE -Label 'git add verification-report.md'
git diff --cached --name-status
Assert-NativeExit -Code $LASTEXITCODE -Label 'git diff --cached --name-status'
git commit -m "docs(workflow): verify simple successor retirement"
Assert-NativeExit -Code $LASTEXITCODE -Label 'git commit verification-report.md'
git status --short --branch
Assert-NativeExit -Code $LASTEXITCODE -Label 'final git status --short --branch'
```

Expected staged path: only the verification report. After commit, tracked status is clean; protected unrelated untracked paths remain visible and unchanged.

## Delivery Gate

Delivery is ready for independent code review only when all of the following are true:

- The old Tauri command and authenticated server route are still registered and return the exact single retirement error.
- The compatibility wrappers have no operation arguments or state dependency.
- Current frontend code has no request helper, result DTO, successor control, pending/error state, locale key, or navigation path.
- Archived reads keep history and compatibility field names but always return `null` and `false` successor values.
- The archived mutation fence keeps code `workflow_v2_retired` and uses the new Design guidance message.
- Fresh databases have a locator-only `simple_workflows` table and no bootstrap table/index/entity.
- Ordinary Simple registration, locator update, projection, delegation, review, recovery, and execution tests remain green.
- Static scans prove the successor engine is absent while `legacy_source_workflow_id` and `v2_successor` remain.
- Full frontend, desktop Rust, server Rust, all three Clippy surfaces, and detached production build are green.
- The final report names the exact reviewed commit and contains no unsupported success claim.

## Execution Handoff

After this plan is committed, choose one execution mode:

1. **Subagent-Driven (recommended):** use `superpowers:subagent-driven-development`; dispatch a fresh implementer for each Task, run spec-compliance review and code-quality review before advancing, and reuse the Task's implementer for any fix round.
2. **Inline Execution:** use `superpowers:executing-plans`; execute the Tasks in this session in order, stopping at each Task commit for the same verification and review checkpoints.

Do not execute Tasks 2-4 concurrently. They touch shared DTOs, fixtures, and schema-generated Rust types, so the durable order is Task 1, Task 2, Task 3, Task 4, then Task 5.
