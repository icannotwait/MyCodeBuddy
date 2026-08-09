# Task 6 Brief

### Task 6: Project Live State with Snapshot Recovery and Deterministic Interaction Decline

**Milestone:** M4 backend/live path.

**Files:**

- Create: `src-tauri/codeg-eui-core/src/live.rs`
- Expand: `src-tauri/codeg-eui-core/src/model.rs`
- Expand: `src-tauri/codeg-eui-core/src/runtime.rs`
- Expand: `src-tauri/codeg-eui-core/src/perf.rs`
- Test: `src-tauri/codeg-eui-core/tests/live_recovery.rs`
- Test: `src-tauri/codeg-eui-core/tests/interaction_decline.rs`

**Interfaces:**

- Consumes: `ConnectionManager::get_state`, `SessionState::{to_snapshot,event_stream}`, `EventEnvelope.seq`, `AcpEvent`, `respond_permission`, `cancel`, `cancel_question`, and `cancel_plan_approvals_by_parent`.
- Produces: `LiveProjector::attach(connection_id, selection_epoch)`, `snapshot_and_subscribe`, `Projection::replace_from_snapshot`, `Projection::apply_envelope`, `reconcile_snapshot_interactions`, `needs_resync`, generation-counted assistant/transcript output, and `decline_interaction`.
- Recovery invariant: snapshot and subscribe happen under one `SessionState` read lock; after replacement at sequence `S`, pending snapshot interactions are declined exactly once before the receiver resumes, and only envelopes `S+1` onward may mutate the projection.

**Task Routing Matrix:**

| task_index | title | files/modules | hard triggers evidence | soft signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 6 | Live snapshot recovery and deterministic decline | projector, permission policy, frame projection, tests | `concurrency_lifecycle`: attach/lag/overflow/switch; `security_trust_boundary`: permission/question/plan fail closed | `cross_runtime_or_process=2`, `multiple_ownership_modules=1`, `shared_interface=1`; total `4` | `high`: both hard triggers apply | `codex` | `codex (separate) + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Write the atomic attach race RED test**

Drive a real test `SessionState` and stream with barriers, not timing sleeps:

```rust
#[tokio::test]
async fn attach_cannot_miss_event_between_snapshot_and_subscribe() {
    let fixture = LiveFixture::new().await;
    fixture.pause_attach_while_read_locked();
    let attach = tokio::spawn(fixture.projector().attach(fixture.connection_id(), 1));
    fixture.emit_after_attach_attempt(AcpEvent::ContentDelta {
        text: "hello".into(), parent_tool_use_id: None,
    }).await;
    fixture.release_attach();
    let projector = attach.await.unwrap().unwrap();
    assert_eq!(projector.snapshot().live_assistant, "hello");
    assert_eq!(projector.snapshot().event_seq, 1);
}

```

- [ ] **Step 2: Run the attach test to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery attach_cannot_miss -- --nocapture
```

Expected: FAIL because the attach API and lock seam do not exist.

- [ ] **Step 3: Add the projector skeleton and atomic snapshot/subscription helper**

```rust
pub struct AttachPoint {
    pub snapshot: LiveSessionSnapshot,
    pub receiver: tokio::sync::broadcast::Receiver<EventEnvelope>,
}

async fn snapshot_and_subscribe(state: &Arc<RwLock<SessionState>>) -> AttachPoint {
    let guard = state.read().await;
    AttachPoint {
        snapshot: guard.to_snapshot(),
        receiver: guard.event_stream().subscribe(),
    }
}

pub struct LiveProjector {
    connection_id: String,
    selection_epoch: u64,
    cursor: u64,
    projection: Arc<Mutex<Projection>>,
    declined: HashSet<InteractionKey>,
}
```

`attach` obtains `AttachPoint`, releases the read lock, and replaces the projection at `snapshot.event_seq`. Once the interaction policy below is present, it reconciles all pending interactions in that snapshot before consuming the already-subscribed receiver. No await or event emission occurs while holding the state read lock.

