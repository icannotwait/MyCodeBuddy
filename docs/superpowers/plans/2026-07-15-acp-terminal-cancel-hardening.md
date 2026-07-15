# ACP Terminal Cancel Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ACP terminal cancellation safe across timeout and notification races, and clear cancelled permission state immediately in backend and frontend clients.

**Architecture:** Each removed terminal is owned by a spawned kill task; `release_all_for_session` only bounds waiting on task handles, so timing out cannot cancel the kill/wait/drain/publish transition. Exit publication uses a retained Tokio `watch` value. Cancellation drains permission responders through one helper that emits `PermissionResolved`, while the frontend also clears permission state on `turn_complete`.

**Tech Stack:** Rust 2021, Tokio 1.49, SACP 11, React 19, TypeScript strict, Vitest.

## Global Constraints

- Preserve the ACP request and response schema.
- Keep `kill_tree` process-tree termination on Windows and Unix.
- Keep `TurnComplete { cancelled }` before terminal cleanup.
- Bound all terminal cleanup for one session by `RELEASE_KILL_BOUND` rather than multiplying the bound by terminal count.
- Do not modify or stage unrelated dirty-worktree files.
- Use TDD: every production behavior change follows a failing targeted test.

---

### Task 1: Make terminal exit publication cancellation-safe

**Files:**
- Modify: `src-tauri/src/acp/terminal_runtime.rs:13-172`
- Modify: `src-tauri/src/acp/terminal_runtime.rs:700-742`
- Test: `src-tauri/src/acp/terminal_runtime.rs:1549-end`

**Interfaces:**
- Consumes: `TerminalInstance::kill_command() -> Result<(), TerminalRuntimeError>` and `RELEASE_KILL_BOUND`.
- Produces: `exit_status_tx: watch::Sender<Option<TerminalExitStatus>>`, retained publication through `publish_exit_status`, and session-wide bounded owned kill tasks in `release_all_for_session`.

- [ ] **Step 1: Add a failing timeout-cancellation regression test**

Add a current-thread Tokio test that creates a long-running terminal, appends a never-finishing reader task, pauses Tokio time, starts `release_all_for_session`, waits until `child` is `None`, advances through `RELEASE_KILL_BOUND`, then asserts the terminal snapshot eventually contains `exit_status`:

```rust
#[tokio::test]
async fn release_timeout_does_not_cancel_exit_publication() {
    tokio::time::pause();
    let runtime = Arc::new(test_runtime(platform_test_shell()));
    let session_id = SessionId::new("release-timeout-publication");
    let response = runtime
        .create_terminal(CreateTerminalRequest::new(
            session_id.clone(),
            platform_long_running_command(),
        ))
        .await
        .expect("create terminal");
    let terminal = runtime
        .find_terminal(response.terminal_id.0.as_ref(), session_id.0.as_ref())
        .await
        .expect("terminal exists");
    terminal
        .reader_handles
        .lock()
        .await
        .push(tokio::spawn(std::future::pending()));

    let release_runtime = Arc::clone(&runtime);
    let release_session = session_id.0.to_string();
    let release = tokio::spawn(async move {
        release_runtime.release_all_for_session(&release_session).await;
    });

    let wall_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while terminal.child.lock().await.is_some() {
        assert!(std::time::Instant::now() < wall_deadline, "kill did not clear child");
        tokio::task::yield_now().await;
    }
    // Keep publication blocked after the child has been cleared. The pending
    // reader gives this task time to acquire the snapshot lock first.
    let snapshot_guard = terminal.snapshot.lock().await;
    for _ in 0..3 {
        tokio::time::advance(READER_DRAIN_GRACE).await;
        tokio::task::yield_now().await;
    }
    tokio::time::advance(RELEASE_KILL_BOUND).await;
    release.await.expect("release task join");
    drop(snapshot_guard);

    for _ in 0..100 {
        if terminal.snapshot().await.exit_status.is_some() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("detached kill task did not publish exit status");
}
```

Use these helpers rather than duplicating command strings:

```rust
#[cfg(windows)]
fn platform_long_running_command() -> String {
    "Start-Sleep -Seconds 3600".to_string()
}

#[cfg(unix)]
fn platform_long_running_command() -> String {
    "sleep 3600".to_string()
}
```

- [ ] **Step 2: Run the timeout test and verify RED**

Run:

```powershell
cargo test --no-default-features --lib terminal_runtime::tests::release_timeout_does_not_cancel_exit_publication -- --nocapture
```

Expected: FAIL at `detached kill task did not publish exit status`, proving the direct timeout dropped the only publisher.

- [ ] **Step 3: Spawn owned kill tasks and bound only their handles**

Change `release_all_for_session` to start every task first, log `kill_command` errors inside the task, and use one shared deadline while awaiting handles:

```rust
let kill_tasks = removed
    .into_iter()
    .map(|terminal| {
        tokio::spawn(async move {
            if let Err(err) = terminal.kill_command().await {
                tracing::error!("[ACP] Failed to release terminal during cleanup: {err:?}");
            }
        })
    })
    .collect::<Vec<_>>();
let deadline = tokio::time::Instant::now() + RELEASE_KILL_BOUND;
let mut timed_out = 0usize;
for task in kill_tasks {
    match tokio::time::timeout_at(deadline, task).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::error!("[ACP] terminal cleanup task failed: {err}"),
        Err(_) => timed_out += 1,
    }
}
if timed_out > 0 {
    tracing::error!(
        "[ACP] {timed_out} terminal cleanup task(s) exceeded {RELEASE_KILL_BOUND:?}; continuing in background"
    );
}
```

- [ ] **Step 4: Run the timeout test and verify GREEN**

Run the Step 2 command again. Expected: PASS.

- [ ] **Step 5: Add a failing retained-publication test**

Add a test that lets a short command publish its exit before subscribing, then reads the desired retained signal:

```rust
#[tokio::test]
async fn exit_status_signal_retains_publication_for_late_subscriber() {
    let runtime = test_runtime(platform_test_shell());
    let session_id = SessionId::new("retained-exit-status");
    let response = runtime
        .create_terminal(CreateTerminalRequest::new(session_id.clone(), "exit 0"))
        .await
        .expect("create terminal");
    let terminal = runtime
        .find_terminal(response.terminal_id.0.as_ref(), session_id.0.as_ref())
        .await
        .expect("terminal exists");
    let expected = terminal.wait_for_exit().await.expect("wait for exit");
    let observed = terminal.exit_status_tx.subscribe().borrow().clone();
    assert_eq!(observed, Some(expected));
    runtime.release_all_for_session(session_id.0.as_ref()).await;
}
```

- [ ] **Step 6: Run the retained-publication test and verify RED**

Run:

```powershell
cargo test --no-default-features --lib terminal_runtime::tests::exit_status_signal_retains_publication_for_late_subscriber -- --nocapture
```

Expected: compilation failure because `TerminalInstance::exit_status_tx` does not exist yet.

- [ ] **Step 7: Replace `Notify` with retained `watch` state**

Import `tokio::sync::{watch, Mutex}`. Initialize `watch::channel(None)` in `TerminalInstance::new`, publish only the first status to both `snapshot.exit_status` and `exit_status_tx`, and wait with `timeout_at(deadline, receiver.changed())`. Treat a closed channel as `TerminalRuntimeError::Internal`; retain the existing ten-second corruption bound.

- [ ] **Step 8: Make the original wait/release regression deterministic**

Run `concurrent_wait_and_session_release_completes_promptly` on the current-thread runtime. Keep an `Arc<TerminalInstance>` and replace the 100ms sleep with:

```rust
let wall_deadline = std::time::Instant::now() + Duration::from_secs(5);
loop {
    match terminal.child.try_lock() {
        Ok(guard) => {
            drop(guard);
            assert!(
                std::time::Instant::now() < wall_deadline,
                "waiter never acquired the child mutex"
            );
            tokio::task::yield_now().await;
        }
        Err(_) => break,
    }
}
```

On a current-thread runtime, an unavailable mutex proves the waiter is suspended inside `child.wait()` while holding it, because `refresh_exit_status` never yields while holding that mutex.

- [ ] **Step 9: Run all terminal-runtime tests**

Run:

```powershell
cargo test --no-default-features --lib terminal_runtime::tests -- --nocapture
```

Expected: all filtered tests pass with no leaked long-running process.

- [ ] **Step 10: Commit Task 1**

```powershell
git add -- src-tauri/src/acp/terminal_runtime.rs
git commit -m "fix(acp): make terminal cleanup cancellation safe"
```

### Task 2: Clear cancelled permission state before terminal cleanup

**Files:**
- Modify: `src-tauri/src/acp/connection.rs:962-964`
- Modify: `src-tauri/src/acp/connection.rs:2850-2921`
- Modify: `src-tauri/src/acp/connection.rs:4363-4560`
- Test: `src-tauri/src/acp/connection.rs` test module
- Modify: `src/contexts/acp-connections-context.tsx:3190-3198`
- Test: `src/contexts/acp-connections-context.test.tsx:353-578`

**Interfaces:**
- Consumes: `PendingPermissions`, `RequestPermissionOutcome::Cancelled`, `emit_with_state`.
- Produces: `cancel_pending_permissions(state, emitter, perms)` and unconditional frontend `PERMISSION_CLEARED` dispatch on `turn_complete`.

- [ ] **Step 1: Add a failing backend event-sequence test**

Add this test with `EventEmitter::Noop`; it asserts two supplied IDs become two ordered `AcpEvent::PermissionResolved` entries in `SessionState::recent_events_after(0)`:

```rust
#[tokio::test]
async fn cancelled_permission_ids_emit_resolution_events() {
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-permissions".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    )));
    let emitter = EventEmitter::Noop;

    emit_cancelled_permission_events(
        &state,
        &emitter,
        vec!["p-1".to_string(), "p-2".to_string()],
    )
    .await;

    let guard = state.read().await;
    let resolved = guard
        .recent_events_after(0)
        .expect("events recorded")
        .iter()
        .filter_map(|event| match &event.payload {
            AcpEvent::PermissionResolved { request_id } => Some(request_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved, vec!["p-1", "p-2"]);
}
```

- [ ] **Step 2: Run the backend test and verify RED**

Run:

```powershell
cargo test --no-default-features --lib connection::tests::cancelled_permission_ids_emit_resolution_events -- --nocapture
```

Expected: compilation failure because `emit_cancelled_permission_events` does not exist.

- [ ] **Step 3: Implement one permission-drain helper and reorder cancel paths**

Implement:

```rust
async fn emit_cancelled_permission_events(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request_ids: impl IntoIterator<Item = String>,
) {
    for request_id in request_ids {
        emit_with_state(
            state,
            emitter,
            AcpEvent::PermissionResolved { request_id },
        )
        .await;
    }
}

async fn cancel_pending_permissions(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
) {
    let drained = perms.lock().await.drain().collect::<Vec<_>>();
    let mut request_ids = Vec::with_capacity(drained.len());
    for (request_id, responder) in drained {
        let _ = responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ));
        request_ids.push(request_id);
    }
    emit_cancelled_permission_events(state, emitter, request_ids).await;
}
```

The event helper emits one `PermissionResolved` per ID after the permission mutex is released. Replace all three duplicated drain loops. In the mid-prompt path use `TurnComplete -> tracked_terminal_tool_calls.clear() -> cancel_pending_permissions -> release_all_for_session`; use permission cancellation before terminal release in disconnect and idle-cancel paths too.

- [ ] **Step 4: Run the backend test and verify GREEN**

Run the Step 2 command again. Expected: PASS.

- [ ] **Step 5: Add a failing frontend turn-complete test**

In `AcpConnectionsProvider permission request details`, connect an owner, emit `status_changed(prompting)`, emit `permission_request`, assert `pendingPermission` exists, then emit:

```typescript
emitAcpEvent(handlers, {
  seq: 3,
  connection_id: "spawned-conn",
  type: "turn_complete",
  session_id: "sess-1",
  stop_reason: "cancelled",
})
expect(h.store!.getConnection(TAB)!.pendingPermission).toBeNull()
```

- [ ] **Step 6: Run the frontend test and verify RED**

Run:

```powershell
pnpm test src/contexts/acp-connections-context.test.tsx
```

Expected: FAIL because `pendingPermission` remains populated after `turn_complete`.

- [ ] **Step 7: Clear live permission state on turn completion**

In the `turn_complete` event branch, dispatch `{ type: "PERMISSION_CLEARED", contextKey }` immediately after flushing queued updates and before changing status to `connected`. This is idempotent with later `permission_resolved` events.

- [ ] **Step 8: Run focused Rust and frontend tests**

```powershell
cargo test --no-default-features --lib connection::tests::cancelled_permission_ids_emit_resolution_events -- --nocapture
pnpm test src/contexts/acp-connections-context.test.tsx
```

Expected: both commands pass.

- [ ] **Step 9: Commit Task 2**

```powershell
git add -- src-tauri/src/acp/connection.rs src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git commit -m "fix(acp): clear permissions promptly on cancel"
```

### Task 3: Verify the combined change

**Files:**
- Verify only; do not stage unrelated files.

**Interfaces:**
- Consumes: Task 1 and Task 2 commits.
- Produces: fresh test, check, lint, and diff evidence.

- [ ] **Step 1: Run focused regression tests**

```powershell
cargo test --no-default-features --lib terminal_runtime::tests -- --nocapture
cargo test --no-default-features --lib connection::tests::cancelled_permission_ids_emit_resolution_events -- --nocapture
pnpm test src/contexts/acp-connections-context.test.tsx
```

- [ ] **Step 2: Run Rust checks required for the affected shared library**

```powershell
cargo check
cargo check --no-default-features --bin codeg-server
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

- [ ] **Step 3: Run frontend project checks**

```powershell
pnpm eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
pnpm test
pnpm build
```

- [ ] **Step 4: Inspect final scope**

```powershell
git diff --check HEAD~2..HEAD
git show --stat --oneline HEAD~2..HEAD
git status --short
```

Expected: only the design, terminal runtime, ACP connection, and permission reducer/test files belong to this fix; all pre-existing unrelated worktree changes remain untouched.
