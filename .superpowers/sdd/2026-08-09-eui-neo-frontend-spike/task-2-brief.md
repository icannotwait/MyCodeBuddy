# Task 2 Brief

### Task 2: Pin the Isolated Data Root and Construct the EUI AppState Profile

**Milestone:** M1.

**Files:**

- Create: `src-tauri/codeg-eui-core/src/data_root.rs`
- Create: `src-tauri/codeg-eui-core/src/bootstrap.rs`
- Modify: `src-tauri/codeg-eui-core/src/lib.rs`
- Modify: `src-tauri/codeg-eui-core/Cargo.toml`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/document_translate/service.rs`
- Modify: `src-tauri/src/logging/init.rs`
- Test: `src-tauri/codeg-eui-core/tests/data_root_isolation.rs`
- Test: `src-tauri/codeg-eui-core/tests/bootstrap_profile.rs`

**Interfaces:**

- Consumes: `codeg_lib::{db::init_database,logging::init::init_eui}`, `InternalAgentSessionRegistry::load`, `EventEmitter::web_only`, and dormant core constructors.
- Produces: `resolve_eui_data_root(&EuiRootInputs) -> Result<PathBuf, DataRootError>`, process-once `pin_eui_data_root(PathBuf) -> Result<(), DataRootError>`, `logging::init::init_eui() -> LogGuard`, `AppState::new_eui(db, data_dir) -> Result<AppState, AppCommandError>`, and `EuiBootstrap::start() -> Result<Self, BootstrapError>`.
- Invariant: the first successful pin is immutable; re-init with the same normalized path succeeds, while a different path returns `DataRootError::AlreadyPinned`.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2 | Pin isolated root and construct EUI AppState | resolver, bootstrap, AppState profile, isolation tests | `security_trust_boundary`: ambient app roots/credentials; `concurrency_lifecycle`: pin before logging/runtime | `multiple_ownership_modules=1`, `shared_interface=1`; total `2` | `high`: both hard triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write data-root precedence and isolation tests**

Use a pure input struct so precedence tests do not race process environment:

```rust
#[test]
fn ambient_main_data_dir_and_codeg_home_never_choose_the_eui_root() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: None,
        xdg_data_home: Some(PathBuf::from("/tmp/xdg")),
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };
    assert_eq!(resolve_eui_data_root(&inputs).unwrap(),
               PathBuf::from("/tmp/xdg/codeg-eui"));
}

#[test]
fn explicit_eui_root_is_absolutized() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: Some(PathBuf::from("relative-eui")),
        xdg_data_home: Some(PathBuf::from("/tmp/ignored")),
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };
    assert_eq!(resolve_eui_data_root(&inputs).unwrap(),
               PathBuf::from("/work/relative-eui"));
}
```

The integration test serializes process-env mutation with one static mutex, sets `CODEG_DATA_DIR=<main>`, `CODEG_HOME=<main-home>`, and `CODEG_EUI_DATA_DIR=<eui>`, starts bootstrap, and asserts only `<eui>/codeg.db` plus `<eui>/logs` are created; `<main>/codeg.db` and `<main-home>/logs` remain absent. A second RED case calls `codeg_eui_init(<argument-root>, len)` while `CODEG_EUI_DATA_DIR=<env-root>` and expects `<argument-root>/codeg.db`, no write under `<env-root>`, a successful two-phase shutdown, and a stable init error when later re-init supplies a different normalized argument root.

- [ ] **Step 2: Run the isolation test to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test data_root_isolation
```

Expected: FAIL because the resolver/bootstrap do not exist.

- [ ] **Step 3: Implement the pure resolver and one-time process pin**

Use this exact precedence and process mutation order:

