# Delegation Runtime Flush Deadlock Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent the coalesced delegation runtime flush from retaining `pending.inner` and blocking completion or cancellation forever.

**Architecture:** Keep the existing two-phase state read around the attention lookup. Move the first read into an explicit lexical scope so its Tokio `MutexGuard` is dropped before any await, then reacquire the lock after attention lookup to confirm the task is still running before publishing metadata.

**Tech Stack:** Rust 2021, Tokio async synchronization and paused-time tests, existing delegation broker mock stores and runtime publication gate.

## Global Constraints

- Modify only `src-tauri/src/acp/delegation/broker.rs` production behavior and its colocated unit tests.
- Preserve the post-attention `running` lookup so terminal tasks cannot receive stale running metadata.
- Preserve all existing debug logging and unrelated dirty-worktree changes.
- Add no dependencies, schema changes, wire-contract changes, retries, or timeouts to production code.

---

## File Structure

- `src-tauri/src/acp/delegation/broker.rs`: owns the coalesced runtime flush, pending-state lock protocol, and broker unit tests. Add one regression test and narrow two lock scopes here.

### Task 1: Release Pending State Between Runtime Publication Reads

**Files:**
- Modify and test: `src-tauri/src/acp/delegation/broker.rs:4160`
- Add test near: `src-tauri/src/acp/delegation/broker.rs:6827`

**Interfaces:**
- Consumes: `LiveRuntimeState::install_publication_gate`, `DelegationBroker::project_child_tool_event`, `DelegationBroker::complete_call`, and existing `coordination_broker`, `spawn_running`, `within`, `tool_call`, and `completed_outcome` test helpers.
- Produces: unchanged `DelegationBroker` API; the runtime flush no longer retains `pending.inner` across `latest_open_attention(...).await`.

- [ ] **Step 1: Write the failing regression test**

Add this test beside the existing coalesced runtime tests:

```rust
#[tokio::test(start_paused = true)]
async fn coalesced_runtime_flush_releases_pending_lock_before_meta_refresh() {
    let (broker, spawner, store, _attention) = coordination_broker().await;
    let task_id = spawn_running(&broker, &spawner, "parent", 11, "child-conn").await;
    let runtime = broker
        .coordination_for_test("child-conn")
        .await
        .expect("coordination identity")
        .runtime;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    runtime
        .install_publication_gate(entered_tx, release_rx)
        .await;

    broker
        .project_child_tool_event("child-conn", &tool_call("tc-1", "read", "Read"))
        .await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(RUNTIME_STATS_FLUSH_INTERVAL).await;
    entered_rx
        .await
        .expect("coalesced flush should reach runtime publication");
    assert_eq!(store.runtime_write_count(&task_id).await, 1);

    release_tx.send(()).unwrap();
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    let pending = within(broker.pending.inner.lock()).await;
    drop(pending);

    within(broker.complete_call(&task_id, completed_outcome("done"))).await;
    let persisted = store.load(&task_id).await.unwrap().unwrap();
    assert_eq!(persisted.status, TaskStatus::Completed);
    let stats = store.latest_runtime(&task_id).await.unwrap();
    assert_eq!(stats.tool_call_count, 1);
    assert!(stats.finished_at.is_some());
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run from `src-tauri`:

```powershell
cargo test --features test-utils coalesced_runtime_flush_releases_pending_lock_before_meta_refresh -- --nocapture
```

Expected: FAIL after the `within` one-second timeout with `operation should finish within one second`, proving the successful flush retained `pending.inner`.

- [ ] **Step 3: Implement the minimal guard-lifetime fix**

Replace the temporary guard in the `if let` scrutinee and make the second read's scope explicit:

```rust
let identity = {
    let inner = broker.pending.inner.lock().await;
    inner
        .coordination_by_child
        .values()
        .find(|identity| identity.task_id == task_id)
        .cloned()
};
if let Some(identity) = identity {
    let attention = broker
        .latest_open_attention(identity.parent_conversation_id, &task_id)
        .await;
    let child_conversation_id = {
        let inner = broker.pending.inner.lock().await;
        inner
            .running
            .get(&task_id)
            .map(|task| task.child_conversation_id)
    };
    if let Some(child_conversation_id) = child_conversation_id {
        broker
            .write_meta_if_real(
                &identity.parent_connection_id,
                &identity.parent_tool_use_id,
                build_delegation_meta(&DelegationMetaSnapshot {
                    status: "running".into(),
                    task_id: task_id.clone(),
                    child_connection_id: Some(identity.child_connection_id.clone()),
                    child_conversation_id,
                    error_code: None,
                    text_preview: None,
                    started_at: written_stats.started_at,
                    finished_at: None,
                    runtime_stats: Some(written_stats),
                    attention_request: attention,
                }),
            )
            .await;
    }
}
```

- [ ] **Step 4: Run the focused test and verify GREEN**

Run:

```powershell
cargo test --features test-utils coalesced_runtime_flush_releases_pending_lock_before_meta_refresh -- --nocapture
```

Expected: PASS; the pending lock is acquired, completion returns, the durable task is `Completed`, and final runtime stats retain the tool call.

- [ ] **Step 5: Run related broker regressions**

Run:

```powershell
cargo test --features test-utils runtime_ -- --nocapture
```

Expected: all runtime projection, persistence, race, and settlement tests selected by the filter pass.

- [ ] **Step 6: Run required Rust verification**

Run from `src-tauri`:

```powershell
cargo check
cargo check --no-default-features --bin codeg-server
cargo test --features test-utils
```

Expected: every command exits with status 0 and no new compiler diagnostics attributable to this change.

- [ ] **Step 7: Review the final diff without committing overlapping user changes**

Run from the repository root:

```powershell
git diff --check -- src-tauri/src/acp/delegation/broker.rs
git diff -- src-tauri/src/acp/delegation/broker.rs
```

Expected: no whitespace errors. The diff contains the pre-existing user-owned `complete_call` logging plus only the new regression test and two explicit lock scopes from this task. Do not create an implementation commit because the same file already contains user-owned uncommitted changes that must remain independently attributable.
