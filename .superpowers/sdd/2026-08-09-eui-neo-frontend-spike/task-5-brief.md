# Task 5 Brief

### Task 5: Add Workspace, Conversation, Connection, History, and Send Operations

**Milestone:** M3 plus send admission needed by M4.

**Files:**

- Expand: `src-tauri/src/commands/eui_facade.rs`
- Expand: `src-tauri/codeg-eui-core/src/commands.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Expand: `src-tauri/codeg-eui-core/src/abi.rs`
- Modify: `codeg-eui/app/bridge/codeg_eui_bridge.h`
- Test: unit tests in `src-tauri/src/commands/eui_facade.rs`
- Test: `src-tauri/codeg-eui-core/tests/session_contract.rs`

**Interfaces:**

- Consumes: `open_folder_core`, `create_project_conversation_core`, `get_folder_conversation_with_live_core`, `build_acp_launch_inputs`, `verify_agent_installed`, `ConnectionManager::spawn_agent`, and `send_prompt_linked_with_message_id`.
- Produces: `EuiWorkspace`, `EuiSessionSummary`, `EuiSessionSelection`, `set_eui_workspace`, `create_eui_conversation`, `create_eui_session`, `select_eui_session`, `send_eui_message`; all corresponding ABI enqueue functions; model `selection_epoch` increments on workspace/session change.
- Session ownership: EUI connections use owner label `"eui"`, user launch context from DB, no delegation route override, and only Grok/Codex.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 5 | Workspace, conversation, connection, history, send | session facade, bridge commands, DB/manager tests | `concurrency_lifecycle`: process spawn, linked send, selection epochs | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`, `broad_production_surface=1`; total `5` | `high`: lifecycle hard trigger and soft threshold | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write workspace and persisted-session facade tests**

Use a fresh DB and a real temporary directory:

```rust
#[tokio::test]
async fn workspace_and_conversation_reuse_existing_database_cores() {
    let state = eui_test_state().await;
    let workspace = set_eui_workspace(&state, fixture_dir()).await.unwrap();
    assert_eq!(workspace.path, fixture_dir().canonicalize().unwrap());
    let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Grok)
        .await.unwrap();
    assert!(row.conversation_id > 0);
    assert_eq!(row.agent_type, AgentType::Grok);
    assert_eq!(state.db_count_regular_conversations().await, 1);
}
```

Add invalid/non-directory workspace tests, Codex/Grok acceptance, unsupported agent rejection, and history projection from `get_folder_conversation_with_live_core` using `HistoryLoadOpts { user_turn_limit: Some(100), before_turn_id: None }`.

- [ ] **Step 2: Run facade tests to verify RED**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests::workspace_and_conversation -- --nocapture
cd ..
```

Expected: FAIL because the session facade functions do not exist.

- [ ] **Step 3: Implement workspace and conversation DTOs**

Canonicalize and verify an existing directory before `open_folder_core`. Create the DB row with `create_project_conversation_core(&state.db.conn, workspace.folder_id, agent_type, None, None)`. The DTOs carry only `folder_id`, absolute path, `conversation_id`, title, agent, status, external session ID, and transcript turns serialized as backend `MessageTurn` JSON. Do not expose `AppState`, DB connections, or parser objects.

- [ ] **Step 4: Write spawn/send tests with deterministic manager seams**

Use `test-utils` manager connections or an injected `EuiSessionOps` implementation to prove the exact orchestration:

```rust
#[tokio::test]
async fn create_session_builds_launch_inputs_before_spawn_and_binds_on_send() {
    let ops = RecordingSessionOps::default();
    let bridge = TestBridge::with_session_ops(ops.clone());
    let create_id = bridge.enqueue_create_session("codex").unwrap();
    let create = bridge.wait_completion(create_id).await;
    assert_eq!(create.status, CompletionStatus::Ok);
    assert_eq!(ops.calls(), ["verify_installed", "build_launch_inputs", "spawn_agent"]);
    let send_id = bridge.enqueue_send("hello").unwrap();
    assert_eq!(ops.last_send().unwrap().conversation_id, create.conversation_id());
    assert_eq!(bridge.wait_completion(send_id).await.status, CompletionStatus::Ok);
}
```

Add a test where session selection changes during slow create/send and the old completion arrives once with `stale`.

- [ ] **Step 5: Implement create/select/send through shared core paths**

`create_eui_session` verifies installation, builds launch inputs with `AcpRouteRequest::root(Some(conversation_id), None)`, loads `user_launch_context_from_db`, and calls `spawn_agent` with workspace path and owner `"eui"`. `select_eui_session` loads the persisted row/history, reuses `find_connection_by_conversation_id` when live, or spawns with the row's `external_id` when resuming. `send_eui_message` builds exactly one `PromptInputBlock::Text`, uses a UUID client message ID, and calls `send_prompt_linked_with_message_id` with folder/conversation IDs.

- [ ] **Step 6: Expose all async session ABI calls**

Implement `set_workspace`, `create_session`, `select_session`, and `send_user_message` with Task 3 validation/acceptance. On successful create, the completion JSON contains `conversationId` and `connectionId`. On selection, update the model's transcript/session list and increment `selection_epoch` before launching slow work so prior operations become stale. On send acceptance, record native `t0_ns` immediately after enqueue succeeds; Task 9 consumes the marker.

- [ ] **Step 7: Run M3/session verification**

```bash
cd src-tauri
cargo test --lib --features test-utils commands::eui_facade::tests -- --nocapture
cd ..
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test session_contract
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bridge_contract
```

Expected: invalid workspace fails without a row, only Grok/Codex create, history is backend-derived, launch order is recorded, linked sends carry the selected IDs, poll remains non-blocking, and selection changes mark old completions stale exactly once.

- [ ] **Step 8: Commit and prepare the Task 5 review package**

```bash
git add --dry-run -- src-tauri/src/commands/eui_facade.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/session_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git add -- src-tauri/src/commands/eui_facade.rs src-tauri/codeg-eui-core/src/commands.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/abi.rs src-tauri/codeg-eui-core/tests/session_contract.rs codeg-eui/app/bridge/codeg_eui_bridge.h
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add workspace and session command loop"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/src/commands/eui_facade.rs src-tauri/codeg-eui-core codeg-eui/app/bridge/codeg_eui_bridge.h
```

Expected package: one session-loop commit with DB, launch, selection, history, and send tests. Route it to both high-risk reviewers, then continue directly to Task 6.