```rust
pub fn resolve_eui_data_root(input: &EuiRootInputs) -> Result<PathBuf, DataRootError> {
    let candidate = input.codeg_eui_data_dir.as_ref().filter(|p| !p.as_os_str().is_empty())
        .cloned()
        .or_else(|| input.xdg_data_home.as_ref().map(|p| p.join("codeg-eui")))
        .or_else(|| input.home.as_ref().map(|p| p.join(".local/share/codeg-eui")))
        .ok_or(DataRootError::HomeUnavailable)?;
    Ok(if candidate.is_absolute() { candidate } else { input.cwd.join(candidate) })
}

pub fn pin_eui_data_root(root: PathBuf) -> Result<(), DataRootError> {
    let absolute = absolutize_without_requiring_existence(&root)?;
    verify_or_set_process_pin(&absolute)?;
    std::env::remove_var("CODEG_HOME");
    std::env::set_var("CODEG_DATA_DIR", &absolute);
    Ok(())
}
```

If the public ABI data-dir argument is non-empty, validate UTF-8/bounds, absolutize it against the captured startup working directory, and use it as the authoritative EUI root even when `CODEG_EUI_DATA_DIR` differs. Only an empty argument consults `CODEG_EUI_DATA_DIR` and then the documented XDG/home defaults. Both paths call `pin_eui_data_root`, remove ambient `CODEG_HOME`, and overwrite `CODEG_DATA_DIR`. The C++ product entrypoint passes empty; ABI tests exercise the authoritative non-empty path.

- [ ] **Step 4: Write the failing EUI AppState profile test**

`bootstrap_profile.rs` must assert:

```rust
let bootstrap = EuiBootstrap::start_for_test(temp.path()).await.unwrap();
assert_eq!(bootstrap.state.data_dir, temp.path());
assert!(matches!(bootstrap.state.emitter, EventEmitter::WebOnly { .. }));
assert_eq!(bootstrap.state.connection_manager.list_connections().await.len(), 0);
assert!(!bootstrap.started_services.web_server);
assert!(!bootstrap.started_services.auto_title);
assert!(!bootstrap.started_services.automation);
assert!(!bootstrap.started_services.chat_channels);
assert!(!bootstrap.started_services.pet_mapper);
```

- [ ] **Step 5: Add `AppState::new_eui` with disabled auxiliary services**

Refactor the test constructor only enough to share field assembly. Add `DocumentTranslationService::new_disabled` as the production-visible replacement for the test-only `new_inert`, and retain `new_inert` as a test alias. Build the dormant auto-title coordinator with `build_production_coordinator` but never call `recover_and_start`; construct the reference registry but never spawn `run_reference_search_sweeper`; construct the completion dispatcher but never call `spawn_completion_outbox_dispatcher`. `new_eui` must use this complete field map:

```rust
pub async fn new_eui(db: AppDatabase, data_dir: PathBuf)
    -> Result<Self, AppCommandError>
{
    let broadcaster = Arc::new(WebEventBroadcaster::new());
    let metrics = Arc::new(crate::acp::EventBusMetrics::default());
    let bus = Arc::new(InternalEventBus::new(metrics));
    let emitter = EventEmitter::web_only(broadcaster.clone(), bus.clone());
    let manager = ConnectionManager::new();
    let internal_sessions = InternalAgentSessionRegistry::load(
        db.conn.clone(), &data_dir).await.map_err(AppCommandError::from)?;
    let chat_channel_manager = default_chat_channel_manager();
    let conversation_experience_gate =
        Arc::new(ConversationExperienceMutationGate::default());
    let db_handle = Arc::new(AppDatabase { conn: db.conn.clone() });
    let auto_title_coordinator = crate::auto_title::build_production_coordinator(
        Arc::clone(&db_handle),
        manager.clone_ref(),
        chat_channel_manager.clone_ref(),
        EventEmitter::Noop,
        Arc::clone(&conversation_experience_gate),
    );
    let document_translation = DocumentTranslationService::new_disabled(
        Arc::clone(&db_handle),
    );
    let reference_search_registry = ReferenceSearchRegistry::new(
        crate::commands::conversation_experience::DEFAULT_REFERENCE_SEARCH_LIMIT,
        Arc::new(crate::reference_search::ProductionReferenceSourceFactory {
            db: db.conn.clone(),
        }),
    );
    let stack = build_delegation_stack(&manager, db.conn.clone(), data_dir.clone());
    let completion_protocol_rollout = Arc::new(
        crate::acp::delegation::workflow::CompletionProtocolRolloutConfig::default(),
    );
    manager.install_completion_protocol_runtime(
        Arc::clone(&completion_protocol_rollout),
        Arc::clone(&stack.metrics),
    );
    let completion_outbox_dispatcher = Arc::new(
        CompletionOutboxDispatcher::new(db_handle, emitter.clone())
            .with_metrics(Arc::clone(&stack.metrics)),
    );

    Ok(Self {
        db,
        connection_manager: manager,
        terminal_manager: default_terminal_manager(),
        event_broadcaster: broadcaster,
        acp_event_bus: bus,
        emitter,
        data_dir,
        internal_sessions,
        auto_title_coordinator,
        document_translation,
        conversation_experience_gate,
        reference_search_registry,
        web_server_state: WebServerState::new(),
        chat_channel_manager,
        workspace_transfer: Arc::new(WorkspaceTransferManager::new_from_env()),
        pet_state: crate::pet_state_mapper::new_pet_state_handle(),
        delegation_broker: stack.broker,
        continuation_coordinator: stack.continuation_coordinator,
        delegation_metrics: stack.metrics,
        completion_protocol_rollout,
        completion_outbox_dispatcher,
        delegation_runtime_settings: stack.runtime_settings,
        delegation_tokens: stack.tokens,
        delegation_leases: stack.leases,
        delegation_socket_path: stack.socket_path,
        feedback_config: stack.feedback,
        question_config: stack.ask,
        session_info_config: stack.sessions,
        system_op_lock: default_system_op_lock(),
        update_state: default_update_state(),
    })
}
```

