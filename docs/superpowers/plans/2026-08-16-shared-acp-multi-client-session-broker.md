# Shared ACP Multi-Client Session Broker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one process-local, server-owned ACP root-session broker so many authenticated devices and tabs can atomically attach, queue prompts, answer interactions, and stop the exact active turn without duplicating or accidentally terminating the underlying agent.

**Architecture:** `ConnectionManager` owns a shallow-cloned `SharedSessionBroker` whose global lock only indexes per-session records; each record atomically owns generation, leases, prompt FIFO, active turn, bootstrap phase, idempotency, and idle eligibility. The existing `AgentConnection`, `SessionState`, prompt-linking path, event ring, and WebSocket snapshot/replay remain the execution and delivery mechanisms; broker mutations commit first, then publish additive ACP events into `SessionState`. Only server-hosted user-root sessions use the broker; Tauri ownership, pop-outs, delegated children, automation, probes, and hidden generation retain their existing paths.

**Tech Stack:** Rust 2021, Tokio synchronization/time, SeaORM/SQLite, Axum HTTP/WebSocket, serde, sha2, Next.js 16 static export, React 19, TypeScript strict, Vitest, and pnpm.

## Global Constraints

- Scope is one `codeg-server` process with many authenticated devices/tabs; broker state, leases, queues, bootstrap state, and active turns are process-local and are not restored after restart.
- A persisted root is keyed by `Conversation(conversation_id)` and has at most one non-terminal `Reserved | Bootstrapping | Ready | Closing` incarnation; `agent_type` is launch identity, not an alternate key.
- `connect_or_attach` installs the broker record and a `Connecting` `SessionState` before returning; it returns a stable `connection_id`, generation, and lease while bootstrap continues asynchronously.
- Client heartbeat cadence is 30 seconds, lease TTL defaults to 90 seconds (`CODEG_ACP_CLIENT_LEASE_TTL_SECS`), and shared-root idle grace defaults to 900 seconds (`CODEG_ACP_IDLE_TIMEOUT_SECS`). A zero idle timeout continues to disable idle reclamation.
- The per-session waiting FIFO admits at most 64 items and at most 32 MiB of serialized waiting payload. Capacity rejection never drops an accepted item.
- `device_id`, `client_instance_id`, `request_id`, and `client_request_id` are ASCII labels matching `[A-Za-z0-9._:-]{1,128}`. Each generation admits at most 256 active leases, 4,096 distinct connect-request ledger entries, and 65,536 distinct prompt-idempotency ledger entries; an existing idempotency key remains retryable at capacity, while a new key receives a stable typed capacity error.
- Each record retains at most 1,024 recently expired lease-id tombstones so a recent expired credential returns `client_lease_expired`; after bounded FIFO eviction, an older unknown credential returns `client_lease_missing`. The broker index retains at most 4,096 replaced connection-id/generation tombstones so an old subscription receives `session_replaced`; all diagnostics/logs expose only tombstone counts, never ids.
- Prompt retry identity is exactly `(generation, client_instance_id, client_request_id)`; identical retries return the original result and a changed payload returns `idempotency_key_conflict`.
- Every shared mutation validates exact `connection_id + generation + lease_id`; turn stop additionally validates exact `turn_id`, and an expired lease must reconnect instead of being revived by mutation.
- Permission, question, and plan-approval responses are one-shot compare-and-set transitions; the first valid response reaches the existing responder and later responses return `interaction_already_resolved`.
- Releasing or expiring a lease never disconnects ACP and never removes accepted queue items. Stopping an active turn preserves the queued tail and dispatch resumes only after the existing cancellation finalizer/quarantine emits terminal completion.
- Explicit process termination is a separate authenticated, generation-fenced command used only by the existing explicit-user disconnect affordance; provider/tab/socket teardown remains release-only.
- Idle eligibility is the intersection of `Ready`, underlying `Connected`, zero leases, empty queue, no active turn/quarantine, no permission/question/plan approval, no continuation wait, no active delegation, `background_outstanding == 0`, and no registered host work. Any blocker resets `idle_zero_since`.
- Explicitly required Codeg companion routes fail with typed `companion_initialization_failed`, complete bounded cleanup, and never call native fallback. Auto/native route policies retain fallback only when their resolved route policy permits it.
- Broker-managed roots reject legacy browser prompt/interaction mutations without shared fencing as `shared_session_protocol_required`; legacy browser disconnect/touch cannot terminate or keep alive the broker root.
- Lock order is broker map -> per-session record -> manager connection map -> `SessionState`; clone and release before awaits, never perform DB/process/channel/WebSocket awaits while holding broker locks, and never acquire broker locks while holding `SessionState`.
- Logs, metrics, errors, and diagnostics may contain connection/conversation ids, generation, agent type, stable code, counts, and durations; they must not contain lease ids, client ids, bearer tokens, prompt/answer text, paths, environment, stderr, or raw agent output.
- All eleven built-in agents plus conforming custom ACP agents use the same server-root broker path; local Tauri roots, pop-out handoff, delegated children, automation, probes, title/translation, and hidden generation remain outside shared ownership.
- Next.js remains static-export only, TypeScript remains strict, Rust uses `thiserror`, and existing desktop/delegation behavior must remain covered by regression tests.
- On macOS, do not execute default-feature `cargo test --lib --features test-utils` or `cargo test --features test-utils`: those binaries enable `tauri-runtime` and can access the real `codeg` login-keychain entry under a new ad-hoc-signed test hash. Run shared/core tests with `--no-default-features --features server,test-utils`; desktop mode receives compile/clippy coverage until the separate keyring test-isolation defect has a mock or temporary backend.

## File Structure

- Create `src-tauri/src/acp/shared_session.rs`: broker keys, launch identity, phases, leases, prompt records/projections, idempotency, interaction claims, idle predicate, counters, diagnostics, and unit tests.
- Modify `src-tauri/src/acp/manager.rs`: own/clone the broker, register asynchronous shared roots, run broker dispatcher/monitor tasks, fence mutations, and protect legacy paths.
- Modify `src-tauri/src/acp/connection.rs`: expose the already-registered spawn attempt and apply the resolved route fallback policy without changing non-shared callers.
- Modify `src-tauri/src/acp/types.rs` and `src-tauri/src/acp/session_state.rs`: additive queue/bootstrap/turn events plus the snapshot-recoverable shared projection.
- Modify `src-tauri/src/acp/error.rs`, `src-tauri/src/app_error.rs`, and `src-tauri/src/web/handlers/error.rs`: stable secret-safe error taxonomy and HTTP status mapping.
- Modify `src-tauri/src/acp/idle_sweep.rs`: 900-second shared idle default, 90-second lease configuration, and broker sweep integration.
- Modify `src-tauri/src/web/handlers/acp.rs`, `src-tauri/src/web/router.rs`, `src-tauri/src/web/ws_attach.rs`, and `src-tauri/src/web/ws.rs`: connect/lease/queue routes and lease-bound WebSocket subscriptions.
- Modify `src-tauri/tests/ws_attach.rs` and create `src-tauri/tests/shared_session_http.rs`: authenticated HTTP/WebSocket concurrency, lease, replay, queue, and compatibility coverage.
- Create `src/lib/acp/shared-session-client.ts`: stable device/document identity and request-id helpers.
- Modify `src/lib/types.ts`, `src/lib/api.ts`, `src/lib/tauri.ts`, `src/lib/transport/types.ts`, and `src/lib/transport/web-event-stream.ts`: TypeScript contracts and lease-bound attach frames.
- Modify `src/lib/snapshot-denormalize.ts`, `src/contexts/acp-connections-context.tsx`, and `src/hooks/use-connection.ts`: shared connection state, snapshot/event convergence, fenced actions, and release-only teardown.
- Create `src/components/chat/shared-message-queue-display.tsx` and modify `src/components/conversations/conversation-session-surface.tsx` plus `src/components/chat/chat-input.tsx`: authoritative non-editable FIFO display, cancel action, and exact dispatch reconciliation while retaining the existing local desktop draft queue.

---

### Task 1: Broker State, Stable Errors, and Secret-Safe Metrics

**Files:**
- Create: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/acp/error.rs`
- Modify: `src-tauri/src/app_error.rs`
- Modify: `src-tauri/src/web/handlers/error.rs`
- Modify: `src/lib/types.ts`

**Interfaces:**
- Consumes: `AgentType`, `DelegationRoutePlan.fingerprint`, `ResolvedShellSnapshot.selection_key`, `PromptInputBlock`, `PromptCaptureContext`, and `ConnectionStatus` from existing ACP modules.
- Produces: `SharedSessionBroker`, `SharedSessionKey`, `SharedLaunchIdentity`, `SharedConfigConflictKind`, `SharedSessionPhase`, `SharedMutationGuard`, `SharedReserveRequest`, `SharedReserveOutcome`, `SharedSessionProjection`, `SharedSessionError`, `SharedSessionMetrics`, `SharedSessionMetricsSnapshot`, `MAX_WAITING_PROMPTS = 64`, `MAX_WAITING_BYTES = 32 * 1024 * 1024`, `MAX_ACTIVE_LEASES = 256`, `MAX_CONNECT_LEDGER_ENTRIES = 4_096`, `MAX_PROMPT_LEDGER_ENTRIES = 65_536`, `MAX_EXPIRED_LEASE_TOMBSTONES = 1_024`, and `MAX_REPLACED_CONNECTION_TOMBSTONES = 4_096`.

- [ ] **Step 1: Write failing unit tests for atomic reservation, configuration conflict, generation retry, lease identity, and diagnostics redaction**

Add `#[cfg(test)] mod tests` to the new module with these exact cases and helpers:

```rust
fn request(key: SharedSessionKey, connection_id: &str, client: &str, request_id: &str) -> SharedReserveRequest {
    SharedReserveRequest {
        key,
        connection_id: connection_id.into(),
        launch_identity: SharedLaunchIdentity::fixture(),
        client_instance_id: client.into(),
        device_id: "device-a".into(),
        request_id: request_id.into(),
        retry_failed_generation: None,
        now: tokio::time::Instant::now(),
        now_utc: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn one_hundred_reservations_share_one_incarnation() {
    let broker = SharedSessionBroker::default();
    let mut joins = Vec::new();
    for n in 0..100 {
        let broker = broker.clone();
        joins.push(tokio::spawn(async move {
            broker.reserve_or_attach(request(
                SharedSessionKey::Conversation(42),
                &format!("candidate-{n}"),
                &format!("client-{n}"),
                &format!("request-{n}"),
            )).await.unwrap()
        }));
    }
    let outcomes = futures::future::join_all(joins).await;
    let ids: std::collections::HashSet<_> = outcomes.into_iter()
        .map(|result| result.unwrap().attachment.connection_id)
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(broker.metrics().snapshot().created_total, 1);
    assert_eq!(broker.metrics().snapshot().attached_total, 99);
}

#[tokio::test]
async fn immutable_launch_conflict_does_not_mutate_live_record() {
    let broker = SharedSessionBroker::default();
    let first = broker.reserve_or_attach(request(
        SharedSessionKey::Conversation(7), "conn-a", "client-a", "req-a"
    )).await.unwrap();
    let mut conflicting = request(
        SharedSessionKey::Conversation(7), "conn-b", "client-b", "req-b"
    );
    conflicting.launch_identity.working_dir_fingerprint = "different".into();
    assert!(matches!(
        broker.reserve_or_attach(conflicting).await,
        Err(SharedSessionError::ConfigConflict { conflict_kind: SharedConfigConflictKind::WorkingDirectory, .. })
    ));
    assert_eq!(broker.diagnostic_for_connection(&first.attachment.connection_id).await.unwrap().generation, 1);
}

#[tokio::test]
async fn failed_retry_requires_cleanup_and_increments_generation() {
    let broker = SharedSessionBroker::default();
    let first = broker.reserve_or_attach(request(
        SharedSessionKey::Conversation(8), "conn-a", "client-a", "req-a"
    )).await.unwrap();
    broker.mark_failed(&first.attachment.connection_id, 1, "companion_initialization_failed", false).await.unwrap();
    let mut retry = request(SharedSessionKey::Conversation(8), "conn-b", "client-a", "req-b");
    retry.retry_failed_generation = Some(1);
    assert!(matches!(broker.reserve_or_attach(retry.clone()).await, Err(SharedSessionError::CleanupInProgress)));
    broker.mark_cleanup_complete(&first.attachment.connection_id, 1).await.unwrap();
    let replacement = broker.reserve_or_attach(retry).await.unwrap();
    assert_eq!(replacement.attachment.generation, 2);
    assert_eq!(replacement.attachment.connection_id, "conn-b");
}

#[tokio::test]
async fn concurrent_failed_retries_create_one_next_generation() {
    let broker = failed_cleanup_complete_fixture(18).await;
    let outcomes = futures::future::join_all((0..10).map(|n| {
        let broker = broker.clone();
        async move {
            let mut retry = request(
                SharedSessionKey::Conversation(18),
                &format!("retry-{n}"),
                &format!("client-{n}"),
                &format!("request-{n}"),
            );
            retry.retry_failed_generation = Some(1);
            broker.reserve_or_attach(retry).await.unwrap()
        }
    })).await;
    let ids: std::collections::HashSet<_> = outcomes.iter()
        .map(|outcome| outcome.attachment.connection_id.as_str())
        .collect();
    assert_eq!(ids.len(), 1);
    assert!(outcomes.iter().all(|outcome| outcome.attachment.generation == 2));
}

#[tokio::test]
async fn diagnostics_never_expose_lease_or_client_identity() {
    let broker = SharedSessionBroker::default();
    let result = broker.reserve_or_attach(request(
        SharedSessionKey::Conversation(9), "conn", "private-client", "req"
    )).await.unwrap();
    let value = serde_json::to_value(
        broker.diagnostic_for_connection(&result.attachment.connection_id).await.unwrap()
    ).unwrap();
    let encoded = value.to_string();
    assert!(!encoded.contains(&result.attachment.lease_id));
    assert!(!encoded.contains("private-client"));
}

#[test]
fn client_labels_have_exact_ascii_bounds() {
    let max = "a".repeat(128);
    let too_long = "a".repeat(129);
    assert!(validate_client_label("client_instance_id", "aZ09._:-").is_ok());
    assert!(validate_client_label("client_instance_id", &max).is_ok());
    for invalid in ["", too_long.as_str(), "contains space", "非ascii"] {
        assert!(matches!(
            validate_client_label("client_instance_id", invalid),
            Err(SharedSessionError::InvalidField { field: "client_instance_id" })
        ));
    }
}

#[tokio::test]
async fn capacity_limits_reject_only_new_identities() {
    let fixture = broker_at_identity_limits().await;
    assert!(fixture.retry_existing_connect().await.is_ok());
    assert!(matches!(fixture.connect_new_identity().await, Err(SharedSessionError::ConnectLedgerCapacityExceeded)));
    assert!(matches!(fixture.attach_new_client().await, Err(SharedSessionError::ClientLeaseCapacityExceeded)));
}
```

The test module owns concrete helpers rather than relying on later tasks:

- `SharedLaunchIdentity::fixture()` is `#[cfg(test)]`, uses `AgentType::Codex`, `SessionAttachMode::Default`, `ConnectionPurpose::User`, and fixed non-secret fingerprints `cwd-fixture`, `route-fixture`, and `shell-fixture`.
- `broker_at_identity_limits()` reserves one record, fills its connect ledger to `MAX_CONNECT_LEDGER_ENTRIES` through valid distinct requests, and fills active leases to `MAX_ACTIVE_LEASES` through valid distinct client ids. It stores the first request as the retry case and exposes only `retry_existing_connect`, `connect_new_identity`, and `attach_new_client`; prompt-ledger capacity belongs to Task 5 and is not referenced here.
- `failed_cleanup_complete_fixture(id)` reserves the named conversation, marks generation 1 Failed, then marks cleanup complete before returning the broker.
- `SharedSessionBroker::with_limits_for_test` may lower limits for fast unit tests, but the test above must also assert the production constants exactly so a test-only limit cannot hide a changed public bound.