- [ ] **Step 4: Run the attach test to verify GREEN**

Run the Step 2 command. Expected: the event attempted during attach is either in the snapshot or receiver, never missed or duplicated.

- [ ] **Step 5: Write gap, lag, overflow, switch, and parity RED tests**

Add the sequence-gap test plus broadcast `Lagged`, a full 128-control-event queue containing permission plus `TurnComplete`, text delta coalescing, session switch during stream, and final JSON parity between `Projection` and `SessionState::to_snapshot()`. The old selection task must terminate without overwriting the new selection, while its accepted request completion still arrives stale.

```rust
#[tokio::test]
async fn sequence_gap_replaces_projection_from_authoritative_snapshot() {
    let mut projector = projector_at_seq(4, "partial");
    projector.apply(envelope(6, delta("wrong"))).await;
    assert!(projector.snapshot().needs_resync);
    projector.resync(authoritative_snapshot(6, "final")).await.unwrap();
    assert_eq!(projector.snapshot().live_assistant, "final");
    assert_eq!(projector.snapshot().event_seq, 6);
    assert!(!projector.snapshot().needs_resync);
}
```

- [ ] **Step 6: Run recovery tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery
```

Expected: FAIL on the first unimplemented gap/lag/overflow recovery path.

- [ ] **Step 7: Implement merge and authoritative resubscription**

Coalesce consecutive text into the live assistant buffer; reduce tools to `{name,status}` summaries; carry errors/turn state explicitly. Any `seq != cursor+1`, `RecvError::Lagged`, or control enqueue failure sets `needs_resync`, discards the old receiver, calls `snapshot_and_subscribe` again, replaces from that authoritative snapshot, reconciles snapshot interactions through the shared decline policy below, then resumes from the new receiver. Producers never await the projector.

- [ ] **Step 8: Run recovery tests to verify GREEN**

Run the Step 6 command. Expected: attach, gap, lag, overflow, coalescing, switch isolation, and final parity pass without sleeps.

- [ ] **Step 9: Write live-event and snapshot-only interaction RED tests**

```rust
#[tokio::test]
async fn permission_uses_reject_option_or_cancels_turn() {
    let manager = RecordingManager::default();
    decline_permission(&manager, "c1", "r1", &[
        option("allow", "Allow once", "allow_once"),
        option("deny", "Deny", "reject_once"),
    ]).await.unwrap();
    assert_eq!(manager.permission_response(), Some(("r1", "deny")));

    decline_permission(&manager, "c1", "r2", &[]).await.unwrap();
    assert_eq!(manager.cancel_count(), 1);
}
```

Add question and plan-approval event cases that resolve the parked receiver, emit the resolved event, surface `Interactive prompts require the main app` in the EUI error strip, and reach `TurnComplete` or hard error within a bounded test timeout.

Add initial-attach and resync fixtures for each snapshot field with no corresponding request event after the snapshot cursor: `pending_permission.request_id`, `pending_question.question_id`, and `pending_plan_approval.approval_id`. For each fixture, assert the appropriate manager method is invoked once before `receive_next` begins, a second resync containing the same ID does not invoke it again, and the turn reaches `t_end` or a surfaced hard error.

```rust
#[tokio::test]
async fn snapshot_pending_interactions_decline_before_event_resume_once() {
    for case in [PendingCase::Permission, PendingCase::Question, PendingCase::Plan] {
        let fixture = InteractionFixture::snapshot_only(case).await;
        let projector = fixture.attach().await.unwrap();
        assert_eq!(fixture.decline_count(case), 1);
        assert!(!projector.has_started_receive_before_decline());
        projector.resync(fixture.same_pending_snapshot()).await.unwrap();
        assert_eq!(fixture.decline_count(case), 1);
        fixture.assert_terminal_within(Duration::from_secs(2)).await;
    }
}
```

- [ ] **Step 10: Run interaction tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test interaction_decline -- --nocapture
```