The delegation objects exist because shared launch helpers require them, but do not call the listener, supervisor, outbox-dispatcher, chat-channel, auto-title, translation, reference-search, pet, web-server, or updater start functions from this profile.

- [ ] **Step 6: Implement bootstrap ordering**

Add this production logging entry point, then have `EuiBootstrap::start` run synchronously on the eventual UI thread in this order: resolve root, pin `CODEG_HOME`/`CODEG_DATA_DIR`, create directories, call `init_eui`, create Tokio runtime, run `init_database`, apply persisted log level, call `AppState::new_eui`, then return the state/runtime/log guard. No `tokio::spawn` occurs before the env pin.

```rust
pub fn init_eui() -> LogGuard {
    init_with_file("codeg-eui")
}
```

- [ ] **Step 7: Run M1 verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test data_root_isolation -- --test-threads=1
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bootstrap_profile -- --test-threads=1
cargo check --manifest-path src-tauri/codeg-eui-core/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --lib
```

Expected: the EUI root is absolute and exclusive, SQLite/logs use it, the state is `WebOnly` with zero sessions, no excluded service is started, and no Tauri dependency is enabled.

- [ ] **Step 8: Commit and prepare the Task 2 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/data_root.rs src-tauri/codeg-eui-core/src/bootstrap.rs src-tauri/codeg-eui-core/tests/data_root_isolation.rs src-tauri/codeg-eui-core/tests/bootstrap_profile.rs src-tauri/src/app_state.rs src-tauri/src/document_translate/service.rs src-tauri/src/logging/init.rs
git add -- src-tauri/codeg-eui-core/Cargo.toml src-tauri/codeg-eui-core/src/lib.rs src-tauri/codeg-eui-core/src/data_root.rs src-tauri/codeg-eui-core/src/bootstrap.rs src-tauri/codeg-eui-core/tests/data_root_isolation.rs src-tauri/codeg-eui-core/tests/bootstrap_profile.rs src-tauri/src/app_state.rs src-tauri/src/document_translate/service.rs src-tauri/src/logging/init.rs
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add isolated core bootstrap profile"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core src-tauri/src/app_state.rs src-tauri/src/document_translate/service.rs src-tauri/src/logging/init.rs
```

Expected package: one commit proving root precedence, `CODEG_HOME` clearing, SQLite/log isolation, `WebOnly` construction, and excluded-service non-start. Route it to both high-risk reviewers, then continue directly to Task 3.

