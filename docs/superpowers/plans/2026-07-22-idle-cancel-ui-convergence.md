# Idle Cancel UI Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure an explicit Cancel reasserts `Connected` from the ACP idle
loop so a main-window composer with a stale `prompting` projection unlocks.

**Architecture:** Keep the frontend and `ConnectionControl` protocol unchanged.
The idle cancellation branch in the backend will publish one authoritative
`AcpEvent::StatusChanged { Connected }` after local permission and terminal
cleanup but before delegation cascade work. The existing active-turn cancel
path continues to own `TurnComplete(cancelled)`.

**Tech Stack:** Rust 2021, Tokio, ACP session loop, `SessionState`, Rust unit
tests in `src-tauri/src/acp/connection.rs`.

## Global Constraints

- Preserve backend authority; do not optimistically mutate frontend composer
  state.
- Do not add an acknowledgement field to `ConnectionControl` or change public
  Tauri/web API shapes.
- The idle branch emits `StatusChanged(Connected)` but never synthesizes a
  `TurnComplete`.
- Emit the status before awaiting `DelegationBroker::cancel_by_parent_turn`.
- Use the existing `run_suspension_test_loop` integration harness and `TDD`:
  observe the test fail before production code changes.
- Keep all implementation work in the isolated worktree branch
  `fix/idle-cancel-ui-convergence`.

---

## File Structure

- Modify: `src-tauri/src/acp/connection.rs`
  - Owns the ACP idle command/control loop and its in-file integration test
    harness.
  - Adds the convergence event to the idle Cancel branch.
  - Adds a test proving the real loop emits the event and remains usable.

No frontend files, public type definitions, database migrations, or new source
files are required.

### Task 1: Reassert Idle Connection State on Cancel

**Files:**

- Modify: `src-tauri/src/acp/connection.rs`
- Test: `src-tauri/src/acp/connection.rs`

**Interfaces:**

- Consumes: `ConversationInput::Control(ConnectionControl::Cancel)`,
  `cancel_pending_permissions`, `TerminalRuntime::release_all_for_session`,
  and `emit_with_state`.
- Produces: one `AcpEvent::StatusChanged { status: ConnectionStatus::Connected }`
  in the idle Cancel path before delegation cleanup.
- Preserves: the active prompt loop's existing `TurnComplete(cancelled)` path
  and all `ConnectionControl` variants.

- [ ] **Step 1: Add the failing integration regression test**

Insert the following test after
`delegation_suspend_idle_reverse_closed_lanes_exit_hung_ancillary` in
`src-tauri/src/acp/connection.rs`:

```rust
    #[tokio::test]
    async fn idle_user_cancel_reasserts_connected_without_turn_complete() {
        use std::sync::atomic::AtomicUsize;

        let mut idle_state = SessionState::new(
            "parent-conn".into(),
            AgentType::Codex,
            None,
            "test".into(),
            None,
        );
        idle_state.status = ConnectionStatus::Connected;
        let state = Arc::new(RwLock::new(idle_state));
        let (broker, _spawner, _task_id) =
            delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: modes.clone(),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        let event_seq_before_cancel = state.read().await.event_seq;
        control_tx.send(ConnectionControl::Cancel).await.unwrap();

        for _ in 0..200 {
            let has_connected = state
                .read()
                .await
                .recent_events_after(event_seq_before_cancel)
                .unwrap_or_default()
                .iter()
                .any(|event| {
                    matches!(
                        &event.payload,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Connected
                        }
                    )
                });
            if has_connected {
                break;
            }
            tokio::task::yield_now().await;
        }

        let events = state
            .read()
            .await
            .recent_events_after(event_seq_before_cancel)
            .unwrap_or_default();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        &event.payload,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Connected
                        }
                    )
                })
                .count(),
            1,
            "idle Cancel must publish one authoritative Connected assertion"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(&event.payload, AcpEvent::TurnComplete { .. })),
            "idle Cancel must not synthesize a TurnComplete"
        );
        assert_eq!(state.read().await.status, ConnectionStatus::Connected);

        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "idle-cancel-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("idle Cancel set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        modes
            .lock()
            .unwrap()
            .remove(0)
            .respond(sacp::schema::SetSessionModeResponse::new())
            .expect("idle Cancel must leave the command loop usable");
        wait_for_suspension_mode_event(&state, "idle-cancel-mode").await;

        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }
```

- [ ] **Step 2: Run the focused test and confirm RED**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils idle_user_cancel_reasserts_connected_without_turn_complete --lib
```

Expected: the new test fails at the connected-event count because the current
idle Cancel branch emits no `StatusChanged(Connected)` event.

- [ ] **Step 3: Add the minimal backend convergence event**

In the idle `ConversationInput::Control(ConnectionControl::Cancel)` branch in
`src-tauri/src/acp/connection.rs`, insert this block immediately after
`release_all_for_session(...).await` and before the delegation-cascade comment:

```rust
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Connected,
                    },
                )
                .await;
```

Do not alter the active-loop `finalize_active_user_cancel` implementation, the
broker call, or any frontend state.

- [ ] **Step 4: Re-run the focused test and confirm GREEN**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils idle_user_cancel_reasserts_connected_without_turn_complete --lib
```

Expected: one passed test; it proves the event is present in `SessionState`, no
`TurnComplete` was emitted, and the loop accepts the subsequent mode command.

- [ ] **Step 5: Run scoped regression tests**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils delegation_suspend --lib
```

Expected: all matching suspension/cancellation tests pass.

- [ ] **Step 6: Format and run build-quality checks**

Run from `src-tauri/`:

```powershell
cargo fmt --check
cargo check
cargo check --no-default-features --bin codeg-server
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

Expected: every command exits with code 0 and reports no Clippy warnings.

- [ ] **Step 7: Commit the fix**

```powershell
git add src-tauri/src/acp/connection.rs
git commit -m "fix(acp): reassert idle cancel connection status"
```

Expected: the worktree contains one implementation commit with only the ACP
loop and its regression test changed.