Expected: live-event policy and snapshot-only reconciliation cases fail before implementation.

- [ ] **Step 11: Implement one deduplicated decline function for events and snapshots**

Choose a permission option whose normalized `kind`, then `name`, then `option_id` contains `reject` or `deny`; call `respond_permission`. If none exists, call `ConnectionManager::cancel`. For a question, call `cancel_question(connection_id, question_id)`. For a plan approval, call `cancel_plan_approvals_by_parent(connection_id)`. Do not mutate persisted question/feedback settings.

Both event and snapshot paths create the same key and call the same function:

```rust
#[derive(Clone, Hash, Eq, PartialEq)]
enum InteractionKey { Permission(String), Question(String), Plan(String) }

async fn decline_once(
    manager: &ConnectionManager,
    connection_id: &str,
    interaction: PendingInteraction,
    seen: &mut HashSet<InteractionKey>,
) -> Result<(), AcpError> {
    let key = interaction.key();
    if !seen.insert(key) { return Ok(()); }
    decline_interaction(manager, connection_id, interaction).await
}
```

Immediately after every `replace_from_snapshot`, call `decline_once` for `pending_permission`, `pending_question`, and `pending_plan_approval` in that order, then begin receiving at `snapshot.event_seq + 1`. If decline fails, cancel the active turn, set a hard error, and do not resume with a parked responder.

- [ ] **Step 12: Run interaction tests to verify GREEN**

Run the Step 10 command. Expected: event and snapshot-only interactions resolve once and terminate without parking.

- [ ] **Step 13: Write native marker/frame RED tests**

Assert `t_first_token_ns` is set once on the first authoritative non-empty assistant buffer, resync does not move it, `t_end_ns` is set by `TurnComplete` or hard error, and `needs_resync` remains visible until replacement commits.

- [ ] **Step 14: Run native marker tests to verify RED**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery marker -- --nocapture
```

Expected: FAIL because frame marker propagation is absent.

- [ ] **Step 15: Wire projector output and native markers into frames**

The projector updates the shared model without touching `last_frame`; poll picks it up on the next dirty generation. Set `t_first_token_ns` exactly once when the authoritative/coalesced live assistant first becomes non-empty after `t0`. Set `t_end_ns` on `TurnComplete` or hard error. `needs_resync` remains true in frames until the replacement snapshot is committed.

- [ ] **Step 16: Run marker tests to verify GREEN**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery marker -- --nocapture
```

- [ ] **Step 17: Run M4 live-path verification**

```bash
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test live_recovery
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test interaction_decline
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test session_contract
```

Expected: race-free attach, lag/overflow resync, non-blocking producers, session-switch isolation, final snapshot parity, snapshot-before-resume decline, exactly-once completions, and terminal decline all pass.

- [ ] **Step 18: Commit and prepare the Task 6 review package**

```bash
git add --dry-run -- src-tauri/codeg-eui-core/src/live.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/perf.rs src-tauri/codeg-eui-core/tests/live_recovery.rs src-tauri/codeg-eui-core/tests/interaction_decline.rs
git add -- src-tauri/codeg-eui-core/src/live.rs src-tauri/codeg-eui-core/src/model.rs src-tauri/codeg-eui-core/src/runtime.rs src-tauri/codeg-eui-core/src/perf.rs src-tauri/codeg-eui-core/tests/live_recovery.rs src-tauri/codeg-eui-core/tests/interaction_decline.rs
git diff --cached --name-only
git status --short --untracked-files=all
git commit -m "feat(eui): add recoverable live stream projection"
git show --stat --oneline HEAD
git diff HEAD^ -- src-tauri/codeg-eui-core
```

Expected package: one live-path commit with recovery and fail-closed interaction evidence. Route it to both high-risk reviewers, then continue directly to Task 7.