- [ ] **Step 2: Run the new tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests -- --nocapture`

Expected: FAIL because `acp::shared_session` and every listed type are undefined.

- [ ] **Step 3: Implement the broker index, record schema, reservation CAS, lease records, diagnostics, and counters**

Create the following public types and constants. Use `tokio::sync::Mutex`; the map stores `Arc<Mutex<SharedSessionRecord>>`, and all methods drop the map lock before awaiting a record lock.

```rust
pub const MAX_WAITING_PROMPTS: usize = 64;
pub const MAX_WAITING_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ACTIVE_LEASES: usize = 256;
pub const MAX_CONNECT_LEDGER_ENTRIES: usize = 4_096;
pub const MAX_PROMPT_LEDGER_ENTRIES: usize = 65_536;
pub const MAX_EXPIRED_LEASE_TOMBSTONES: usize = 1_024;
pub const MAX_REPLACED_CONNECTION_TOMBSTONES: usize = 4_096;
pub const MAX_CLIENT_LABEL_LEN: usize = 128;
pub const MAX_QUEUE_VISIBLE_TEXT_CHARS: usize = 512;
pub const DEFAULT_CLIENT_LEASE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum SharedSessionKey {
    Conversation(i32),
    ExternalSession { agent_type: AgentType, normalized_working_dir: String, external_session_id: String },
    Ephemeral(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedConfigConflictKind {
    AgentType,
    WorkingDirectory,
    ExternalSession,
    AttachMode,
    DelegationRoute,
    TerminalShell,
    Purpose,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SharedLaunchIdentity {
    pub agent_type: AgentType,
    pub working_dir_fingerprint: String,
    pub external_session_id: Option<String>,
    pub attach_mode: crate::acp::session_attach::SessionAttachMode,
    pub route_fingerprint: String,
    pub terminal_shell_fingerprint: String,
    pub purpose: crate::auto_title::ConnectionPurpose,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum SharedSessionPhase {
    Reserved,
    Bootstrapping,
    Ready,
    Failed { error_code: String, cleanup_complete: bool },
    Closing,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedMutationGuard {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedDisposition {
    Created,
    Attached,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedQueuedPromptState {
    Queued,
    Dispatching,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedQueuedPromptSummary {
    pub queue_item_id: String,
    pub enqueue_seq: u64,
    pub client_message_id: String,
    pub visible_text: Option<String>,
    pub visible_text_truncated: bool,
    pub attachment_count: u32,
    pub submitted_at: chrono::DateTime<chrono::Utc>,
    pub state: SharedQueuedPromptState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedActiveTurnProjection {
    pub turn_id: String,
    pub queue_item_id: String,
    pub enqueue_seq: u64,
    pub client_message_id: String,
    pub stop_requested: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedTurnOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone)]
pub struct SharedSessionAttachment {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
    pub lease_expires_at: chrono::DateTime<chrono::Utc>,
    pub disposition: SharedDisposition,
    pub phase: SharedSessionPhase,
}

#[derive(Clone)]
pub struct SharedReserveOutcome {
    pub attachment: SharedSessionAttachment,
    pub created: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedSessionProjection {
    pub generation: u64,
    pub phase: SharedSessionPhase,
    pub queue: Vec<SharedQueuedPromptSummary>,
    pub active_turn: Option<SharedActiveTurnProjection>,
    pub lease_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

`SharedReserveRequest` contains the key, preallocated connection id, immutable launch identity, the three validated connect labels, retry generation, and monotonic/wall-clock times shown in Step 1; it has no derived `Debug`. Queue summaries concatenate only text blocks, truncate on a Unicode scalar boundary to 512 characters, set `visible_text_truncated`, and expose only a count for image/resource/resource-link blocks. They never expose base64, resource text/blob, URI/path, MIME detail, or capture context. Because existing `AcpEvent` and `LiveSessionSnapshot` implement `Debug`, add manual allowlisted `Debug` implementations: summary formatting includes ids, sequence, state, attachment count, and text-present/truncated booleans but not `visible_text`; projection formatting includes generation, phase, queue count, active-turn ids, and lease expiry but formats each queue entry through that redacted implementation. Mutation guard/attachment formatting, if required by callers, writes `lease_id: "***"`.

Define the initial record schema now so later tasks extend one synchronization domain instead of inventing parallel state. Task 1's concrete fields are generation, public connection id, immutable launch identity, phase, cleanup flag, active leases, connect ledger, expired-lease FIFO, and the timestamps/counters needed by its tests. Tasks 2, 5, 6, and 7 add registration handles, queue/turn/interaction state, and idle/host-work state to this same struct when their types are introduced. The connect ledger value stores the complete last `SharedSessionAttachment`, not only a lease id: an identical retry returns the original `Created` or `Attached` disposition and expiry. If that lease expired or was released, the same request key is updated with a new lease and `Attached`; it never reports `Created` again within the generation.

Define `SharedSessionMetrics` as atomic counters/gauges and define this complete snapshot now, initialized to zero:

```rust
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SharedSessionMetricsSnapshot {
    pub created_total: u64,
    pub attached_total: u64,
    pub live_sessions: u64,
    pub active_leases: u64,
    pub bootstrap_ready_total: u64,
    pub bootstrap_failed_total: BTreeMap<String, u64>,
    pub bootstrap_duration_ms_total: u64,
    pub bootstrap_duration_samples: u64,
    pub waiting_prompts: u64,
    pub waiting_bytes: u64,
    pub enqueue_total: u64,
    pub cancel_total: u64,
    pub dispatch_total: u64,
    pub capacity_rejected_total: u64,
    pub queue_item_failed_total: u64,
    pub interaction_winner_total: u64,
    pub interaction_stale_total: u64,
    pub stale_stop_total: u64,
    pub lease_expired_total: u64,
    pub lease_released_total: u64,
    pub idle_candidate_total: u64,
    pub idle_cas_lost_total: u64,
    pub idle_reclaimed_total: u64,
    pub cleanup_duration_ms_total: u64,
    pub cleanup_duration_samples: u64,
    pub cleanup_incomplete_total: u64,
}
```

Task 1 wires created/attached/live-session/active-lease/capacity counters; later tasks populate the remaining fields. This keeps `metrics().snapshot()` source-compatible through every task and prevents Task 12 from replacing a partially defined type. `bootstrap_failed_total` is the only mutex-protected map; `snapshot()` clones it under that short lock and reads every scalar atomically.

Implement `validate_client_label` as byte-wise ASCII validation: length `1..=128`, and every byte must be alphanumeric or one of `._:-`. Do not normalize, truncate, or accept Unicode lookalikes.

Implement `reserve_or_attach` as a true two-phase pointer-CAS loop. Never call retry/replacement logic while holding a record lock:

```rust
pub async fn reserve_or_attach(
    &self,
    request: SharedReserveRequest,
) -> Result<SharedReserveOutcome, SharedSessionError> {
    validate_client_label("device_id", &request.device_id)?;
    validate_client_label("client_instance_id", &request.client_instance_id)?;
    validate_client_label("request_id", &request.request_id)?;
    loop {
        let lookup = {
            let mut index = self.index.lock().await;
            if let Some(record) = index.sessions.get(&request.key) {
                ReserveLookup::Existing(record.clone())
            } else {
                // Build the complete initial record and its first lease before
                // publishing the Arc. There is no await/cancellation point from
                // index insertion through this method's Created return.
                let mut initial = SharedSessionRecord::reserved(&request, 1);
                let attachment = initial.attach_or_renew_lease(
                    &request,
                    self.lease_ttl,
                    SharedDisposition::Created,
                )?;
                let record = Arc::new(Mutex::new(initial));
                index.by_connection.insert(request.connection_id.clone(), request.key.clone());
                index.sessions.insert(request.key.clone(), record);
                ReserveLookup::Created(attachment)
            }
        };
        let record = match lookup {
            ReserveLookup::Created(attachment) => {
                self.metrics.record_connect(true);
                return Ok(SharedReserveOutcome { attachment, created: true });
            }
            ReserveLookup::Existing(record) => record,
        };
        let decision = {
            let mut current = record.lock().await;
            current.check_attach_identity(&request.launch_identity)?;
            match current.retry_decision(&request)? {
                FailedRetryDecision::Attach => {
                    let attachment = current.attach_or_renew_lease(
                        &request,
                        self.lease_ttl,
                        SharedDisposition::Attached,
                    )?;
                    ReserveDecision::Attach(attachment)
                }
                FailedRetryDecision::Replace { failed_generation } => {
                    ReserveDecision::Replace { failed_generation }
                }
            }
        };

        match decision {
            ReserveDecision::Attach(attachment) => {
                self.metrics.record_connect(false);
                return Ok(SharedReserveOutcome { attachment, created: false });
            }
            ReserveDecision::Replace { failed_generation } => {
                // This method locks the index, pointer-compares `record`, then
                // uses `record.try_lock()` under the documented map -> record
                // order to revalidate Failed + generation + cleanup_complete.
                // On contention it drops the map and retries; it never awaits a
                // record lock while holding the map. The winning critical
                // section constructs and installs the complete replacement.
                if let Some(outcome) = self.replace_failed_generation(
                    &request,
                    &record,
                    failed_generation,
                ).await? {
                    self.metrics.record_connect(true);
                    return Ok(outcome);
                }
                // A competing retry changed the pointer; resolve against the
                // authoritative index on the next loop iteration.
            }
        }
    }
}
```

Initial insertion constructs the complete record and first lease before publishing its `Arc`. `replace_failed_generation` locks the index, verifies `Arc::ptr_eq`, and calls `try_lock` on that exact record while the map guard is held. If the record is busy it drops the map guard, yields, and retries without changing state. With both guards and no await/cancellation point, it rechecks `Failed { cleanup_complete: true }` plus the exact generation, constructs a complete generation `old + 1` record with the retry caller's first lease, and replaces key and connection indexes atomically. Task 2 adds lifecycle notification for obsolete owned tasks; Task 4 adds the bounded replacement tombstone in this same critical section. A pointer-CAS loser returns `None`, loops, and attaches to the winner. This closes the terminate-versus-retry race without ever acquiring the index while already holding a record lock. Keep the mutable canonical key only in `SharedSessionIndex.sessions` plus `by_connection`; do not duplicate it in `SharedSessionRecord`, so Task 5 rekey is one map-lock atomic move rather than a cross-lock update. Do not derive `Debug` for mutation guards, keys, launch identity, projections, prompt records, answer records, or any aggregate that can transitively contain lease ids, paths, prompt blocks, visible text, or answer content; if debugging is needed, implement an allowlisted redacted formatter.

Metrics change in the same winning critical section: `created_total` increments once, `live_sessions` remains unchanged for pointer replacement, and `active_leases` subtracts every old-generation lease before adding the retry winner's first lease. Attach/retry CAS losers increment only `attached_total` when they successfully join the winner.

When a CAS loser loops with `retry_failed_generation = Some(old)` and finds the winner's live generation `old + 1`, treat it as a normal attach to that winner. A retry generation greater than the indexed generation or one that does not name the immediately replaced failed generation returns `GenerationStale`; it never creates another generation.

- [ ] **Step 4: Add a dedicated `SharedSessionError` taxonomy and map it through `AcpError`, `AppErrorCode`, HTTP, and TypeScript**

Implement exact stable codes via `SharedSessionError::code()`:

```rust
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SharedSessionError {
    #[error("shared session configuration conflicts with connection {connection_id}")]
    ConfigConflict { connection_id: String, conflict_kind: SharedConfigConflictKind },
    #[error("shared session fencing is required")]
    ProtocolRequired,
    #[error("shared session generation is stale")]
    GenerationStale,
    #[error("shared session is closing")]
    Closing,
    #[error("shared session cleanup is in progress")]
    CleanupInProgress,
    #[error("client lease is missing")]
    LeaseMissing,
    #[error("client lease has expired")]
    LeaseExpired,
    #[error("client lease capacity is exhausted")]
    ClientLeaseCapacityExceeded,
    #[error("connect idempotency capacity is exhausted")]
    ConnectLedgerCapacityExceeded,
    #[error("prompt idempotency capacity is exhausted")]
    PromptLedgerCapacityExceeded,
    #[error("prompt queue is full")]
    PromptQueueFull,
    #[error("idempotency key was reused with different content")]
    IdempotencyKeyConflict,
    #[error("queued prompt was not found")]
    QueueItemNotFound,
    #[error("queued prompt is already dispatching")]
    QueueItemAlreadyDispatching,
    #[error("interaction was already resolved")]
    InteractionAlreadyResolved,
    #[error("turn id is stale")]
    StaleTurn,
    #[error("shared session is unavailable")]
    SessionUnavailable,
    #[error("required Codeg companion initialization failed")]
    CompanionInitializationFailed,
    #[error("conversation is already bound to another shared session")]
    ConversationKeyConflict,
    #[error("invalid shared-session field: {field}")]
    InvalidField { field: &'static str },
}
```

`SharedSessionError::code()` must return these exact strings, with no message parsing:

```text
ConfigConflict -> shared_session_config_conflict
ProtocolRequired -> shared_session_protocol_required
GenerationStale -> shared_session_generation_stale
Closing -> shared_session_closing
CleanupInProgress -> shared_session_cleanup_in_progress
LeaseMissing -> client_lease_missing
LeaseExpired -> client_lease_expired
ClientLeaseCapacityExceeded -> client_lease_capacity_exceeded
ConnectLedgerCapacityExceeded -> connect_idempotency_capacity_exceeded
PromptLedgerCapacityExceeded -> prompt_idempotency_capacity_exceeded
PromptQueueFull -> prompt_queue_full
IdempotencyKeyConflict -> idempotency_key_conflict
QueueItemNotFound -> queue_item_not_found
QueueItemAlreadyDispatching -> queue_item_already_dispatching
InteractionAlreadyResolved -> interaction_already_resolved
StaleTurn -> stale_turn
SessionUnavailable -> session_unavailable
CompanionInitializationFailed -> companion_initialization_failed
ConversationKeyConflict -> shared_session_conversation_key_conflict
InvalidField -> invalid_shared_session_field
```

Add `AcpError::Shared(#[from] SharedSessionError)`, return its stable code from `AcpError::code`, and map it to a structured `AppCommandError`. Add matching explicit `AppErrorCode` variants and exact snake-case codes. Map invalid field to HTTP 400, missing lease to HTTP 409, expired lease to HTTP 410, and queue/lease/connect-ledger/prompt-ledger capacity errors to HTTP 429. Map every other shared concurrency/fencing code to HTTP 409. Bearer-token middleware remains the only source of HTTP 401. Extend the TypeScript `AppErrorCode` union with the same snake-case strings.

- [ ] **Step 5: Run focused Rust tests and clippy for the touched library surface**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib app_error::tests -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib web::handlers::error::tests -- --nocapture`

Expected: PASS, including the 100-caller single-reservation assertion and stable HTTP mappings.

Run: `cd src-tauri && cargo clippy --no-default-features --features server,test-utils --lib -- -D warnings`

Expected: PASS with no lock held across an await warning and no secret-bearing debug serialization.

- [ ] **Step 6: Commit Task 1**

```bash
git add src-tauri/src/acp/shared_session.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/error.rs src-tauri/src/app_error.rs src-tauri/src/web/handlers/error.rs src/lib/types.ts
git commit -m "feat: add shared ACP broker primitives"
```

### Task 2: Registered Spawn and Asynchronous Shared Bootstrap

**Files:**
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/session_state.rs`
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/shared_session/dto.rs`
- Modify: `src-tauri/src/acp/shared_session/tests.rs`

**Interfaces:**
- Consumes: Task 1 `SharedSessionBroker::reserve_or_attach`, `SharedLaunchIdentity`, `SharedSessionPhase`, `SharedSessionError`, and existing `spawn_agent_connection -> SpawnHandshake`.
- Produces: `SharedConnectLaunch`, `ConnectionManager::connect_or_attach_shared`, `ConnectionManager::shared_session_broker`, `ConnectionManager::start_registered_shared_root`, a per-record registration notification, and owned registration/bootstrap tasks that move `Reserved -> Bootstrapping | Failed` and then `Bootstrapping -> Ready | Failed` for the exact generation.

Preserve Task 1's reviewed module boundary: public/redacted shared-session DTOs go
in `shared_session/dto.rs`, broker record synchronization and lifecycle methods
stay in `shared_session.rs`, and broker-focused tests go in
`shared_session/tests.rs`. `SessionState::prepare_registered_replacement` and its
state-preservation unit test belong in `session_state.rs`; do not hide that change
inside `connection.rs`.

- [ ] **Step 1: Write failing manager tests for fast registration, bootstrap single-flight, concurrent distinct roots, and required-companion failure policy**

Add tests using this test-only injection point. Production has no trait indirection: when the optional field is `None`, `connect_or_attach_shared` calls `start_registered_shared_root` directly. The fake returns a `RegisteredSpawnAttempt` with a real `SessionState::new`, `EventEmitter::Noop`, fixed `connection_incarnation`, and controlled `oneshot::Receiver<RouteBootstrapOutcome>`.

```rust
#[cfg(any(test, feature = "test-utils"))]
#[async_trait::async_trait]
pub trait SharedSpawnDriver: Send + Sync {
    async fn start(
        &self,
        connection_id: String,
        launch: SharedConnectLaunch,
        existing_public_state: Option<Arc<RwLock<SessionState>>>,
    ) -> Result<RegisteredSpawnAttempt, AcpError>;
}
```

Under the same cfg, `ConnectionManager` contains `shared_spawn_override: Option<Arc<dyn SharedSpawnDriver>>`; `new()` initializes `None`, `clone_ref()` clones it, and `new_with_shared_spawn_driver` sets `Some`. Cover:

```rust
#[tokio::test]
async fn shared_connect_returns_before_bootstrap_settles() {
    let (driver, gate) = FakeSharedSpawnDriver::pending();
    let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
    let response = tokio::time::timeout(
        Duration::from_millis(100),
        manager.connect_or_attach_shared(shared_launch(41, "client-a")),
    ).await.expect("reservation must return without route readiness").unwrap();
    assert_eq!(response.phase, SharedSessionPhase::Bootstrapping);
    assert!(manager.get_state(&response.connection_id).await.is_some());
    assert_eq!(manager.shared_spawn_count_for_test(), 1);
    gate.send(RouteBootstrapOutcome::Ready).unwrap();
    manager.wait_for_shared_phase(&response.connection_id, response.generation, SharedSessionPhase::Ready).await.unwrap();
}

#[tokio::test]
async fn concurrent_same_conversation_starts_one_driver() {
    let manager = manager_with_immediate_ready_driver();
    let results = futures::future::join_all((0..10).map(|n| {
        let manager = manager.clone_ref();
        async move { manager.connect_or_attach_shared(shared_launch(55, &format!("c-{n}"))).await.unwrap() }
    })).await;
    assert!(results.windows(2).all(|pair| pair[0].connection_id == pair[1].connection_id));
    assert_eq!(manager.shared_spawn_count_for_test(), 1);
}

#[tokio::test]
async fn cancelled_creator_cannot_strand_reserved_or_block_attachers() {
    let (manager, registration_gate) = manager_with_blocked_registration();
    let creator = tokio::spawn(manager.clone_ref().connect_or_attach_shared(shared_launch(56, "creator")));
    wait_until_broker_reserved(&manager, 56).await;
    creator.abort();
    let attacher = tokio::spawn(manager.clone_ref().connect_or_attach_shared(shared_launch(56, "attacher")));
    registration_gate.send(RegisteredSpawnFixture::success()).unwrap();
    let response = tokio::time::timeout(Duration::from_millis(100), attacher)
        .await.expect("owned registration must outlive the cancelled HTTP caller")
        .unwrap().unwrap();
    assert_ne!(response.phase, SharedSessionPhase::Reserved);
    assert!(manager.get_state(&response.connection_id).await.is_some());
}

#[tokio::test]
async fn persisted_registration_binds_ids_before_connect_response() {
    let manager = manager_with_pending_bootstrap_driver();
    let response = manager.connect_or_attach_shared(shared_launch_with_folder(57, 9, "client")).await.unwrap();
    let state = manager.get_state(&response.connection_id).await.unwrap();
    let state = state.read().await;
    assert_eq!(state.conversation_id, Some(57));
    assert_eq!(state.folder_id, Some(9));
    assert_eq!(response.phase, SharedSessionPhase::Bootstrapping);
}

#[test]
fn explicit_codeg_route_never_falls_back_after_companion_failure() {
    let plan = codeg_route_plan(DelegationRouteSource::SessionOverride);
    assert_eq!(
        shared_bootstrap_action(&plan, RouteBootstrapOutcome::RouteSpecific(RouteDegradedReason::CompanionInitializationFailed)),
        SharedBootstrapAction::Fail(SharedSessionError::CompanionInitializationFailed)
    );
}
```

Add a legacy regression where `build_agent` fails after registration while a session-id dedup lock is held; the typed Fatal outcome must return promptly without waiting for the session-start handshake timeout, and the exact registered map entry must be removed.

The helper contract is exact: `shared_launch` and `shared_launch_with_folder` construct `SharedConnectLaunch` with an in-memory test `DatabaseConnection`, `ConnectionPurpose::User`, `SessionAttachMode::Default`, matching route/shell fingerprints, valid labels, and a persisted `Conversation` key; `manager_with_blocked_registration` gates the fake before it returns `RegisteredSpawnAttempt`; `wait_until_broker_reserved` observes the broker diagnostic watch rather than sleeping; `manager_with_pending_bootstrap_driver` releases registration immediately and retains only the route-bootstrap sender; and `RegisteredSpawnFixture::success()` creates the same public connection id/state Arc the request supplied. Every timeout is a deadlock bound, not a timing source; helpers signal readiness through `Notify`, `watch`, or `oneshot`.

- [ ] **Step 2: Run manager tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::shared_ -- --nocapture`

Expected: FAIL because shared launch/driver APIs do not exist.

- [ ] **Step 3: Extract registered spawn from the current wait-for-readiness loop without changing legacy callers**

In `connection.rs`, change the internal return value of `spawn_agent_connection` from `SpawnHandshake` to a registered attempt that owns the handshake. Update the existing manager caller to destructure `.handshake`, so every legacy caller still waits on the same two receivers and observes unchanged behavior:

```rust
pub struct RegisteredSpawnAttempt {
    pub connection_id: String,
    pub connection_incarnation: String,
    pub state: Arc<tokio::sync::RwLock<SessionState>>,
    pub emitter: EventEmitter,
    pub handshake: SpawnHandshake,
    pub route_plan: DelegationRoutePlan,
}
```

Add `existing_public_state: Option<Arc<RwLock<SessionState>>>` to this internal function only; the existing manager caller passes `None`. Move fallible `build_agent`/process-driver initialization into the already-owned connection driver after `AgentConnection` map insertion and `task_abort` installation. If that initialization fails, send `RouteBootstrapOutcome::Fatal` through the typed bootstrap channel and let the existing exact-incarnation cleanup guard remove the manager entry. Return `RegisteredSpawnAttempt` immediately after registration, before that driver can fail, so the broker receives the exact state Arc and emitter and can retain them through a Failed tombstone.

Because a legacy session-id spawn currently waits `session_started_rx` before reading `route_bootstrap_rx`, refactor that wait to select both receivers under the same handshake deadline. A Fatal or RouteSpecific outcome that arrives before `SessionStarted` is handled immediately; a Ready outcome is retained until the session-start result settles; after SessionStarted wins, continue waiting only for the route outcome. This preserves dedup-lock semantics while ensuring post-registration build failure does not turn into a handshake-timeout delay. Non-dedup callers continue to await only the route outcome.

When `existing_public_state` is `Some`, call a new `SessionState::prepare_registered_replacement` under its write lock before manager-map insertion. It requires the same public connection id; preserves the event stream/ring, `event_seq`, shared projection, conversation/folder ids, and immutable client-visible history; installs the fresh `connection_incarnation`, route snapshot, tool/MCP registries, purpose, working directory, and launch metadata; resets driver-owned transient connection fields to the same values as a new `SessionState`; and sets status to `Connecting`. The new `AgentConnection` and returned `RegisteredSpawnAttempt` must both carry that same fresh incarnation. This method is used only by permitted same-generation fallback and has a unit test proving the state Arc and event sequence are unchanged while the incarnation changes.

In `manager.rs`, extract the current `finalize_acp_launch_config` + route validation + one call to `spawn_agent_connection` into:

```rust
#[allow(clippy::too_many_arguments)]
async fn start_registered_shared_root(
    &self,
    connection_id: String,
    agent_type: AgentType,
    working_dir: Option<String>,
    session_id: Option<String>,
    launch_inputs: AcpLaunchInputs,
    emitter: EventEmitter,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    launch_context: ConnectionLaunchContext,
    session_attach_mode: crate::acp::session_attach::SessionAttachMode,
    existing_public_state: Option<Arc<tokio::sync::RwLock<SessionState>>>,
) -> Result<RegisteredSpawnAttempt, AcpError>;
```

It must call `spawn_agent_connection` with owner label `"shared-server"`, origin `Root`, no owner operation, no parent connection, the preallocated broker connection id, and return immediately after `spawn_agent_connection` has inserted the `AgentConnection` and installed `task_abort`. Existing `spawn_agent*` methods continue through their current route-bootstrap wait/fallback loop.

`start_registered_shared_root` runs `finalize_acp_launch_config` exactly once for the supplied immutable `AcpLaunchInputs`, then rejects with `ConfigConflict` before spawn if the resulting route fingerprint, terminal selection key, purpose, attach mode, agent type, external session, or normalized working-directory fingerprint differs from `SharedLaunchIdentity`. This prevents a settings or call-site mismatch from reserving one identity and launching another.

- [ ] **Step 4: Implement `connect_or_attach_shared` and exact-generation bootstrap settlement**

Define:

```rust
pub struct SharedConnectLaunch {
    pub database: sea_orm::DatabaseConnection,
    pub key: SharedSessionKey,
    pub conversation_id: Option<i32>,
    pub folder_id: Option<i32>,
    pub launch_identity: SharedLaunchIdentity,
    pub agent_type: AgentType,
    pub working_dir: Option<String>,
    pub external_session_id: Option<String>,
    pub launch_inputs: AcpLaunchInputs,
    pub emitter: EventEmitter,
    pub preferred_mode_id: Option<String>,
    pub preferred_config_values: BTreeMap<String, String>,
    pub launch_context: ConnectionLaunchContext,
    pub session_attach_mode: crate::acp::session_attach::SessionAttachMode,
    pub device_id: String,
    pub client_instance_id: String,
    pub request_id: String,
    pub retry_failed_generation: Option<u64>,
}
```

`SharedConnectLaunch` does not implement `Debug`. The creator clones `database` before moving the remaining launch into the owned registration task; Task 5 captures that clone in the one dispatcher. Attachers never replace the creator's database handle. `DatabaseConnection` is already a cheap cloneable SeaORM handle and is not part of launch identity or diagnostics.

`connect_or_attach_shared` preallocates a UUID and calls `reserve_or_attach`. Immediately after a winning `created` reservation returns, and before the method reaches another cancellation point, it starts one owned Tokio registration task. Every caller, including the winner, then waits on the record's registration `watch` channel only until the phase is no longer internal `Reserved` and the exact `SessionState` is present. The owned task performs `start_registered_shared_root`, stores its exact internal `connection_incarnation`, installs persisted `conversation_id` and `folder_id` into `SessionState` before notifying waiters, changes `Reserved -> Bootstrapping`, and starts the bootstrap settler:

```rust
let manager = self.clone_ref();
let connection_id = attachment.connection_id.clone();
let generation = attachment.generation;
let driver_incarnation = registered.connection_incarnation.clone();
tokio::spawn(async move {
    let outcome = registered.handshake.route_bootstrap_rx.await
        .unwrap_or(RouteBootstrapOutcome::Fatal(AcpError::ProcessExited));
    manager.settle_shared_bootstrap(
        connection_id,
        generation,
        driver_incarnation,
        registered.route_plan,
        outcome,
    ).await;
});
```

The record owns a `watch::Sender<SharedRegistrationState>` initialized to `Reserved`; `wait_until_registered` returns only `Bootstrapping` or `Failed`, never `Reserved`. It also owns a separate lifecycle watch whose terminal values are `Failed`, `Closing`, `Removed`, and `Replaced`; Task 1 failed-generation replacement publishes `Replaced` in the same map/record `try_lock` critical section before swapping pointers. Spawn both registration and bootstrap settlement as owned futures with supervisors that await their `JoinHandle`s; a panic/abort makes the supervisor generation/driver-incarnation-CAS the record to `Failed(SessionUnavailable)` and notify waiters/tasks. A dropped bootstrap oneshot is the same typed failure, not an indefinitely Bootstrapping record. A test-only registration gate must abort the original connect future after reservation and prove a second caller still completes. On successful registration, store the returned public state/emitter in the broker record before notifying. If registration itself fails or either owned task panics, create/store a minimal failed public `SessionState` with the preallocated id, immutable launch facts, persisted ids, `ConnectionStatus::Error`, and failed shared projection, then notify all waiters. Run `teardown_unexposed_attempt`, set `cleanup_complete` only after driver/process termination and connection-map absence, and return a normal attachment whose phase/error snapshot is observable to every concurrent client. Never remove the broker record before concurrent attachers can observe the typed failure. Assert `launch_context.purpose == launch_identity.purpose` and `session_attach_mode == launch_identity.attach_mode` before reservation; persisted `conversation_id` and `folder_id` are written into the registered state before the watch publishes `Bootstrapping`.

- [ ] **Step 5: Implement route-policy classification and prove all ACP agents use the same path**

Use route plan facts, not `AgentType`, to choose:

```rust
enum SharedBootstrapAction {
    Ready,
    AllowedFallback(RouteDegradedReason),
    Fail(SharedSessionError),
}

fn shared_bootstrap_action(
    plan: &DelegationRoutePlan,
    outcome: RouteBootstrapOutcome,
) -> SharedBootstrapAction {
    match outcome {
        RouteBootstrapOutcome::Ready => SharedBootstrapAction::Ready,
        RouteBootstrapOutcome::RouteSpecific(reason)
            if plan.requested == DelegationRoutePolicy::Codeg
                && plan.source == DelegationRouteSource::SessionOverride =>
        {
            SharedBootstrapAction::Fail(map_route_failure(reason))
        }
        RouteBootstrapOutcome::RouteSpecific(reason)
            if plan.managed
                && plan.effective == DelegationRoutePolicy::Codeg
                && plan.source == DelegationRouteSource::GlobalDefault =>
        {
            SharedBootstrapAction::AllowedFallback(reason)
        }
        RouteBootstrapOutcome::RouteSpecific(reason) =>
            SharedBootstrapAction::Fail(map_route_failure(reason)),
        RouteBootstrapOutcome::Fatal(_) => {
            SharedBootstrapAction::Fail(SharedSessionError::SessionUnavailable)
        }
    }
}
```

`map_route_failure` maps every route-specific required-companion failure (`NativeSuppressionUnsupported`, `NativeSuppressionInvalid`, `CompanionBinaryUnavailable`, `AgentMcpUnsupported`, and `CompanionInitializationFailed`) to the public stable `CompanionInitializationFailed` error while retaining only the typed reason in secret-safe metrics. No display-string parsing participates in classification.

For `AllowedFallback`, keep the broker generation in `Bootstrapping`, perform bounded teardown of the registered failed-route attempt, and observe that the old driver task/process has terminated and the manager connection map no longer contains it. Then start the permitted fallback attempt with the same broker-preallocated public `connection_id`, same generation, and the exact retained public `SessionState` Arc passed as `existing_public_state`. The replacement AgentConnection gets a fresh internal `connection_incarnation` cleanup fence but reuses the public state/event ring, so already attached sockets remain on one monotonic event sequence through fallback. Store the new incarnation in the broker before enabling its monitor; callbacks from the prior incarnation then fail their broker CAS, while complete old-driver teardown prevents them from mutating the reused `SessionState`. The broker record remains authoritative during the bounded gap; attachers read the retained state and observe `Bootstrapping` instead of receiving a different id. Never mark this permitted same-generation fallback `Closing`, never allocate/expose another public id, and never start the replacement before the old driver/process and connection-map entry are gone. An explicit session-override Codeg route always fails closed and never enters this fallback path. Add tests for same-id permitted fallback, continuous state Arc/event sequence, map and driver absence before replacement spawn, and fail-closed session override. Add a registry-driven test that iterates `BUILTIN_AGENT_TYPES` plus `AgentType::custom("fixture").expect("valid fixture id")`, creates distinct shared roots, and asserts broker code never branches on concrete agent variants.

- [ ] **Step 6: Run focused manager/connection tests**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::shared_ -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::connection::tests::bootstrap_outcome_typed_only_no_substring_fallback -- --nocapture`

Expected: PASS; fast-return test completes before the readiness sender fires, and explicit companion failure records the stable failure without fallback.

- [ ] **Step 7: Commit Task 2**

```bash
git add src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/shared_session.rs src-tauri/src/acp/shared_session/dto.rs src-tauri/src/acp/shared_session/tests.rs
git commit -m "feat: register shared ACP bootstrap asynchronously"
```

### Task 3: Shared Snapshot Projection and Event Vocabulary

**Files:**
- Modify: `src-tauri/src/acp/types.rs`
- Modify: `src-tauri/src/acp/session_state.rs`
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/snapshot-denormalize.ts`
- Test: `src/lib/snapshot-denormalize.test.ts`

**Interfaces:**
- Consumes: Task 1 `SharedSessionProjection`, `SharedSessionPhase`, queue/turn projections and existing `emit_with_state` event ordering.
- Produces: `LiveSessionSnapshot.shared_session`, five queue events, phase/turn events, `SnapshotPatch.sharedSession`, and reducers that reconstruct the full shared state after replay gaps.

- [ ] **Step 1: Write failing Rust and TypeScript snapshot reconstruction tests**

Add a Rust test that applies `SharedSessionPhaseChanged`, two `PromptQueued`, `PromptQueueItemCancelled`, and `PromptDispatchStarted`, serializes `to_snapshot`, and asserts exact queue order plus active turn. Add a TypeScript test:

```ts
it("denormalizes a complete shared-session projection", () => {
  const patch = denormalizeSnapshot(baseSnapshot({
    shared_session: {
      generation: 3,
      phase: { phase: "ready" },
      queue: [{ queue_item_id: "q2", enqueue_seq: 2, client_message_id: "m2", visible_text: "later", visible_text_truncated: false, attachment_count: 0, submitted_at: "2026-08-16T00:00:00Z", state: "queued" }],
      active_turn: { turn_id: "turn-1", queue_item_id: "q1", enqueue_seq: 1, client_message_id: "m1", stop_requested: false },
      lease_expires_at: "2026-08-16T00:01:30Z",
    },
  }))
  expect(patch.sharedSession?.generation).toBe(3)
  expect(patch.sharedSession?.queue.map((item) => item.enqueueSeq)).toEqual([2])
  expect(patch.sharedSession?.activeTurn?.turnId).toBe("turn-1")
})
```

- [ ] **Step 2: Run the focused tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::session_state::tests::shared_session_projection -- --nocapture`

Expected: FAIL because shared variants and snapshot field are absent.

Run: `pnpm test -- src/lib/snapshot-denormalize.test.ts`

Expected: FAIL because `shared_session`/`sharedSession` are absent.

- [ ] **Step 3: Add exact Rust event variants and snapshot structures**

Add these variants to `AcpEvent` with snake-case serde names:

```rust
SharedSessionPhaseChanged { generation: u64, phase: SharedSessionPhase },
PromptQueued { generation: u64, item: SharedQueuedPromptSummary },
PromptQueueItemCancelled { generation: u64, queue_item_id: String },
PromptDispatchStarted { generation: u64, turn: SharedActiveTurnProjection },
PromptQueueItemFailed { generation: u64, queue_item_id: String, error_code: String },
PromptQueueDepthChanged { generation: u64, waiting_count: u32, waiting_bytes: u64 },
SharedTurnSettled { generation: u64, turn_id: String, outcome: SharedTurnOutcome },
```

Add `pub shared_session: Option<SharedSessionProjection>` to `SessionState` and `LiveSessionSnapshot` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. `SessionState::apply_event` must ignore an event whose generation differs from the installed projection, sort queue entries by `enqueue_seq`, remove only matching ids, and clear `active_turn` only for matching `turn_id`.

Retrofit Task 2 registration so, before its registration watch publishes `Bootstrapping`, the exact `SessionState` gets `shared_session = Some(SharedSessionProjection { generation, phase: Bootstrapping, queue: [], active_turn: None, lease_expires_at: None })`. Failed registration installs/updates the same projection to `Failed`; the per-subscription overlay in Task 4 is the only code that writes a non-`None` lease expiry into a serialized clone.

- [ ] **Step 4: Publish broker commits through one helper after releasing broker locks**

Implement in `ConnectionManager` or `SharedSessionBroker` integration:

```rust
async fn publish_shared_event(&self, connection_id: &str, event: AcpEvent) -> Result<(), AcpError> {
    let handles = self.shared_session_broker()
        .public_state_and_emitter(connection_id).await;
    let (state, emitter) = match handles {
        Some(handles) => handles,
        None => self.get_state_and_emitter(connection_id).await
            .ok_or_else(|| AcpError::ConnectionNotFound(connection_id.into()))?,
    };
    emit_with_state(&state, &emitter, event).await;
    Ok(())
}
```

`public_state_and_emitter` clones the stored handles under the record lock and releases it before returning. Reuse the existing public `ConnectionManager::get_state_and_emitter(&str) -> Option<(Arc<RwLock<SessionState>>, EventEmitter)>`; do not add a duplicate helper. Every broker mutation returns an owned `Vec<AcpEvent>` after committing its record; callers publish after the record lock is dropped. Never call `emit_with_state` from inside a broker lock.

- [ ] **Step 5: Mirror the wire types and denormalization in TypeScript**

Add the exact wire union to `src/lib/types.ts`:

```ts
export type SharedSessionPhase =
  | { phase: "reserved" }
  | { phase: "bootstrapping" }
  | { phase: "ready" }
  | { phase: "failed"; error_code: string; cleanup_complete: boolean }
  | { phase: "closing" }
```

Add `SharedQueuedPromptSummary`, `SharedActiveTurnProjection`, `SharedSessionProjection` and the event union members there. The queue summary mirrors every Task 1 field, including `visible_text_truncated` and `attachment_count`. Add camel-case UI types `SharedSessionPhaseView`, `SharedQueuedPrompt`, and `SharedActiveTurn` in `snapshot-denormalize.ts`; the failed view uses `errorCode`/`cleanupComplete`. Map every field explicitly and do not spread snake-case wire objects into `ConnectionState`.

Keep these three phase domains distinct throughout later tasks:

```ts
// Rust snapshot/event wire object from this task.
export type SharedSessionPhase =
  | { phase: "reserved" }
  | { phase: "bootstrapping" }
  | { phase: "ready" }
  | { phase: "failed"; error_code: string; cleanup_complete: boolean }
  | { phase: "closing" }

// Frontend reducer/view object produced only by explicit mapping.
export type SharedSessionPhaseView =
  | { phase: "reserved" }
  | { phase: "bootstrapping" }
  | { phase: "ready" }
  | { phase: "failed"; errorCode: string; cleanupComplete: boolean }
  | { phase: "closing" }

// Task 8 connect response string; it is never assigned directly to either
// object union above.
export type SharedPublicPhase =
  | "bootstrapping"
  | "ready"
  | "failed"
  | "closing"
```

The `reserved` view variant exists only so one mapping function remains exhaustive over replay/snapshot data; Task 10 must never construct it from an HTTP response or expose it after connect completes.

- [ ] **Step 6: Run Rust and frontend snapshot tests**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::session_state::tests::shared_session_projection -- --nocapture`

Expected: PASS and serialized empty legacy snapshots omit `shared_session`.

Run: `pnpm test -- src/lib/snapshot-denormalize.test.ts`

Expected: PASS with exact generation, turn id, and enqueue order.

- [ ] **Step 7: Commit Task 3**

```bash
git add src-tauri/src/acp/types.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/shared_session.rs src-tauri/src/acp/manager.rs src/lib/types.ts src/lib/snapshot-denormalize.ts src/lib/snapshot-denormalize.test.ts
git commit -m "feat: project shared ACP state into snapshots"
```

### Task 4: Client Leases and Lease-Bound WebSocket Attach

**Files:**
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/web/ws_attach.rs`
- Modify: `src-tauri/src/web/ws.rs`
- Modify: `src-tauri/tests/ws_attach.rs`

**Interfaces:**
- Consumes: Task 1 `SharedMutationGuard`, lease TTL/config, and Task 3 `LiveSessionSnapshot.shared_session`.
- Produces: `SharedSessionBroker::validate_guard`, `validate_and_bind_lease`, `renew_leases`, `release_lease`, `LeaseSocketBinding`, lease-aware `ClientMsg::Attach`, and `DetachReason::{GenerationStale, LeaseMissing, LeaseExpired, SessionReplaced}`.

- [ ] **Step 1: Write failing lease and WebSocket tests**

Add broker tests using `#[tokio::test(start_paused = true)]`:

```rust
#[tokio::test(start_paused = true)]
async fn heartbeat_renews_only_bound_leases_and_expiry_never_disconnects() {
    let broker = broker_with_ttl(Duration::from_secs(90));
    let a = reserve_client(&broker, 1, "a").await;
    let b = attach_client(&broker, 1, "b").await;
    tokio::time::advance(Duration::from_secs(60)).await;
    broker.renew_leases(&[LeaseSocketBinding::from(&a)]).await;
    tokio::time::advance(Duration::from_secs(31)).await;
    let expired = broker.expire_leases(tokio::time::Instant::now()).await;
    assert_eq!(expired, vec![b.lease_id]);
    assert!(broker.validate_guard(&a.guard()).await.is_ok());
    assert!(matches!(broker.validate_guard(&b.guard()).await, Err(SharedSessionError::LeaseExpired)));
    assert_eq!(broker.diagnostic_for_connection(&a.connection_id).await.unwrap().phase, SharedSessionPhase::Bootstrapping);
}

#[tokio::test(start_paused = true)]
async fn expired_lease_tombstones_are_bounded_and_secret_safe() {
    let broker = broker_with_ttl(Duration::from_secs(1));
    let oldest = reserve_client(&broker, 1, "oldest").await;
    fill_and_expire_lease_tombstones(&broker, MAX_EXPIRED_LEASE_TOMBSTONES + 1).await;
    assert!(matches!(broker.validate_guard(&oldest.guard()).await, Err(SharedSessionError::LeaseMissing)));
    let newest = newest_expired_guard(&broker).await;
    assert!(matches!(broker.validate_guard(&newest).await, Err(SharedSessionError::LeaseExpired)));
    let diagnostic = broker.diagnostic_for_connection(&newest.connection_id).await.unwrap();
    assert_eq!(diagnostic.expired_lease_tombstone_count, MAX_EXPIRED_LEASE_TOMBSTONES);
}
```

The broker test helpers use production methods and paused Tokio time: `broker_with_ttl` overrides only TTL; `reserve_client` creates conversation 1 and returns its complete attachment; `attach_client` uses a distinct valid request/client; `fill_and_expire_lease_tombstones` repeatedly attaches, advances past TTL, and calls `expire_leases`; and `newest_expired_guard` returns the guard captured from the last attachment rather than inspecting private tombstone storage. No helper mutates the record map directly.

Extend `src-tauri/tests/ws_attach.rs` with two independently leased tabs, an attach using the wrong generation, a ping after 60 paused seconds, and a snapshot assertion that `shared_session.lease_expires_at` belongs to the attaching lease.

Add a generation-retry case: after failed-generation cleanup and replacement, the old live subscription's next ping receives one terminal `detached { reason: "session_replaced" }`, while a new subscription attaches to the new connection id/generation. A direct reattach using the old id receives the same reason from the bounded replacement tombstone.

- [ ] **Step 2: Run focused tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib shared_session::tests::heartbeat_ -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --test ws_attach ws_attach_shared_ -- --nocapture`

Expected: FAIL because lease-bound attach and renewal methods are absent.

- [ ] **Step 3: Implement lease validation, renewal, release, and expiry**

Use random UUID lease ids. Store both monotonic expiry and the wall-clock expiry returned on the wire:

```rust
struct ClientLease {
    lease_id: String,
    client_instance_id: String,
    expires_at: tokio::time::Instant,
    expires_at_utc: chrono::DateTime<chrono::Utc>,
}

pub struct LeaseSocketBinding {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
    pub lease_expires_at: chrono::DateTime<chrono::Utc>,
}

pub enum LeaseRenewalOutcome {
    Renewed(LeaseSocketBinding),
    Detached(DetachReason),
}
```

`attach_or_renew_lease` is idempotent for the bounded connect-request ledger key `(generation, device_id, client_instance_id, request_id)`: while its recorded lease is active, an identical transport retry returns the same complete attachment/result, including the original disposition. If that recorded lease was explicitly released or expired, a new `connect_or_attach` using the same request key allocates a fresh lease, changes the stored disposition to `Attached`, and updates the ledger; only mutation endpoints are forbidden from reviving expiry. A different request from an already attached `(generation, client_instance_id)` renews and returns that client's existing lease with `Attached`. Reject a new active client at 256 leases and a new request identity at 4,096 ledger entries using the Task 1 capacity errors; do not evict live-generation idempotency entries. `validate_guard` checks connection index, generation, active lease, monotonic expiry, then the bounded expired-id FIFO. `expire_leases` moves expired ids into the 1,024-entry tombstone FIFO; recent ids return `LeaseExpired`, while evicted/unknown ids return `LeaseMissing`. `release_lease` validates the current connection/generation, removes only the matching active lease, updates the lease gauge, and returns `Ok(false)` for any non-active lease id on that current generation. This makes release idempotent without retaining another secret-bearing tombstone set and never affects another lease or the process.

When Task 1 replaces a failed generation, atomically push `(old_connection_id, old_generation)` into a 4,096-entry FIFO tombstone and evict the oldest. `validate_and_bind_lease` checks this tombstone before returning generic missing and maps a match to `DetachReason::SessionReplaced`; HTTP mutation validation maps the same stale identity to `GenerationStale`. `renew_leases` returns one `LeaseRenewalOutcome` for every input binding in input order. `Renewed` carries a binding with both monotonic state updated internally and the new wall-clock expiry for the subscription; `Detached` carries the terminal reason. This keeps replacement detection bounded without retaining old lease/client ids.

- [ ] **Step 4: Extend WebSocket attach and bind subscriptions to leases**

Change the wire enum to:

```rust
Attach {
    subscription_id: String,
    connection_id: String,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default)]
    lease_id: Option<String>,
    #[serde(default)]
    since_seq: Option<u64>,
},
```

`handle_attach` must call `manager.shared_session_broker().validate_and_bind_lease(connection_id, generation, lease_id)` before reading `SessionState`. For broker-managed ids, both optional fields must be `Some` and failure returns the matching new detach reason; read the broker record's retained public state/event receiver so a Failed tombstone remains attachable after manager-map cleanup. For legacy/non-broker ids, accept only when both are `None` and read the existing manager state; reject half-fenced or fenced unknown ids. This keeps local/delegated attach tests intact while bundled shared clients always send both.

Store `binding: Option<LeaseSocketBinding>` in `ActiveSubscription`. On `ClientMsg::Ping`, deduplicate all bindings by `(connection_id, generation, lease_id)`, call `renew_leases`, update renewed expiries, remove every binding with a terminal outcome, send that subscription one `Detached` frame with the mapped reason, and then send `Pong`. Socket close and client `Detach` abort only the forwarder; they do not release a lease.

- [ ] **Step 5: Overlay this subscription's lease expiry onto cold snapshots**

After broker validation and before serializing `ServerMsg::Snapshot`, set only the cloned snapshot projection:

```rust
let mut snapshot = s.to_snapshot();
if let (Some(shared), Some(binding)) = (snapshot.shared_session.as_mut(), binding.as_ref()) {
    shared.lease_expires_at = Some(binding.lease_expires_at);
}
```

Do not write the attaching lease expiry into the durable `SessionState` projection because different subscriptions have different leases.

- [ ] **Step 6: Run broker and WebSocket tests**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib shared_session::tests::heartbeat_ -- --nocapture`

Expected: PASS; only the unbound lease expires.

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --test ws_attach -- --nocapture`

Expected: PASS for legacy attach plus generation/lease validation, ping renewal, and per-lease snapshot expiry.

- [ ] **Step 7: Commit Task 4**

```bash
git add src-tauri/src/acp/shared_session.rs src-tauri/src/acp/manager.rs src-tauri/src/web/ws_attach.rs src-tauri/src/web/ws.rs src-tauri/tests/ws_attach.rs
git commit -m "feat: bind shared ACP streams to client leases"
```

### Task 5: Bounded Idempotent FIFO and Single Dispatcher

**Files:**
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/types.rs`
- Modify: `src-tauri/src/acp/session_state.rs`

**Interfaces:**
- Consumes: existing `ConnectionManager::send_prompt_linked_with_message_id`, Task 1 queue limits/error types, Task 3 queue events, and Task 4 mutation guards.
- Produces: `SharedPromptRequest`, `PromptEnqueueResult`, `SharedSessionBroker::bind_conversation_key`, `ConnectionManager::enqueue_shared_prompt`, `cancel_shared_queued_prompt`, and one per-session dispatcher awakened by `tokio::sync::Notify`.

- [ ] **Step 1: Write failing FIFO, idempotency, capacity, cancel-race, and sender-expiry tests**

Add deterministic broker tests:

```rust
#[tokio::test]
async fn concurrent_enqueues_assign_contiguous_fifo_sequence() {
    let fixture = ready_broker_fixture().await;
    let results = futures::future::join_all((0..64).map(|n| {
        let fixture = fixture.clone();
        async move { fixture.enqueue(prompt_request(n)).await.unwrap() }
    })).await;
    let mut seqs: Vec<_> = results.into_iter().map(|r| r.enqueue_seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=64).collect::<Vec<_>>());
}

#[tokio::test]
async fn identical_retry_returns_original_and_changed_payload_conflicts() {
    let fixture = ready_broker_fixture().await;
    let first = fixture.enqueue(prompt_with_ids("client", "retry", "alpha")).await.unwrap();
    let same = fixture.enqueue(prompt_with_ids("client", "retry", "alpha")).await.unwrap();
    assert_eq!(first, same);
    assert!(matches!(
        fixture.enqueue(prompt_with_ids("client", "retry", "beta")).await,
        Err(SharedSessionError::IdempotencyKeyConflict)
    ));
}

#[tokio::test]
async fn limits_reject_new_item_without_dropping_existing_items() {
    let fixture = ready_broker_fixture().await;
    for n in 0..MAX_WAITING_PROMPTS { fixture.enqueue(prompt_request(n)).await.unwrap(); }
    assert!(matches!(fixture.enqueue(prompt_request(65)).await, Err(SharedSessionError::PromptQueueFull)));
    assert_eq!(fixture.snapshot().await.queue.len(), MAX_WAITING_PROMPTS);
}

#[tokio::test]
async fn waiting_byte_limit_rejects_only_the_new_item() {
    let first_request = prompt_with_ids("client", "bytes-a", "alpha");
    let first_bytes = canonical_prompt_bytes(&first_request).len();
    let fixture = ready_broker_fixture_with_limits(
        MAX_PROMPT_LEDGER_ENTRIES,
        MAX_WAITING_PROMPTS,
        first_bytes,
    ).await;
    let first = fixture.enqueue(first_request).await.unwrap();
    assert!(matches!(
        fixture.enqueue(prompt_with_ids("client", "bytes-b", "beta")).await,
        Err(SharedSessionError::PromptQueueFull)
    ));
    assert_eq!(fixture.snapshot().await.queue[0].queue_item_id, first.queue_item_id);
}

#[tokio::test]
async fn prompt_ledger_capacity_keeps_existing_retry_available() {
    let fixture = ready_broker_fixture_with_limits(2, MAX_WAITING_PROMPTS, MAX_WAITING_BYTES).await;
    let first = fixture.enqueue(prompt_with_ids("client", "retry-a", "alpha")).await.unwrap();
    fixture.enqueue(prompt_with_ids("client", "retry-b", "beta")).await.unwrap();
    assert_eq!(
        fixture.enqueue(prompt_with_ids("client", "retry-a", "alpha")).await.unwrap(),
        first,
    );
    assert!(matches!(
        fixture.enqueue(prompt_with_ids("client", "retry-c", "gamma")).await,
        Err(SharedSessionError::PromptLedgerCapacityExceeded)
    ));
}

#[tokio::test]
async fn cancel_and_dispatch_have_one_linearizable_winner() {
    let fixture = ready_broker_fixture().await;
    let item = fixture.enqueue(prompt_request(1)).await.unwrap();
    let (cancel, claim) = tokio::join!(fixture.cancel(&item.queue_item_id), fixture.claim_head());
    assert_ne!(cancel.is_ok(), claim.is_ok());
    assert!(matches!(
        fixture.item_state(&item.queue_item_id).await,
        Some(InternalPromptState::Cancelled | InternalPromptState::Dispatching)
    ));
}

#[tokio::test]
async fn ephemeral_record_rekeys_before_conversation_linked_is_observable() {
    let fixture = ready_ephemeral_fixture().await;
    fixture.dispatch_prompt_that_creates_conversation(88).await.unwrap();
    assert_eq!(fixture.broker_key().await, SharedSessionKey::Conversation(88));
    assert!(fixture.events().await.iter().any(|event| matches!(
        event,
        AcpEvent::ConversationLinked { conversation_id: 88, .. }
    )));
}

#[tokio::test]
async fn conversation_rekey_collision_fails_closed() {
    let fixture = two_ready_records_fixture().await;
    let error = fixture.bind_second_record_to_first_conversation().await.unwrap_err();
    assert_eq!(error, SharedSessionError::ConversationKeyConflict);
    assert_eq!(fixture.spawn_count(), 2);
    assert_eq!(fixture.conversation_linked_event_count_for_second().await, 0);
}

#[tokio::test]
async fn enqueue_response_reflects_dispatch_claim_that_won_before_response() {
    let fixture = ready_broker_fixture_with_paused_response().await;
    let pending = fixture.begin_enqueue(prompt_request(1)).await;
    fixture.allow_dispatch_claim().await;
    let result = pending.finish_response().await.unwrap();
    assert_eq!(result.state, SharedQueuedPromptState::Dispatching);
}
```

All queue fixtures are thin wrappers over one real broker record. `ready_broker_fixture` reserves, installs a public state/emitter, and marks that exact generation `Ready + Connected`; `ready_broker_fixture_with_limits` overrides only the prompt-ledger/count/byte limits; `prompt_request` builds valid text blocks and unique ids; `prompt_with_ids` varies only the named idempotency fields and text; `ready_ephemeral_fixture` starts under a deterministic test-only ephemeral key; and `two_ready_records_fixture` creates two distinct Arcs so the collision test exercises pointer/index CAS. The paused-response fixture uses two explicit barriers inside `enqueue_shared_prompt` under `cfg(test)` and never relies on sleeps.

- [ ] **Step 2: Run queue tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests -- --nocapture`

Expected: FAIL because queue admission and claiming are undefined.

- [ ] **Step 3: Implement complete queue records, payload accounting, and idempotency**

Define:

```rust
#[derive(Clone)]
pub struct SharedPromptRequest {
    pub guard: SharedMutationGuard,
    pub client_instance_id: String,
    pub client_request_id: String,
    pub blocks: Vec<PromptInputBlock>,
    pub folder_id: Option<i32>,
    pub conversation_id: Option<i32>,
    pub client_message_id: String,
    pub capture: Option<PromptCaptureContext>,
    pub submitted_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptEnqueueResult {
    pub queue_item_id: String,
    pub enqueue_seq: u64,
    pub state: SharedQueuedPromptState,
}
```

Validate `client_instance_id` and `client_request_id` with the Task 1 ASCII-label validator. Serialize a canonical tuple of `blocks`, folder/conversation ids, `client_message_id`, and capture fields with `serde_json::to_vec`, hash it with SHA-256, and use the serialized byte length for waiting-byte accounting. Store the idempotency entry under `(generation, client_instance_id, client_request_id)` with `payload_hash`, queue item id, enqueue sequence, current `InternalPromptState`, and `frozen_result: Option<PromptEnqueueResult>`. The prompt ledger is bounded at 65,536 distinct keys for a live generation: identical retries remain available at capacity, changed payload still conflicts, and a new key returns `PromptLedgerCapacityExceeded`; do not evict a live key and risk double admission. Do not derive `Debug` for `SharedPromptRequest`, the internal queued record, canonical payload, interaction answer, or any container that transitively contains prompt/answer blocks. Validate the full request before insertion. Insert and assign `enqueue_seq` in one record-lock critical section, then return `PromptQueued` and `PromptQueueDepthChanged` events to publish after unlocking.

The manager publishes admission events and wakes the dispatcher after the record lock is released. Immediately before constructing the HTTP result, every new or identical-retry path calls `broker.finalize_enqueue_response(connection_id, generation, queue_item_id)`. Under the record lock, the first caller maps authoritative `Queued` to wire state `queued` and any already claimed or terminal item to wire state `dispatching`, stores that exact result in `frozen_result`, and returns it; later callers return the stored value byte-for-byte. If the original HTTP future was cancelled before finalization, a later retry performs the same freeze. No waiter or lock is held across event publication, and no response can claim `queued` after the dispatcher already won the claim.

- [ ] **Step 4: Implement queue cancellation and exact head claim transitions**

The only queue state transitions are:

```text
Queued -> Dispatching -> Completed | Failed | Cancelled
Queued -> Cancelled
```

`InternalPromptState` has those five variants and is never serialized directly. The public queue projection exposes only `Queued`; `active_turn` exposes dispatching work, and terminal events expose completion/failure/cancellation.

Internally, the waiting `VecDeque` contains only `Queued` records. A dispatch claim atomically pops the head, subtracts its waiting bytes, and moves its identity/projection into `active_turn`; cancelled records are removed and terminal state remains recoverable only through the bounded prompt-idempotency entry and emitted event. `cancel_shared_queued_prompt` validates any active lease, finds the exact id, removes it only while `Queued`, subtracts its serialized bytes, preserves all other order, and emits cancelled/depth events. If the item matches `active_turn` or its frozen result is already `dispatching`, return `QueueItemAlreadyDispatching`. There is no edit or reorder method.

- [ ] **Step 5: Start one dispatcher per created broker record**

`connect_or_attach_shared` starts a dispatcher once for the created record. On that
winning path, clone `launch.database` before moving the remaining launch fields into
the owned registration task, construct exactly
`Arc::new(crate::db::AppDatabase { conn: database })`, and move that `Arc` into the
single dispatcher. `AppDatabase` itself is not `Clone`; attachers neither contribute
nor replace the creator's database handle. The loop waits on the record's `Notify`,
then copies a `SharedRuntimeWorkSnapshot` from `SessionState` and calls pure
`claim_dispatchable_head`. A claim succeeds only when phase is `Ready`, status is
`Connected`, `turn_in_flight` is false, no broker active turn (including
stop-request quarantine) exists, no permission/question/plan approval or
continuation wait is pending, active delegation/background counts are zero, and the
queue head is `Queued`. Thus waiting-input/continuation/background work keeps later
prompts queued instead of turning an expected wait into a failed item.

```rust
pub struct SharedRuntimeWorkSnapshot {
    pub status: ConnectionStatus,
    pub turn_in_flight: bool,
    pub pending_permission_id: Option<String>,
    pub pending_question_id: Option<String>,
    pub pending_plan_approval_id: Option<String>,
    pub continuation_wait: bool,
    pub active_delegations: usize,
    pub background_outstanding: u32,
    pub conversation_writable: bool,
}
```

Build this value under one `SessionState` read lock, copying only the three correlation ids and scalar blockers, then release the lock. Resolve `conversation_writable` through the existing DB/delegate/workflow write guard before the broker call; no DB await occurs under a broker lock. `reconcile_runtime_snapshot` uses the exact optional ids to reconstruct a missed pending interaction after broadcast lag, retain a still-current one, and resolve a broker interaction absent from authoritative state. Booleans alone are insufficient because the next shared answer must CAS the exact interaction id.

For a claim:

```rust
let turn_id = uuid::Uuid::new_v4().to_string();
match broker.claim_dispatchable_head(
    &connection_id,
    generation,
    &turn_id,
    &session_snapshot,
).await? {
    DispatchHeadDecision::Blocked => {}
    DispatchHeadDecision::Failed(failed) => {
        for event in failed.events {
            manager.publish_shared_event(&connection_id, event).await?;
        }
        failed.notify.notify_one();
    }
    DispatchHeadDecision::Claimed(claimed) => {
        for event in claimed.events {
            manager.publish_shared_event(&connection_id, event).await?;
        }
        let result = manager.send_prompt_linked_with_message_id(
            &db,
            &connection_id,
            claimed.blocks,
            claimed.folder_id,
            claimed.conversation_id,
            None,
            Some(claimed.client_message_id),
            claimed.capture,
        ).await;
        if let Err(error) = result {
            let failed = broker.fail_claimed_item(
                &connection_id,
                generation,
                &turn_id,
                stable_dispatch_error(&error),
            ).await?;
            for event in failed.events {
                manager.publish_shared_event(&connection_id, event).await?;
            }
            failed.notify.notify_one();
        }
    }
}
```

`Blocked` leaves the head untouched. When every runtime blocker is clear but `conversation_writable` is false, the broker atomically returns `Failed` for the head with the existing stable workflow/conversation-not-writable code and does not create an active turn. `Claimed` pops exactly one waiting item, moves its projection into `active_turn`, and emits dispatch/depth events. The existing linked-send path still repeats its write validation after the claim; a race there follows `fail_claimed_item`.

`stable_dispatch_error` is a total allowlist over `AcpError`: preserve only an existing static workflow/conversation-not-writable code when available; map connection missing/exited/channel/SDK/provider/unknown failures to `session_unavailable`; never use `Display` text. Continue with the next queued item only when the fresh underlying snapshot remains `Connected`; otherwise `fail_live_session` settles the whole record and remaining queue.

The dispatcher never claims a second item until a matching `TurnComplete`/terminal monitor callback clears the active turn. A stop-requested active turn remains the broker's quarantine blocker until that matching terminal event; there is no separate `SessionState.cancellation_quarantine` field. Sender lease release/expiry never calls any queue mutation.

`settle_active_turn` maps outcomes without message parsing: an active turn whose broker `stop_requested` projection is true settles `Cancelled`; otherwise exact `stop_reason == "end_turn"` settles `Completed`; every other terminal stop reason settles `Failed`. It updates the prompt-ledger state, clears only the matching active turn, emits one `SharedTurnSettled`, and wakes the dispatcher. A duplicate/late terminal for the same or prior driver incarnation is a no-op.

Before publishing any first `ConversationLinked` for an external/ephemeral broker root, call `SharedSessionBroker::bind_conversation_key(connection_id, generation, conversation_id)`. The method locks the broker index, pointer-compares the connection's record, and atomically moves the same record from its old key to `SharedSessionKey::Conversation(conversation_id)` while preserving generation, leases, FIFO, and connection id. If the destination key points at another non-terminal record, return `ConversationKeyConflict`, fail the claimed item, and publish no `ConversationLinked`; never overwrite or merge the records. Persisted roots already keyed by that conversation are a no-op. Add this hook in both caller-supplied-row and backend-created-row branches immediately before `emit_with_state(ConversationLinked)`.

- [ ] **Step 6: Wire connection events back to the dispatcher without reverse locks**

When bootstrap registration finishes, subscribe to the connection's event broadcast before starting the route-bootstrap settler, then read one fresh runtime snapshot and reconcile it before processing the receiver. Spawn a monitor capturing both broker generation and the registered attempt's internal `connection_incarnation`. Every broker callback below validates both; a permitted same-generation fallback installs a new internal incarnation, causing the prior monitor to exit and making any buffered/late old-driver event a no-op. The initial reconcile closes the small registration-to-subscribe window for status or interaction events. The monitor handles only copied envelopes and never holds `SessionState` while calling broker methods:

```rust
match &envelope.payload {
    AcpEvent::TurnComplete { stop_reason, .. } => broker.settle_active_turn(&id, generation, &driver_incarnation, stop_reason).await,
    AcpEvent::StatusChanged { status: ConnectionStatus::Disconnected | ConnectionStatus::Error } => broker.fail_live_session(&id, generation, &driver_incarnation, "session_unavailable").await,
    AcpEvent::PermissionRequest { request_id, .. } => broker.observe_interaction(&id, generation, &driver_incarnation, SharedInteractionKind::Permission, request_id).await,
    AcpEvent::PermissionResolved { request_id } => broker.observe_interaction_resolved(&id, generation, &driver_incarnation, request_id).await,
    AcpEvent::QuestionRequest { question_id, .. } => broker.observe_interaction(&id, generation, &driver_incarnation, SharedInteractionKind::Question, question_id).await,
    AcpEvent::QuestionResolved { question_id } => broker.observe_interaction_resolved(&id, generation, &driver_incarnation, question_id).await,
    AcpEvent::PlanApprovalRequest { approval_id, .. } => broker.observe_interaction(&id, generation, &driver_incarnation, SharedInteractionKind::PlanApproval, approval_id).await,
    AcpEvent::PlanApprovalResolved { approval_id } => broker.observe_interaction_resolved(&id, generation, &driver_incarnation, approval_id).await,
    _ => Ok(Vec::new()),
}
```

Publish returned shared events only after the broker call returns. Reconcile and notify the dispatcher on continuation, delegation-count, background-count, interaction-resolved, status, and turn-terminal events, because each can change readiness. On broadcast lag, read a fresh `SessionState` snapshot with the exact three interaction ids and call `reconcile_runtime_snapshot`; do not infer queue loss from missing transient events.

The dispatcher and monitor also select on the record lifecycle watch. They exit when that exact record is failed, closing, removed, or replaced, and every such transition wakes both the lifecycle watch and `Notify`. A failed-generation pointer replacement therefore cannot leave obsolete tasks parked forever on the old record.

Before enabling the monitor, audit the existing callback sites in `acp/session_state.rs`, `acp/lifecycle.rs`, `acp/internal_bus.rs`, and manager event subscribers. The enforced adapter shape is: copy envelope/state facts under the existing lock, release it, call broker, release broker, then publish. Add `shared_monitor_lock_order_does_not_deadlock` using barriers and a 500 ms timeout to race event reconciliation, connect/attach, snapshot read, and queue admission; add an assertion/helper that no broker callback is invoked from inside `SessionState::apply_event` or while a manager connection-map guard is live.

- [ ] **Step 7: Run queue and manager dispatcher tests**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::shared_dispatch -- --nocapture`

Expected: PASS for exact FIFO order, retry identity, limits, cancel race, sender expiry preservation, dispatch failure terminal events, and stop-tail preservation fixtures.

- [ ] **Step 8: Commit Task 5**

```bash
git add src-tauri/src/acp/shared_session.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/types.rs src-tauri/src/acp/session_state.rs
git commit -m "feat: dispatch shared ACP prompts through FIFO"
```

### Task 6: Generation-Fenced Stop and First-Winner Interactions

**Files:**
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`

**Interfaces:**
- Consumes: Task 5 active-turn/interaction observation, Task 4 `SharedMutationGuard`, and existing `cancel`, `respond_permission`, `answer_question`, and `answer_plan_approval` methods.
- Produces: `SharedStopRequest`, `SharedInteractionRequest`, `claim_interaction`, `complete_interaction`, `release_interaction_claim`, `stop_shared_turn`, and shared handler guards.

- [ ] **Step 1: Write failing race tests for each interaction kind and exact-turn stop**

Add tests that race two claims and count downstream responder calls:

```rust
#[tokio::test]
async fn two_permission_answers_have_one_winner() {
    let fixture = ready_fixture_with_interaction(SharedInteractionKind::Permission, "perm-1").await;
    let (a, b) = tokio::join!(fixture.claim("perm-1"), fixture.claim("perm-1"));
    assert_eq!([a.is_ok(), b.is_ok()].into_iter().filter(|won| *won).count(), 1);
    assert!(matches!(a.err().or_else(|| b.err()), Some(SharedSessionError::InteractionAlreadyResolved)));
}

#[tokio::test]
async fn stale_turn_never_cancels_newer_turn_and_exact_stop_is_idempotent() {
    let fixture = ready_fixture_with_turn("turn-new").await;
    assert!(matches!(fixture.stop("turn-old").await, Err(SharedSessionError::StaleTurn)));
    let (a, b) = tokio::join!(fixture.stop("turn-new"), fixture.stop("turn-new"));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(fixture.cancel_call_count(), 1);
}

#[tokio::test]
async fn definite_cancel_admission_failure_releases_stop_claim_for_retry() {
    let fixture = ready_fixture_with_turn("turn-new").await;
    fixture.fail_next_cancel_before_channel_send();
    assert!(fixture.stop("turn-new").await.is_err());
    assert!(!fixture.active_turn().await.unwrap().stop_requested);
    fixture.stop("turn-new").await.unwrap();
    assert_eq!(fixture.cancel_call_count(), 2);
}
```

Repeat the first-winner assertion for question and plan approval. Add a test where stop settles the interaction and a late answer cannot claim the next turn's interaction.

The fixture installs real broker interaction/turn state and a fake downstream adapter with atomic call counts plus an explicit pre-admission failure queue. `claim` runs the same manager wrapper as production; it does not call `claim_interaction` directly. `fail_next_cancel_before_channel_send` returns the exact internal `DefinitelyNotAdmitted` classification used by the real cancel adapter, so the rollback test proves the public wrapper rather than a test-only branch.

- [ ] **Step 2: Run focused race tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests -- --nocapture`

Expected: FAIL because claims and exact stop are absent.

- [ ] **Step 3: Implement one-shot interaction CAS with rollback only on downstream failure**

Define the secret-bearing request wrappers without `Debug`:

```rust
pub struct SharedStopRequest {
    pub guard: SharedMutationGuard,
    pub turn_id: String,
}

pub struct SharedInteractionRequest<T> {
    pub guard: SharedMutationGuard,
    pub interaction_id: String,
    pub answer: T,
}
```

Store one `Pending | Resolving | Resolved` state per `SharedInteractionKind` in the session record, each with its exact id. `claim_interaction` validates the lease and exact current kind/id, changes `Pending -> Resolving`, and returns a claim token containing generation/kind/id. A second claim returns `InteractionAlreadyResolved`. On successful downstream responder admission, `complete_interaction` changes the exact claim to `Resolved`; on a send/database failure classified by the adapter as `DefinitelyNotAdmitted`, `release_interaction_claim` changes exact `Resolving -> Pending`. A `MayHaveBeenAdmitted` failure completes the claim instead, preventing a duplicate responder call. Turn settlement resolves/clears all three kinds for that turn before a later turn may install new ids.

Wrap the existing methods in `ConnectionManager::{respond_shared_permission, answer_shared_question, answer_shared_plan_approval}`. Preserve the question recovery service's existing settling rollback, but expose all loser paths as the stable shared error instead of generic protocol strings.

- [ ] **Step 4: Implement exact-turn stop and broker-owned cancellation quarantine**

`stop_shared_turn` validates guard and exact `turn_id`. Store an internal `StopAdmissionState::{Open, Resolving { result_tx }, Requested}` on the active turn; project `stop_requested = true` for both `Resolving` and `Requested`. The first caller moves `Open -> Resolving` and invokes existing `cancel`. A concurrent caller for the same turn subscribes to the resolving watch outside the record lock: it returns success only after the winner reaches `Requested`, and if the winner reports `DefinitelyNotAdmitted` it loops so exactly one caller may claim the retry. Any other turn id returns `StaleTurn`.

If the existing cancel path returns a definite pre-channel/pre-provider admission failure, call `release_stop_request(connection_id, generation, turn_id)` to CAS the same active turn back to `Open`, publish the failed resolution to waiters, and return the failure. A `MayHaveBeenAdmitted` error commits `Requested` and keeps the quarantine marker because retrying could deliver a second cancel. Once cancellation may have reached the connection loop, converge only through the terminal monitor. Do not add or read a `SessionState.cancellation_quarantine` field. Do not clear the broker active turn or dispatch the next item in the HTTP call; only the matching terminal event from the connection monitor clears it, so the projected `active_turn.stop_requested` remains the quarantine blocker until cancellation acknowledgement/finalization completes.

- [ ] **Step 5: Add shared fields to mutation handlers without changing local Tauri command signatures**

For Axum request structs add optional `generation`, `lease_id`, and, for cancel, `turn_id`. If `connection_id` is broker-managed, require all relevant fields and call the shared wrapper. If it is not broker-managed, call the existing local/delegated method. This keeps Tauri IPC and purpose-specific internal callers on their current contracts.

- [ ] **Step 6: Run interaction, stop, and legacy manager tests**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::shared_ -- --nocapture`

Expected: PASS with one downstream call per race and no stale-turn cancellation.

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::cancel_ -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::answer_ -- --nocapture`

Expected: existing local/delegated cancellation and interaction tests remain PASS.

- [ ] **Step 7: Commit Task 6**

```bash
git add src-tauri/src/acp/shared_session.rs src-tauri/src/acp/manager.rs src-tauri/src/web/handlers/acp.rs
git commit -m "feat: fence shared ACP turn and interaction control"
```

### Task 7: Idle Predicate, Cleanup, Retry, and Legacy Ownership Guards

**Files:**
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/idle_sweep.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`

**Interfaces:**
- Consumes: Task 1 idle fields, Task 4 lease expiry, Task 5 `SharedRuntimeWorkSnapshot` plus queue/turn monitor, and existing bounded disconnect/teardown.
- Produces: `SharedIdleBlockers`, `SharedHostWorkPermit`, `SharedSessionBroker::{begin_host_work,end_host_work,evaluate_idle}`, `ConnectionManager::{sweep_shared_sessions,terminate_shared_session}`, exact-generation removal CAS, cleanup tombstones, and broker-aware legacy disconnect/touch behavior.

- [ ] **Step 1: Write paused-time idle matrix and attach-versus-reap race tests**

Create one table-driven test that independently enables each blocker: lease, active turn with `stop_requested = false`, active turn with `stop_requested = true`, permission, question, plan approval, queued prompt, continuation wait, active delegation, background work, registered host work, non-ready broker phase, and non-connected ACP status. For every row, advance beyond 900 seconds and assert no reclaim, clear the blocker, advance 899 seconds and assert retained, then advance one second and assert reclaimed.

Add this race test:

```rust
#[tokio::test(start_paused = true)]
async fn attach_racing_final_reclaim_has_one_winner() {
    let fixture = idle_ready_fixture().await;
    tokio::time::advance(Duration::from_secs(900)).await;
    let (attach, reap) = tokio::join!(fixture.attach_new_lease(), fixture.reap_now());
    assert_ne!(attach.is_ok(), reap.removed);
    if attach.is_ok() { assert!(fixture.connection_still_registered().await); }
}
```

Also add `failed_tombstone_reaps_only_after_cleanup_clients_and_grace`: a failed record remains attachable while cleanup is incomplete or a lease is active; after cleanup completes and the final lease expires/releases, it remains for one client-lease TTL, then a pointer/generation CAS removes it without touching any replacement generation.

`idle_ready_fixture` owns one real broker record, a retained `SessionState` Arc, and a fake manager teardown counter. Each table row enables exactly one blocker through the production mutation/event-reconciliation method, not private-field writes. `reap_now` runs `sweep_shared_sessions` with the fixture's paused clock; `attach_new_lease` uses `reserve_or_attach`; and `connection_still_registered` checks both broker and manager indexes. Barrier channels place attach and final recheck at the same linearization point without sleeps.

- [ ] **Step 2: Run idle tests to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests::idle_ -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests::attach_racing -- --nocapture`

Expected: FAIL because the all-predicates timer and final CAS are absent.

- [ ] **Step 3: Implement authoritative idle blocker extraction and timer reset**

Reuse Task 5 `SharedRuntimeWorkSnapshot`; do not create a second idle-only projection that can drift from dispatcher readiness. Do not use `has_active_background_work(now)` because its age cap conflicts with the design; `background_outstanding > 0` remains a blocker until an explicit terminal/lost transition clears it. `host_owned_work` is read from the broker record, not copied from `SessionState`. `evaluate_idle` clears `idle_zero_since` whenever any predicate is false; when all first become true it records `Instant::now`; selection after grace returns a `(record Arc, connection id, generation)` candidate.

Store `host_owned_work: HashSet<Uuid>` in the broker record. `begin_host_work(connection_id, generation)` generation-CAS inserts one id and returns a non-cloneable `SharedHostWorkPermit` that holds a weak broker handle plus `Option<(connection_id, generation, permit_id)>`. Explicit async `end_host_work(mut permit)` takes that tuple, removes the id exactly once, and notifies the dispatcher. `Drop` takes the same tuple and schedules the identical generation-fenced removal on the current Tokio handle; this is the owning task's lost/terminal transition, not silent permission to reap live work. If the runtime is already shutting down, the record is closing through shutdown cleanup, so Drop logs only connection/generation and never a permit id. Extend Task 5 claim readiness with `host_owned_work.is_empty()`. Add tests for begin/end, duplicate end, stale-generation end, dispatcher wake, and dropped-permit release; no path may underflow or leave an unresolvable blocker.

- [ ] **Step 4: Implement final generation/predicate CAS and bounded process cleanup**

For every candidate, lock the same record first, then acquire/read the exact retained public `SessionState` under the documented record -> `SessionState` order. While both guards are held, recheck generation, zero leases, phase, queue, broker active turn (including `stop_requested`), interactions, host permits, and every copied runtime-work fact; perform no DB/process/channel/WebSocket operation in this section. If attach acquired the record first and added a lease, this recheck fails, increments `idle_cas_lost_total`, clears the stale candidate, and leaves the process alive. If the recheck wins, transition `Ready -> Closing` before releasing either guard, then drop both. This closes the stale-snapshot window between final work validation and the phase fence.

Perform bounded manager teardown and wait until the exact `connection_id` is absent from the manager connection map. An attach that linearized before `Closing` wins and prevents cleanup; an attach that arrives during `Closing` receives stable `shared_session_closing`. If teardown times out or map absence cannot be proved, retain the indexed `Closing` record with `cleanup_complete = false`, increment `cleanup_incomplete_total`, and do not permit a retry/new incarnation. Only after process/driver teardown and map absence are observed may cleanup lock the broker index, compare the same record pointer and generation, and atomically remove key plus connection indexes. Thus a new incarnation cannot reserve until the old process is gone.

Bootstrap/closing records never enter the Ready idle timer. Failure immediately fails queued items, broadcasts terminal state, runs `teardown_unexposed_attempt`, and sets `cleanup_complete` only after driver/process termination and map absence. A Failed record remains indexed while cleanup is incomplete or any lease is active. After cleanup is complete and the final lease is absent, start `failed_zero_since`; any new lease clears it. After one configured client-lease TTL, the sweep map-locks, pointer/generation-CAS removes only that failed record, and wakes its lifecycle watch. Explicit retry remains generation-fenced and may replace earlier once cleanup is complete; the failed-tombstone sweep must lose its pointer CAS to that replacement. This gives concurrent clients a bounded failure-observation window without accumulating failed records forever.

- [ ] **Step 5: Change configuration defaults and integrate both sweep classes**

Set `DEFAULT_IDLE_TIMEOUT_SECS = 900`, add `DEFAULT_CLIENT_LEASE_TTL_SECS = 90`, parse `CODEG_ACP_CLIENT_LEASE_TTL_SECS`, and reject/log a lease TTL greater than or equal to enabled idle grace by falling back to 90. The periodic task first expires leases, then calls `sweep_shared_sessions`, then calls legacy `sweep_idle` only for non-broker connection ids.

- [ ] **Step 6: Fence legacy browser disconnect and touch**

Before `disconnect_if_owner` or `touch`, check `shared_session_broker().is_managed_connection`. For a shared root, legacy disconnect returns `SharedSessionError::ProtocolRequired` unless invoked by internal application shutdown/explicit shared termination; legacy touch returns `false` and does not change idle state. Lease release is the only page-teardown path.

Implement `terminate_shared_session(connection_id, generation)` as the explicit operator path: generation-CAS `Ready | Bootstrapping | Failed -> Closing`, reject `Reserved`/stale generation, fail queued items with `session_unavailable`, settle the active turn exactly once, publish phase/terminal events, perform the same bounded teardown/map-absence wait as idle cleanup, then pointer/generation-CAS remove broker indexes. It is never called from lease release, socket close, provider unmount, idle UI cleanup, or tab replacement.

- [ ] **Step 7: Run idle, cleanup, and existing ownership suites**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests::idle_ -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::shared_session::tests::attach_racing -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::sweep_idle -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::idle_sweep::tests -- --nocapture`

Expected: PASS for every blocker and full fresh grace after long work.

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --lib acp::manager::tests::disconnect_if_owner -- --nocapture`

Expected: existing Tauri/pop-out ownership CAS tests remain PASS.

- [ ] **Step 8: Commit Task 7**

```bash
git add src-tauri/src/acp/shared_session.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/idle_sweep.rs src-tauri/src/web/handlers/acp.rs
git commit -m "feat: reclaim only idle shared ACP sessions"
```

### Task 8: Axum Shared-Session HTTP Contracts

**Files:**
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/commands/delegate_access.rs`
- Create: `src-tauri/tests/shared_session_http.rs`

**Interfaces:**
- Consumes: Tasks 2-7 manager methods and stable errors, existing route/terminal launch resolution, DB conversation entities, and bearer-token middleware.
- Produces: `/acp_connect_or_attach`, `/acp_release_lease`, `/acp_cancel_queued_prompt`, `/acp_terminate_shared_session`, broker-aware `/acp_prompt`, stop/interaction requests, and authenticated integration coverage.

- [ ] **Step 1: Write failing HTTP tests for atomic connect, validation, fast bootstrap, enqueue/cancel, and protocol-required compatibility**

Build the test router exactly like `src-tauri/tests/ws_attach.rs`, install a fake shared spawn driver, create a persisted conversation/folder, then assert:

```rust
#[tokio::test]
async fn concurrent_connect_or_attach_returns_one_connection_and_distinct_leases() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let (a, b) = tokio::join!(
        fixture.post_connect("device-a", "client-a", "request-a"),
        fixture.post_connect("device-b", "client-b", "request-b"),
    );
    let a = a.assert_status_ok().json::<AcpConnectOrAttachResponse>();
    let b = b.assert_status_ok().json::<AcpConnectOrAttachResponse>();
    assert_eq!(a.connection_id, b.connection_id);
    assert_eq!(a.generation, b.generation);
    assert_ne!(a.lease_id, b.lease_id);
    assert_eq!(fixture.spawn_count(), 1);
    assert_eq!(a.phase, SharedPublicPhase::Bootstrapping);
}

#[tokio::test]
async fn legacy_prompt_and_disconnect_cannot_mutate_shared_root() {
    let fixture = ready_shared_http_fixture().await;
    let attached = fixture.post_connect("d", "c", "r").await.json::<AcpConnectOrAttachResponse>();
    fixture.post_json("/acp_prompt", json!({ "connectionId": attached.connection_id, "blocks": [{"type":"text","text":"x"}] }))
        .await.assert_status_conflict().assert_json_contains(json!({"code":"shared_session_protocol_required"}));
    fixture.post_json("/acp_disconnect", json!({ "connectionId": attached.connection_id, "origin":"provider_unmount" }))
        .await.assert_status_conflict();
    assert!(fixture.manager().get_state(&attached.connection_id).await.is_some());
}
```

Also assert an invalid conversation/agent/folder combination returns 400 without reserving/spawning; labels outside `[A-Za-z0-9._:-]{1,128}` return 400; an identical persisted, external, or ephemeral connect `requestId` returns the same connection/lease; bounded lease/connect/prompt ledgers return their stable 429 codes only for new identities; missing lease returns 409; recent expired lease returns 410; bearer authentication alone returns 401; direct legacy web `/acp_connect` root creation returns `shared_session_protocol_required` without spawning; release never disconnects; explicit termination requires auth/current generation and completes bounded cleanup; queue cancel is allowed from the other lease; and required companion failure returns phase `failed` with secret-safe error fields.

`shared_http_fixture_with_pending_bootstrap` is a real `AppState`/router fixture using the Task 2 injected spawn driver and a temporary SQLite database. `post_connect` sends the exact camel-case request and includes the fixture bearer token; `assert_status_*` reads and retains the body once; `ready_shared_http_fixture` resolves both registration and bootstrap through channels. Capacity cases use broker test limits but separately assert the production constants. The fixture never starts a real agent process or reads environment/keyring state.

- [ ] **Step 2: Run the integration target to verify RED**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --test shared_session_http -- --nocapture`

Expected: FAIL because the routes and wire contracts do not exist.

- [ ] **Step 3: Add connect request/response structs and persisted conversation validation**

Use the design's camel-case request exactly:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConnectOrAttachRequest {
    pub conversation_id: Option<i32>,
    pub agent_type: AgentType,
    pub working_dir: Option<String>,
    pub external_session_id: Option<String>,
    pub delegation_route_override: Option<DelegationRoutePolicy>,
    pub preferred_mode_id: Option<String>,
    #[serde(default)]
    pub preferred_config_values: BTreeMap<String, String>,
    pub device_id: String,
    pub client_instance_id: String,
    pub request_id: String,
    pub retry_failed_generation: Option<u64>,
}
```

For positive `conversation_id`, load the row and its folder before building launch inputs. Require persisted `agent_type == request.agent_type`, normalized folder path equals requested working directory when supplied, and persisted external id agrees with supplied external id when both exist. Build `SharedSessionKey::Conversation(id)` and freeze `launch_inputs.route_plan.fingerprint` plus `terminal_shell_selection_key(&launch_inputs.terminal_settings)` into `SharedLaunchIdentity`; Task 2 verifies these values against its one `finalize_acp_launch_config` result before spawn. For drafts without a persisted row, use `ExternalSession` only when all identity fields exist. Otherwise call `SharedSessionBroker::ephemeral_key(device_id, client_instance_id, request_id)`, which hashes a process-random startup nonce plus length-delimited validated labels with SHA-256 and returns `Ephemeral(hex_digest)`. An exact transport retry in the same process therefore reaches the same record/lease, while another client/device cannot discover it and restart yields a different key. Never place raw client labels in the key or logs.

When the handler constructs `SharedConnectLaunch`, set
`database: state.db.conn.clone()`. Pass the SeaORM `DatabaseConnection` handle, not
an `AppDatabase` clone; Task 5's created-record dispatcher wraps that handle in its
own `AppDatabase` value for `send_prompt_linked_with_message_id`.

- [ ] **Step 4: Add all shared routes and wire responses**

Register:

```text
POST /acp_connect_or_attach
POST /acp_release_lease
POST /acp_cancel_queued_prompt
POST /acp_terminate_shared_session
```

Extend the shared forms of `/acp_prompt`, `/acp_cancel`, `/acp_respond_permission`, `/acp_answer_question`, and `/acp_answer_plan_approval`. `AcpConnectOrAttachResponse` contains `connection_id`, `generation`, `lease_id`, RFC3339 `lease_expires_at`, `disposition`, public phase, current `event_seq`, and an optional `{ code, retryable, cleanup_complete }` without raw detail.

Use a separate string-valued `SharedPublicPhase::{Bootstrapping, Ready, Failed, Closing}` with snake-case serde for the response; internal `Reserved` is unrepresentable. Snapshot/events continue to use Task 3's tagged `SharedSessionPhase` object. The response error is required exactly when public phase is `Failed`.

Derive `Serialize` with `#[serde(rename_all = "camelCase")]` on the response and its failure object. Use `#[serde(untagged)] enum AcpPromptResponse { Shared(PromptEnqueueResult), Legacy(()) }`, where unit serializes as JSON null; do not wrap either shape in an extra enum tag.

The Axum `/acp_connect` handler must reject ordinary web `ConnectionPurpose::User` root creation with `ProtocolRequired`; it may not call `spawn_agent` as a compatibility fallback. Pure Tauri uses the IPC command, and purpose-specific internal/delegated producers do not enter through this public web root handler. Keep `/acp_find_connection_for_conversation` read-only.

`/acp_prompt` returns camel-case `Json<PromptEnqueueResult>` for shared roots and `Json<null>` for legacy callers through a serde-compatible response enum; the frontend normalizes legacy null to immediate dispatch. The shared handler constructs the result only through Task 5 `finalize_enqueue_response`, so a dispatcher claim that wins before serialization returns `state: "dispatching"`, otherwise `state: "queued"`. Admission runs existing delegate/workflow/block/hydration validation before broker insertion, and no rejection creates an item. A conversation-keyed record requires the request conversation id to equal its canonical key; an external/ephemeral record may have no conversation or bind exactly once through Task 5. A caller cannot use prompt fields to retarget a persisted shared root.

For `AcpConnectOrAttachResponse.error`, `code` is the stable broker code, `cleanup_complete` mirrors the failed phase, and `retryable` is true only when cleanup is complete and the code is `companion_initialization_failed` or `session_unavailable`. It is false while cleanup is incomplete and for configuration/validation/fencing failures. No `Display` string, path, stderr, or launch detail enters this object.

- [ ] **Step 5: Keep internal and local producers outside the shared root path**

Do not change Tauri `acp_connect`, automation enqueue, chat-channel authoring, delegated spawner, probes, or title/translation calls. Add `ensure_web_shared_or_delegate_interactive(db, manager, connection_id, conversation_id)`: for a broker-managed connection it verifies the record launch purpose is exactly `ConnectionPurpose::User` and that any canonical conversation id matches, then returns the root path; for a non-broker id it delegates unchanged to `ensure_effective_delegate_interactive`. Use this helper only in Axum mutation handlers. Do not route delegated child ids into the root FIFO.

- [ ] **Step 6: Run HTTP, server, and router tests**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --test shared_session_http -- --nocapture`

Expected: PASS for atomic reservation, separate leases, prompt/cancel, release, validation, compatibility, and typed failure.

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --bin codeg-server --lib web:: -- --nocapture`

Expected: PASS and server-only feature compilation contains every new route.

- [ ] **Step 7: Commit Task 8**

```bash
git add src-tauri/src/web/handlers/acp.rs src-tauri/src/web/router.rs src-tauri/src/commands/delegate_access.rs src-tauri/tests/shared_session_http.rs
git commit -m "feat: expose shared ACP session API"
```

### Task 9: Frontend Shared Contracts, Identity, and Lease-Bound Transport

**Files:**
- Create: `src/lib/acp/shared-session-client.ts`
- Create: `src/lib/acp/shared-session-client.test.ts`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/lib/transport/types.ts`
- Modify: `src/lib/transport/web-event-stream.ts`
- Modify: `src/lib/transport/web-event-stream.test.ts`
- Modify: `src/lib/transport/remote-desktop-transport.ts`
- Create: `src/lib/transport/remote-desktop-transport.test.ts`

**Interfaces:**
- Consumes: Task 8 camel-case HTTP contracts and Task 4 WebSocket attach fields.
- Produces: `getSharedClientIdentity`, `AcpConnectOrAttachResponse`, `SharedMutationContext`, shared API wrappers, `AttachOptions.shared`, automatic generation/lease replay on WebSocket reconnect, and one 30-second app-level ping timer per `WebEventStream` while shared subscriptions exist.

- [ ] **Step 1: Write failing identity and transport tests**

Add tests proving device id persists while each module-reset/document load gets a different client instance id, and WebEventStream sends exact fencing fields on first attach and reconnect:

```ts
it("reattaches with the same generation and lease", () => {
  const f = hostFixture()
  const stream = new WebEventStream(f.host)
  stream.attach("conn", {
    shared: { generation: 4, leaseId: "lease-4" },
  }, handlers)
  expect(f.sendFrame).toHaveBeenLastCalledWith(expect.objectContaining({
    action: "attach",
    connection_id: "conn",
    generation: 4,
    lease_id: "lease-4",
  }))
  f.sendFrame.mockClear()
  f.reconnect()
  expect(f.sendFrame).toHaveBeenCalledWith(expect.objectContaining({ generation: 4, lease_id: "lease-4" }))
})

it("pings every 30 seconds only while a shared subscription exists", () => {
  vi.useFakeTimers()
  const f = hostFixture()
  const stream = new WebEventStream(f.host)
  const first = stream.attach("conn", {
    shared: { generation: 4, leaseId: "lease-4" },
  }, handlers)
  f.sendFrame.mockClear()
  vi.advanceTimersByTime(29_999)
  expect(f.sendFrame).not.toHaveBeenCalledWith({ action: "ping" })
  vi.advanceTimersByTime(1)
  expect(f.sendFrame).toHaveBeenCalledWith({ action: "ping" })
  f.sendFrame.mockClear()
  first.detach()
  vi.advanceTimersByTime(60_000)
  expect(f.sendFrame).not.toHaveBeenCalledWith({ action: "ping" })
  stream.destroy()
  vi.useRealTimers()
})
```

Add a second fake-timer case with two shared subscriptions: detaching one keeps exactly one timer alive, detaching the final one cancels it, `destroy()` cancels it, reconnect does not create a duplicate timer, and legacy-only subscriptions never start it. Run the same transport contract through the remote-desktop fixture so both browser and proxied desktop modes emit `{ action: "ping" }`.

- [ ] **Step 2: Run frontend tests to verify RED**

Run: `pnpm test -- src/lib/acp/shared-session-client.test.ts src/lib/transport/web-event-stream.test.ts`

Expected: FAIL because identity and shared attach options are absent.

- [ ] **Step 3: Implement bounded device/document identity**

Use local storage key `codeg.sharedSession.deviceId.v1`; generate UUIDs with the existing `randomUUID`. Keep `clientInstanceId` in a module-level variable only. Export:

```ts
export interface SharedClientIdentity {
  deviceId: string
  clientInstanceId: string
}

export function getSharedClientIdentity(): SharedClientIdentity
export function newSharedRequestId(): string
```

Catch storage security/quota errors and use an in-memory UUID; never use these labels as authentication or paths.

- [ ] **Step 4: Add typed shared API wrappers**

Implement `acpConnectOrAttach`, `acpReleaseLease`, `acpCancelQueuedPrompt`, and shared arguments for prompt/stop/interaction methods in `api.ts`. Preserve `tauri.ts` legacy signatures because pure Tauri does not enter shared ownership. The shared prompt wrapper must return `PromptEnqueueResult`, preserve uploaded-image stripping, and never translate queueing to `TurnBusyError`.

Use:

```ts
export interface SharedMutationContext {
  generation: number
  leaseId: string
}

export interface SharedPromptAdmission extends SharedMutationContext {
  clientInstanceId: string
  clientRequestId: string
}

export type SharedPublicPhase =
  | "bootstrapping"
  | "ready"
  | "failed"
  | "closing"

export type PromptEnqueueResult = {
  queueItemId: string
  enqueueSeq: number
  state: "queued" | "dispatching"
}
```

Add `AcpConnectOrAttachResponse` with `phase: SharedPublicPhase`, required lease/generation/disposition/eventSeq fields, and optional secret-safe failure. Keep current positional APIs source-compatible by appending optional fencing arguments:

```ts
acpReleaseLease(connectionId, generation, leaseId): Promise<void>
acpCancelQueuedPrompt(connectionId, queueItemId, shared): Promise<void>
acpTerminateSharedSession(connectionId, generation): Promise<void>
acpPrompt(connectionId, blocks, folderId, conversationId, clientMessageId, context, shared?): Promise<PromptEnqueueResult | null>
acpCancel(connectionId, shared?: SharedMutationContext & { turnId: string }): Promise<void>
acpRespondPermission(connectionId, requestId, optionId, shared?: SharedMutationContext): Promise<void>
acpAnswerQuestion(connectionId, questionId, answer, shared?: SharedMutationContext): Promise<void>
acpAnswerPlanApproval(connectionId, approvalId, answer, shared?: SharedMutationContext): Promise<void>
```

The shared `acpPrompt` argument is `SharedPromptAdmission`; serialize its client instance/request ids plus generation/lease. The legacy call omits it and normalizes the server's JSON null to `null`. Add `api.test.ts` assertions for every exact camel-case payload, result normalization, image stripping, and the rule that only legacy turn-in-progress becomes `TurnBusyError` while shared queue admission errors stay typed.

- [ ] **Step 5: Extend `EventStream` attach options, reconnect behavior, and app-level heartbeat**

Add `shared?: { generation: number; leaseId: string }` to `AttachOptions`, store it in `ActiveSub`, and serialize it on every attach. Add detach reasons `generation_stale`, `lease_missing`, `lease_expired`, and `session_replaced`. A lease-expired detach is terminal for that subscription; `AcpConnectionsProvider` will obtain a fresh lease through connect-or-attach instead of replaying the expired lease.

Ensure `RemoteDesktopTransport` forwards the updated attach frame unchanged through its proxy WebSocket path.

`WebEventStream` owns one nullable interval handle. After local attach/detach, server-originated `detached`, and destroy bookkeeping, call `syncSharedHeartbeat()`: start `setInterval(() => { if (host.isWsOpen()) host.sendFrame({ action: "ping" }) }, 30_000)` when at least one active subscription has `shared`, and clear it when none remain or the stream is destroyed. Each tick sends exactly `{ action: "ping" }` only through an open host socket; reconnect reuses the existing interval and must not multiply timers. This is the application heartbeat consumed by Task 4 `ClientMsg::Ping`, not a browser WebSocket protocol ping. `RemoteDesktopTransport` must preserve it through the same proxy path.

- [ ] **Step 6: Run identity, API, transport, and type checks**

Run: `pnpm test -- src/lib/acp/shared-session-client.test.ts src/lib/api.test.ts src/lib/transport/web-event-stream.test.ts src/lib/transport/remote-desktop-transport.test.ts`

Expected: PASS with exact lease/generation fields on reconnect.

Run: `pnpm eslint src/lib/acp/shared-session-client.ts src/lib/api.ts src/lib/tauri.ts src/lib/transport/types.ts src/lib/transport/web-event-stream.ts src/lib/transport/remote-desktop-transport.ts`

Expected: PASS under strict TypeScript/Prettier rules.

- [ ] **Step 7: Commit Task 9**

```bash
git add src/lib/acp/shared-session-client.ts src/lib/acp/shared-session-client.test.ts src/lib/types.ts src/lib/api.ts src/lib/api.test.ts src/lib/tauri.ts src/lib/transport/types.ts src/lib/transport/web-event-stream.ts src/lib/transport/web-event-stream.test.ts src/lib/transport/remote-desktop-transport.ts src/lib/transport/remote-desktop-transport.test.ts
git commit -m "feat: add shared ACP client transport contracts"
```

### Task 10: Provider Migration to Shared Server Ownership

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/hooks/use-connection.ts`
- Modify: `src/hooks/use-connection.test.tsx`
- Modify: `src/hooks/use-connection-lifecycle.ts`
- Modify: `src/hooks/use-connection-lifecycle.test.ts`
- Modify: `src/hooks/use-connection-lifecycle.send-failure.test.ts`

**Interfaces:**
- Consumes: Task 9 shared API/attach contracts and Task 3 `SnapshotPatch.sharedSession`.
- Produces: `ConnectionState.sharedSession`, shared connect/reconnect/release lifecycle, fenced actions, server-root paths with no discovery-before-create or owner disconnect, and a send promise that resolves only after authoritative queue admission.

- [ ] **Step 1: Write failing provider tests for shared connect, release-only teardown, lease refresh, fenced actions, and unchanged desktop ownership**

Add/replace focused tests:

```ts
it("server roots call connect-or-attach directly and install shared state", async () => {
  h.eventStreamEnabled = true
  h.acpConnectOrAttach.mockResolvedValue(sharedResponse({ disposition: "attached" }))
  await mountProvider()
  await act(() => h.actions!.connect(TAB, "codex", "/work", "sess", 42))
  expect(h.acpFindConnectionForConversation).not.toHaveBeenCalled()
  expect(h.acpConnect).not.toHaveBeenCalled()
  expect(h.acpConnectOrAttach).toHaveBeenCalledTimes(1)
  expect(h.connection().sharedSession).toMatchObject({ generation: 1, leaseId: "lease-1" })
})

it("shared teardown releases lease and never disconnects process", async () => {
  await mountReadySharedProvider()
  await act(() => h.actions!.disconnect(TAB, "provider_unmount"))
  expect(h.acpReleaseLease).toHaveBeenCalledWith("conn", 1, "lease-1")
  expect(h.acpDisconnect).not.toHaveBeenCalled()
})
```

Also test created/attached dispositions produce identical state; bootstrapping attaches immediately; lease-expired detach reconnects to the same conversation for a fresh lease while preserving the last event cursor; generation/session-replaced detach reconnects and drops the cursor if generation changes; an explicit retry from `{ phase: "failed", cleanupComplete: true }` sends `retryFailedGeneration` and installs the returned new id/generation/lease; cleanup-incomplete failure does not start a retry; shared prompt/cancel/three interactions carry the exact guard/turn id; local Tauri owner, viewer, pop-out, and delegated-child tests keep using the old path.

Add a distinct explicit-user case asserting `acpTerminateSharedSession("conn", 1)` is called and `acpDisconnect` is not; provider-unmount/tab-replacement/idle cleanup cases must assert termination is not called.

- [ ] **Step 2: Run focused provider tests to verify RED**

Run: `pnpm test -- src/contexts/acp-connections-context.test.tsx src/hooks/use-connection-lifecycle.test.ts`

Expected: FAIL because shared state/actions are missing and discovery still runs.

- [ ] **Step 3: Add shared connection state and snapshot/event reducer actions**

Extend `ConnectionState`:

```ts
export interface SharedConnectionState {
  generation: number
  leaseId: string
  leaseExpiresAt: string
  connectRequestId: string
  phase: SharedSessionPhaseView
  queue: SharedQueuedPrompt[]
  activeTurn: SharedActiveTurn | null
}

sharedSession: SharedConnectionState | null
```

`CONNECTION_CREATED` receives the connect response plus the locally generated request id and initializes shared state before attach. `connectRequestId` is client-local and never serialized into snapshot/events/logs. `HYDRATE_FROM_SNAPSHOT` replaces phase/queue/turn from the authoritative snapshot while preserving local lease and connect-request ids when the snapshot projection omits them. Add reducer actions for every Task 3 shared event and ignore mismatched generations.

Convert the response's `SharedPublicPhase` string to `SharedSessionPhaseView` in one helper: bootstrapping/ready/closing become `{ phase }`, while failed requires the response error and becomes `{ phase: "failed", errorCode, cleanupComplete }`. Snapshot/event wire `SharedSessionPhase` is also mapped explicitly into this view. Never compare a response string directly with the tagged snapshot/event union.

- [ ] **Step 4: Replace server root discovery/owner flow with connect-or-attach**

Treat `getEventStream() !== null` as shared-server capability; this includes browser and remote-desktop transports but excludes pure Tauri. For every initial server `own_or_observe` user-root connect, call `acpConnectOrAttach` directly with stable device/document identity and one new request id: persisted roots pass their positive conversation id, resumable pre-link roots pass external session plus working directory, and other drafts use the server's idempotent ephemeral-key path. Retain that request id in shared client state. Created and attached use one code path, both attach the WebSocket immediately with `{ shared: { generation, leaseId } }`, and both set `isViewer = false` because lifecycle ownership is server-side.

Keep `observe_existing` for delegated-child viewing and all local Tauri owner/viewer/handoff branches. Do not remove `acpFindConnectionForConversation`; it remains diagnostic and legacy desktop behavior, not a server-root creation decision.

- [ ] **Step 5: Make teardown release-only and fence all shared mutations**

For a shared root, idle UI cleanup, provider unmount, socket loss, and tab replacement detach the subscription then best-effort `acpReleaseLease`; they never call `acpDisconnect`. The existing explicit-user disconnect affordance instead calls `acpTerminateSharedSession(connectionId, generation)` after detaching/releasing its lease; no lifecycle/unmount reason may reach that route. Disable frontend `acpTouchConnection` and one-minute idle disconnect for shared roots; 30-second WebSocket ping maintains leases. On lease-expired, generation-stale, or session-replaced detach, enter the provider's existing per-tab connect single-flight and reconnect with the retained `connectRequestId`, so duplicate detach/reconnect signals issue at most one HTTP call and persisted, external, and same-document ephemeral records all reach the broker key again. Preserve the current `eventSeq` as `sinceSeq` only when the response generation is unchanged; omit it for a new generation and require a cold snapshot. A full document reload can recover persisted/external identity, while an unpersisted ephemeral draft remains intentionally undiscoverable.

Route prompt, queued-item cancel, exact-turn stop, permission, question, and plan approval through shared APIs using `conn.sharedSession`. On `interaction_already_resolved` or `stale_turn`, request a cold snapshot/reattach and treat it as normal convergence without an error toast.

Wire the existing explicit reconnect/retry affordance for a shared Failed phase to detach the old subscription, best-effort release its lease, then call `acpConnectOrAttach` with `retryFailedGeneration: current.generation` only when `cleanupComplete` is true. Use a new request id and cold-attach the returned incarnation. Automatic lease refresh, socket reconnect, provider unmount, and ordinary duplicate connect never set this field. If cleanup is incomplete, keep the failed state and surface the existing retry-disabled/loading behavior rather than polling or bypassing the cleanup fence.

- [ ] **Step 6: Return an awaited queue-admission promise from provider/lifecycle send**

Change `AcpActionsValue.sendPrompt`, `UseConnectionReturn.sendPrompt`, and `useConnectionLifecycle.handleSend` to return `Promise<PromptEnqueueResult | null>`, where null means the legacy immediate path. The shared promise resolves only after `/acp_prompt` has accepted/frozen the Task 5 admission result; it rejects on validation, capacity, lease, or transport failure. `useConnectionLifecycle.handleSend` accepts `onPromptAdmitted?: (result: PromptEnqueueResult | null) => void`, invokes it only after the API resolves, and returns the same result. After running existing failure callbacks it must rethrow shared admission failure so an awaited composer retains its draft; local callers keep their existing fire-and-forget usage and immediate-clear UI behavior.

- [ ] **Step 7: Run provider, lifecycle, and desktop compatibility tests**

Run: `pnpm test -- src/contexts/acp-connections-context.test.tsx src/hooks/use-connection.test.tsx src/hooks/use-connection-lifecycle.test.ts src/hooks/use-connection-lifecycle.send-failure.test.ts src/lib/conversation-popout-acp-bridge.test.ts`

Expected: PASS; server roots have no discovery/disconnect/touch ownership, while Tauri/pop-out/delegation tests remain unchanged.

- [ ] **Step 8: Commit Task 10**

```bash
git add src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-connection.ts src/hooks/use-connection.test.tsx src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts src/hooks/use-connection-lifecycle.send-failure.test.ts
git commit -m "feat: migrate server ACP roots to shared ownership"
```

### Task 11: Authoritative Queue UI and Exact Dispatch Reconciliation

**Files:**
- Create: `src/components/chat/shared-message-queue-display.tsx`
- Create: `src/components/chat/shared-message-queue-display.test.tsx`
- Modify: `src/components/chat/chat-input.tsx`
- Modify: `src/components/chat/message-input.tsx`
- Modify: `src/components/chat/message-input.test.tsx`
- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Modify: `src/components/conversations/conversation-session-surface.test.tsx`
- Modify: `src/contexts/acp-connections-context.tsx`

**Interfaces:**
- Consumes: Task 10 `conn.sharedSession.queue/activeTurn`, `onPromptAdmitted`, queue cancel action, existing optimistic turn ids, and Task 3 `PromptDispatchStarted`/`UserMessage` events.
- Produces: read-only server FIFO display with cancel buttons, no edit/reorder, an awaited-admission `MessageInput` mode that retains drafts until success, and exact message-id transcript reconciliation.

- [ ] **Step 1: Write failing component/surface tests for queue order, no edit/reorder, cancel, admission rollback, and cross-client dispatch**

Add component tests asserting rows render by `enqueueSeq`, only queued rows have an `X` cancel icon, no drag/edit controls exist, cancel receives `queueItemId`, duplicate cancel clicks are suppressed while its promise is pending, and a failed cancel re-enables the row without removing it. Add surface tests:

```ts
it("removes optimistic history when admission is queued and keeps authoritative queue", async () => {
  await mountSharedSurface({ promptResult: { queueItemId: "q2", enqueueSeq: 2, state: "queued" } })
  await sendDraft("later")
  expect(runtimeOptimisticTurns()).toHaveLength(0)
  expect(screen.getByText("later")).toBeInTheDocument()
})

it("dispatch-start plus user-message restores exact queued message once", async () => {
  await mountSharedSurfaceWithQueue([{ queueItemId: "q2", enqueueSeq: 2, clientMessageId: "m2", visibleText: "later" }])
  emitShared({ type: "prompt_dispatch_started", generation: 1, turn: turn("q2", "m2") })
  emitAcp({ type: "user_message", message_id: "m2", blocks: [{ type: "text", text: "later" }] })
  expect(runtimeUserTurns().filter((turn) => turn.id === "m2")).toHaveLength(1)
  expect(within(screen.getByTestId("shared-message-queue")).queryByText(/^later$/)).not.toBeInTheDocument()
})
```

Add `src/components/chat/message-input.test.tsx` cases using a deferred `onSend` promise. With `sendClearMode="after-admission"`, assert the editor/draft remains populated and editing/attachment controls are disabled while pending, a second Enter/click does not call `onSend` again, a resolved promise clears editor/attachments/storage exactly once, and a rejected promise re-enables editing while retaining all draft content without an unhandled rejection. With the default `sendClearMode="immediate"`, assert existing Tauri behavior still clears synchronously. Add shared-prompting and first-message/new-conversation surface assertions that local `onEnqueue` is not called, DB creation plus backend admission are awaited as one promise, and a rejected admission retains the composer draft.

Extend the existing conversation-surface harness rather than inventing a second renderer: `mountSharedSurface` installs a `ConnectionState` with generation 1/lease and a mocked lifecycle promise; `mountSharedSurfaceWithQueue` hydrates the supplied authoritative queue; `sendDraft` drives the real composer; `emitShared` and `emitAcp` dispatch through the provider reducer; and `runtimeOptimisticTurns`/`runtimeUserTurns` read the existing runtime adapter store. `turn(q, m)` returns a Task 3 `SharedActiveTurn` with deterministic ids. The helpers do not mutate rendered component state directly.

- [ ] **Step 2: Run component/surface tests to verify RED**

Run: `pnpm test -- src/components/chat/shared-message-queue-display.test.tsx src/components/chat/message-input.test.tsx src/components/conversations/conversation-session-surface.test.tsx`

Expected: FAIL because the authoritative queue component/reconciliation do not exist.

- [ ] **Step 3: Build a compact authoritative queue component**

Use the existing visual density and `X` Lucide icon. Props are exact:

```ts
interface SharedMessageQueueDisplayProps {
  queue: SharedQueuedPrompt[]
  onCancel: (queueItemId: string) => Promise<void>
}
```

Sort by `enqueueSeq`, render `#<enqueueSeq>` and bounded `visibleText`; for attachment-only items render the Lucide `Paperclip` icon plus `attachmentCount`. Put `data-testid="shared-message-queue"` on the unframed list container so tests can distinguish queue text from the transcript. Use the existing `messageQueue.deleteItem` translation as the cancel button aria-label/tooltip, line-clamp text so it cannot resize the fixed row, and keep a per-item pending-cancel set that disables duplicate clicks until the promise settles. Expose no edit/reorder control or explanatory feature copy.

- [ ] **Step 4: Select local versus shared queue in the conversation surface**

Keep `useMessageQueue` and `MessageQueueDisplay` unchanged for local Tauri and delegated legacy paths. When `conn.sharedSession` exists, bypass local direct-send queueing/auto-flush/bounce logic, including the current prompting-mode `onEnqueue` branch, and submit every valid draft through the provider to the backend FIFO. Pass `onEnqueue={undefined}` for shared sessions even while prompting, pass `conn.sharedSession.queue` to `SharedMessageQueueDisplay`, and wire its cancel callback to `acpCancelQueuedPrompt` through the provider action.

Add to `MessageInputProps`:

```ts
onSend: (draft: PromptDraft, modeId?: string | null) => void | Promise<unknown>
sendClearMode?: "immediate" | "after-admission"
```

Default to `immediate`. In `after-admission`, set a local `sendAdmissionPending` latch before invoking `onSend`, mark the submit control busy/disabled, make editor and attachment mutation controls temporarily read-only/disabled, block keyboard/button duplicates while set, await the returned promise inside `try/catch`, and only then clear editor, attachments, and persisted draft. This prevents an edit made during a slow first-conversation/admission request from being erased by the older request's success. On rejection, clear only the pending latch, re-enable editing, and retain the draft; the lifecycle already reports the error, so consume the rejection in the input event handler. Do not apply this mode to queue-edit, fork-send, steering, or local Tauri paths. The conversation surface selects `after-admission` exactly when `conn.sharedSession` exists and returns the Task 10 lifecycle promise from its send callback. Refactor its persisted and create-first-conversation branches so the same promise awaits conversation creation and `lifecycleSend`; do not launch an untracked async IIFE in shared mode.

- [ ] **Step 5: Reconcile optimistic turns at the admission and dispatch boundaries**

The surface may append its existing optimistic turn before the network call, but the shared composer remains populated and submit-disabled until admission resolves. In `onPromptAdmitted`:

- legacy/null or shared `dispatching`: keep the optimistic turn;
- shared `queued`: remove the optimistic turn, set runtime sync back to idle, and rely on authoritative queue projection;
- rejection: rollback the optimistic turn and runtime state, but do not issue draft-restore because `MessageInput` has retained the original shared draft; legacy immediate-clear rejection restoration remains unchanged.

On `PromptDispatchStarted`, remove the queue item from connection state and record `client_message_id` as the expected shared user turn. On the following exact `UserMessage`, mirror the user turn for every shared client, including the submitting client; use message id dedup so a retained immediate optimistic turn and the event converge to one turn. Never reconcile by text or timestamp.

- [ ] **Step 6: Ensure stop/cancel leaves the queued tail visible**

On `SharedTurnSettled(cancelled)`, clear only the matching active turn. Do not clear the queue. The next `PromptDispatchStarted` removes only its exact queue item. Add an assertion that queue `[q2, q3]` remains after stopping q1 and q2 begins only after q1 settled.

- [ ] **Step 7: Run UI, provider, and queue regressions**

Run: `pnpm test -- src/components/chat/shared-message-queue-display.test.tsx src/components/chat/message-input.test.tsx src/components/chat/message-queue-display.test.tsx src/components/conversations/conversation-session-surface.test.tsx src/contexts/acp-connections-context.test.tsx src/hooks/use-message-queue.test.ts`

Expected: PASS for shared authoritative FIFO and unchanged local editable/reorder queue.

Run: `pnpm eslint src/components/chat/shared-message-queue-display.tsx src/components/chat/chat-input.tsx src/components/chat/message-input.tsx src/components/conversations/conversation-session-surface.tsx src/contexts/acp-connections-context.tsx`

Expected: PASS with no layout overflow or unused legacy queue symbols.

- [ ] **Step 8: Commit Task 11**

```bash
git add src/components/chat/shared-message-queue-display.tsx src/components/chat/shared-message-queue-display.test.tsx src/components/chat/chat-input.tsx src/components/chat/message-input.tsx src/components/chat/message-input.test.tsx src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.tsx src/contexts/acp-connections-context.tsx
git commit -m "feat: render authoritative shared ACP prompt queue"
```

### Task 12: Concurrency Integration, Observability, Compatibility, and Full Verification

**Files:**
- Modify: `src-tauri/src/acp/shared_session.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/internal_bus.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/web/handlers/event_metrics.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/tests/shared_session_http.rs`
- Modify: `src-tauri/tests/ws_attach.rs`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/lib/types.ts`
- Modify: `docs/superpowers/plans/2026-08-14-shared-acp-multi-client-session-broker-design.md`

**Interfaces:**
- Consumes: every prior task, existing `acp_get_event_metrics`, registry agent enumeration, `AppState` test fixtures, and shutdown cleanup.
- Produces: secret-safe broker metrics/diagnostics endpoint, final race/route/restart/compatibility matrix, implementation status note in the design document, and full repository verification evidence.

- [ ] **Step 1: Write failing end-to-end concurrency and diagnostic tests**

Add integration tests for:

```text
2, 10, and 100 simultaneous connect-or-attach calls -> one spawn/id/generation, independent leases
attach during Bootstrapping -> same ready event or same typed failed event
distinct conversations -> concurrent bootstrap, no global serialization
64 concurrent accepted prompts -> contiguous enqueue_seq and exact dispatch order
32 MiB/64 item bounds -> reject new only
sender lease expiry -> queued item remains
dispatch versus cancel -> exactly one terminal outcome
exact-turn concurrent stop -> one cancel/finalizer, tail preserved
permission/question/plan two-client races -> one responder call each
lagged/reconnected socket -> snapshot restores phase, queue, turn, interactions, lease expiry
all sockets close/leases expire during active/background work -> no disconnect
Ready+Connected+no clients/work -> retained before 900s, reclaimed after final CAS
required companion failure -> typed failure, no fallback, cleanup fence, one generation-incrementing retry
simulated server restart -> no leases/queue restored and no prompt auto-resubmitted
legacy disconnect/touch/prompt -> cannot own, kill, or keep alive broker root
explicit terminate with current generation -> Closing, bounded teardown, no queued/active survivor
all eleven built-ins plus one custom agent -> same shared admission code path
```

Seed unique sentinels for a lease, client/device/request ids, prompt/answer text, working path, bearer token, environment value, and agent stderr. Add a JSON diagnostic test that recursively rejects both forbidden key names and every sentinel value/sub-string; also assert the authenticated diagnostic route returns 401 without the bearer token and never reflects the supplied token.

- [ ] **Step 2: Run integration tests to verify remaining RED assertions**

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --test shared_session_http -- --nocapture`

Run: `cd src-tauri && cargo test --no-default-features --features server,test-utils --test ws_attach -- --nocapture`

Expected: FAIL only for missing metrics/diagnostic/conformance/restart assertions added in Step 1.

- [ ] **Step 3: Expose secret-safe metrics and diagnostics**

Expose the complete `SharedSessionMetricsSnapshot` defined in Task 1 as a nested broker snapshot on the existing event metrics response:

```rust
pub struct SharedSessionMetricsSnapshot {
    pub created_total: u64,
    pub attached_total: u64,
    pub live_sessions: u64,
    pub active_leases: u64,
    pub bootstrap_ready_total: u64,
    pub bootstrap_failed_total: BTreeMap<String, u64>,
    pub bootstrap_duration_ms_total: u64,
    pub bootstrap_duration_samples: u64,
    pub waiting_prompts: u64,
    pub waiting_bytes: u64,
    pub enqueue_total: u64,
    pub cancel_total: u64,
    pub dispatch_total: u64,
    pub capacity_rejected_total: u64,
    pub queue_item_failed_total: u64,
    pub interaction_winner_total: u64,
    pub interaction_stale_total: u64,
    pub stale_stop_total: u64,
    pub lease_expired_total: u64,
    pub lease_released_total: u64,
    pub idle_candidate_total: u64,
    pub idle_cas_lost_total: u64,
    pub idle_reclaimed_total: u64,
    pub cleanup_duration_ms_total: u64,
    pub cleanup_duration_samples: u64,
    pub cleanup_incomplete_total: u64,
}
```

Key `bootstrap_failed_total` only by a bounded built-in/custom-agent category, route capability (`standard | required_companion | fallback`), and stable error code; unknown custom names collapse to `custom` so labels cannot grow per user input.

Add an additive `AcpEventMetricsSnapshot` wrapper containing the existing flattened event-bus counters plus `shared_session_broker: SharedSessionMetricsSnapshot`. Change `acp_get_event_metrics_core` to accept both `&EventBusMetrics` and `&SharedSessionBroker`; update the existing Tauri command and authenticated `GET /api/debug/event_metrics` handler to return the wrapper. Existing counter field names remain at the top level, so pollers only see one new nested field.

Add `ConnectionManager::shared_session_diagnostics() -> Vec<SharedSessionDiagnostic>`, core helper `acp_get_shared_session_diagnostics_core`, and authenticated `GET /api/debug/shared_sessions`. Each list item contains only connection id, optional conversation id, generation, phase, bounded built-in-or-`custom` agent category, lease count, queue depth/bytes, stable idle-blocker names, cleanup state, and aggregate durations. Sort by connection id for deterministic output. It never returns lease/client/request ids or prompt/answer/path data. Use structured tracing fields with the same allowlist; no tracing statement may format a full request, guard, prompt, event, answer, launch identity, or diagnostic source struct.

- [ ] **Step 4: Close shutdown, process-exit, and restart behavior**

Add an `accepting: Arc<AtomicBool>` to the broker, initialized true. `begin_shutdown()` swaps it false before any await; every connect, enqueue, interaction, and stop admission checks it and returns `Closing`, while release/cleanup remains available. On graceful shutdown, call that fence first, emit server shutdown through existing WebSocket handling, publish `Closing` for indexed shared records, and delegate to bounded `disconnect_all` plus exact map-absence cleanup. On unexpected ACP exit, the incarnation-fenced monitor atomically marks failed, settles the active turn once, fails every queued item with `session_unavailable`, and performs cleanup. A new `ConnectionManager` constructs a fresh accepting broker with empty indexes, proving restart restores no queue/leases and never calls prompt dispatch without a fresh client enqueue.

- [ ] **Step 5: Add a registry-driven route conformance test**

Iterate `BUILTIN_AGENT_TYPES` plus `AgentType::custom("shared-conformance").expect("valid fixture id")`, registered through the existing custom-agent test registry. For each, build launch inputs through the existing registry/terminal-context fixture, call the same `connect_or_attach_shared`, and assert the returned broker diagnostic reports the same record/lease/dispatcher implementation with no agent-specific branch. For Codeg-required route plans, assert `Ready` waits for companion readiness; for standard routes, ACP readiness alone reaches `Ready`; for global-default fallback-permitted plans, assert one typed route failure produces same-id fallback; for session-override Codeg plans, assert the same failure closes without fallback.

- [ ] **Step 6: Run the full frontend verification suite**

Run: `pnpm eslint .`

Expected: PASS.

Run: `pnpm test`

Expected: PASS for all Vitest files, including shared connection/queue convergence and existing Tauri/pop-out/delegation regressions.

Run: `pnpm build`

Expected: PASS with static export output and no dynamic route usage.

- [ ] **Step 7: Run Rust desktop compile/lint plus full server/MCP verification without macOS keychain access**

Run from `src-tauri/` with 30-60 second yields for each long command:

```bash
cargo fmt --all -- --check
cargo check
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server,test-utils --bin codeg-server --lib
cargo test --no-default-features --features server,test-utils --test shared_session_http
cargo test --no-default-features --features server,test-utils --test ws_attach
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: every command exits 0. Do not substitute either default-feature desktop test command on macOS; it currently reads the real `codeg` login-keychain entry and repeatedly prompts because each rebuilt `codeg_lib-<hash>` has a new temporary signature. Record this known test-isolation gap separately rather than granting keychain access. If a snapshot changes, run `cargo insta review`, accept only the intended additive shared fields/events, then rerun the affected server-mode suite.

- [ ] **Step 8: Update the approved design document implementation status only after verification is green**

After every Step 6 and Step 7 command exits zero, append one factual line under `Status` naming this implementation plan and branch, without changing approved requirements:

```text
Implementation completed from docs/superpowers/plans/2026-08-16-shared-acp-multi-client-session-broker.md on branch codex/shared-acp-session-broker; verification commands are recorded in that branch's final task handoff.
```

If any required command is not run or fails, do not add this line; record the actual incomplete verification in the Task 12 report instead.

- [ ] **Step 9: Commit Task 12 and force-add the ignored design/plan documents**

```bash
git add src-tauri/src/acp/shared_session.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/internal_bus.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/web/handlers/event_metrics.rs src-tauri/src/web/router.rs src-tauri/tests/shared_session_http.rs src-tauri/tests/ws_attach.rs src/contexts/acp-connections-context.test.tsx src/lib/types.ts
git add -f docs/superpowers/plans/2026-08-14-shared-acp-multi-client-session-broker-design.md docs/superpowers/plans/2026-08-16-shared-acp-multi-client-session-broker.md
git commit -m "test: verify shared ACP multi-client sessions"
```

## Final Acceptance Checklist

- [ ] Two or more authenticated devices opening one persisted conversation concurrently receive one connection incarnation and separate leases.
- [ ] A second device attaches during bootstrap without waiting for companion initialization and observes the same ready/failed outcome.
- [ ] All built-in and conforming custom ACP agents enter the same broker path; explicitly required companion failure is typed, cleaned, and never silently downgraded.
- [ ] Accepted prompts have one global `enqueue_seq`, dispatch exactly once in order, survive sender disconnect, and obey 64-item/32-MiB waiting bounds.
- [ ] Any active lease can cancel only an unstarted item, answer the current interaction with first-winner semantics, and stop only the exact current `turn_id`.
- [ ] Stop preserves the queue tail and the next item starts only after cancellation terminal/quarantine completion.
- [ ] Zero-client active, waiting-input, queued, continuation, delegated, and background-working sessions are never client-idle reaped.
- [ ] A Ready + Connected + zero-lease + zero-work session receives a fresh full 15-minute grace and is removed only after a final generation/predicate CAS.
- [ ] Snapshot/replay reconstructs phase, queue, active turn, pending interactions, and this subscription's lease expiry after refresh, lag, or mobile resume.
- [ ] Legacy browser disconnect/touch/unfenced mutation cannot terminate, keep alive, or mutate a broker-managed root.
- [ ] Server restart restores no leases/FIFO/turn state and never automatically resubmits an interrupted prompt.
- [ ] Metrics, diagnostics, errors, and logs contain no tokens, lease/client ids, prompt/answer text, paths, environment, stderr, or raw output.
- [ ] Frontend lint/test/build, desktop Rust check/clippy, full server-mode shared tests, and MCP check/clippy pass without accessing the real macOS keychain; the known default-feature desktop-test isolation gap is reported explicitly.
