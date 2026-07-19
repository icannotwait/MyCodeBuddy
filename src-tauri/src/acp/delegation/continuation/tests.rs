use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use tokio_util::sync::CancellationToken;

use super::coordinator::{
    ContinuationError, ContinuationPromptRequest, DelegationContinuationCoordinator,
    JoinArmRequest, ManagerContinuationPort, ParentContinuationPort, ParentTurnSnapshot,
    PromptAdmissionResult, SuspendRequest, SystemContinuationClock,
};
use super::store::{
    ContStoreError, ContinuationPatch, ContinuationRecord, ContinuationStore,
    InMemoryContinuationStore, NewContinuation,
};
use super::types::{
    ContinuationFailureCode, ContinuationState, ContinuationWaitingProjection,
    ContinuationWakeReason, CONTINUATION_CHECKPOINT_MS,
};
use super::{filter_internal_continuation_turns, internal_prompt_marker};
use crate::acp::connection::{ConnectionControl, SuspensionAck};
use crate::acp::delegation::attention::{
    mock::MemoryDelegationAttentionStore, DelegationAttentionStore, NewAttentionRequest,
};
use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker, JoinEvaluation};
use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
use crate::acp::delegation::store::{mock::MockTaskStore, DelegationTaskStore};
use crate::acp::delegation::types::{
    DelegationError, DelegationOutcome, DelegationSuccess, DelegationWakeReason, TaskStatus,
};
use crate::acp::error::AcpError;
use crate::acp::manager::dispatch_suspension_control;
use crate::acp::session_state::SessionState;
use crate::acp::types::AcpEvent;
use crate::models::{AgentType, ContentBlock, MessageTurn, TurnRole};
use crate::web::event_bridge::EventEmitter;

struct RootDepth;

#[async_trait]
impl ConversationDepthLookup for RootDepth {
    async fn parent_of(&self, _conversation_id: i32) -> Result<Option<i32>, DelegationError> {
        Ok(None)
    }
}

fn test_broker() -> DelegationBroker {
    DelegationBroker::new(
        Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
        Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
    )
}

async fn complete_seeded_task(broker: &DelegationBroker, task_id: &str) {
    broker.seed_live_task_for_test("parent", task_id).await;
    broker
        .complete_call(
            task_id,
            DelegationOutcome::Ok(DelegationSuccess {
                text: "done".into(),
                child_conversation_id: 99,
                child_agent_type: AgentType::ClaudeCode,
                turn_count: 1,
                duration_ms: 1,
                token_usage: None,
            }),
        )
        .await;
}

#[tokio::test]
async fn continuation_broker_immediate_all_terminal_snapshot_is_ready() {
    let broker = Arc::new(test_broker());
    complete_seeded_task(&broker, "task-terminal").await;
    let store = Arc::new(InMemoryContinuationStore::default());
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ReadyPort),
        Arc::new(SystemContinuationClock::new()),
    );

    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-terminal".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await;
    let super::coordinator::JoinArmOutcome::Immediate(batch) = outcome.unwrap() else {
        panic!("all-terminal Join must be immediately ready")
    };
    assert_eq!(batch.wake_reason, Some(DelegationWakeReason::AllTerminal));
    assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
    assert!(store.list_non_terminal().await.unwrap().is_empty());
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_broker_immediate_attention_snapshot_is_ready() {
    let attention = Arc::new(MemoryDelegationAttentionStore::new());
    let broker = Arc::new(
        test_broker().with_attention_store(attention.clone() as Arc<dyn DelegationAttentionStore>),
    );
    broker
        .seed_live_task_for_test("parent", "task-attention")
        .await;
    attention
        .open_or_recover(NewAttentionRequest {
            task_id: "task-attention".into(),
            parent_conversation_id: 7,
            child_conversation_id: 99,
            child_tool_call_id: "child-tool".into(),
            message: "Need a decision".into(),
            created_at: Utc::now(),
        })
        .await
        .unwrap();

    let store = Arc::new(InMemoryContinuationStore::default());
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ReadyPort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-attention".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await;
    let super::coordinator::JoinArmOutcome::Immediate(batch) = outcome.unwrap() else {
        panic!("open attention must make Join immediately ready")
    };
    assert_eq!(
        batch.wake_reason,
        Some(DelegationWakeReason::AttentionRequired)
    );
    assert_eq!(batch.attention_requests.unwrap().len(), 1);
    assert!(store.list_non_terminal().await.unwrap().is_empty());
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_broker_immediate_unavailable_snapshot_is_ready() {
    let broker = Arc::new(test_broker());
    let store = Arc::new(InMemoryContinuationStore::default());
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ReadyPort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["missing-task".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await;
    let super::coordinator::JoinArmOutcome::Immediate(batch) = outcome.unwrap() else {
        panic!("unknown task must fail open as unavailable")
    };
    assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
    assert_eq!(batch.tasks[0].status, TaskStatus::Unknown);
    assert!(store.list_non_terminal().await.unwrap().is_empty());
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_broker_attention_has_priority_over_unavailable() {
    let attention = Arc::new(MemoryDelegationAttentionStore::new());
    let broker =
        test_broker().with_attention_store(attention.clone() as Arc<dyn DelegationAttentionStore>);
    broker
        .seed_live_task_for_test("parent", "task-attention")
        .await;
    attention
        .open_or_recover(NewAttentionRequest {
            task_id: "task-attention".into(),
            parent_conversation_id: 7,
            child_conversation_id: 99,
            child_tool_call_id: "child-tool".into(),
            message: "Need a decision".into(),
            created_at: Utc::now(),
        })
        .await
        .unwrap();

    let evaluation = broker
        .evaluate_join_snapshot(
            "parent",
            7,
            &["task-attention".into(), "missing-task".into()],
        )
        .await;
    let JoinEvaluation::Ready(batch) = evaluation else {
        panic!("attention must be immediately ready")
    };
    assert_eq!(
        batch.wake_reason,
        Some(DelegationWakeReason::AttentionRequired)
    );
    assert_eq!(batch.attention_requests.unwrap().len(), 1);
}

struct ReadyPort;

#[async_trait]
impl ParentContinuationPort for ReadyPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        Ok(ParentTurnSnapshot {
            connection_id: connection_id.into(),
            conversation_id: 7,
            session_id: "session-7".into(),
            turn_generation: 3,
            turn_in_flight: true,
        })
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        Ok(SuspensionAck {
            continuation_id: request.continuation_id,
            parent_turn_generation: request.parent_turn_generation,
        })
    }

    async fn admit_continuation(
        &self,
        _request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        Ok(PromptAdmissionResult::Admitted)
    }

    async fn publish_waiting(
        &self,
        _connection_id: &str,
        _waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        Ok(())
    }

    async fn publish_failure(
        &self,
        _connection_id: &str,
        _code: super::types::ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        Ok(())
    }
}

struct SnapshotGatedPort {
    snapshot_entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    snapshot_release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl SnapshotGatedPort {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                snapshot_entered: Mutex::new(Some(entered_tx)),
                snapshot_release: tokio::sync::Mutex::new(Some(release_rx)),
            }),
            entered_rx,
            release_tx,
        )
    }
}

#[async_trait]
impl ParentContinuationPort for SnapshotGatedPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        if let Some(entered) = self
            .snapshot_entered
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = entered.send(());
        }
        if let Some(release) = self.snapshot_release.lock().await.take() {
            let _ = release.await;
        }
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        ReadyPort.suspend_parent(request).await
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        ReadyPort.admit_continuation(request).await
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

struct ObservedStore {
    inner: InMemoryContinuationStore,
    insert_entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    insert_release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    wake_pending: Mutex<Option<tokio::sync::oneshot::Sender<ContinuationRecord>>>,
    wake_claim_wins: AtomicUsize,
    terminal: tokio::sync::Notify,
    drain_check_broker: Mutex<Option<Arc<DelegationBroker>>>,
    drain_check_task: Mutex<Option<(Arc<MockTaskStore>, String)>>,
    drain_verified: std::sync::atomic::AtomicBool,
    fail_transition_to: Mutex<Option<ContinuationState>>,
    error_transition_to: Mutex<Option<ContinuationState>>,
    failure_attempted: tokio::sync::Notify,
    failure_fence: Mutex<Option<(u64, u64, ContinuationState)>>,
    cleanup_claim_attempted: tokio::sync::Notify,
    cleanup_claim_fence: Mutex<Option<(u64, u64, ContinuationState)>>,
    fail_next_load: AtomicUsize,
    load_failure_observed: tokio::sync::Notify,
}

impl ObservedStore {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<ContinuationRecord>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                inner: InMemoryContinuationStore::default(),
                insert_entered: Mutex::new(None),
                insert_release: tokio::sync::Mutex::new(None),
                wake_pending: Mutex::new(Some(tx)),
                wake_claim_wins: AtomicUsize::new(0),
                terminal: tokio::sync::Notify::new(),
                drain_check_broker: Mutex::new(None),
                drain_check_task: Mutex::new(None),
                drain_verified: std::sync::atomic::AtomicBool::new(false),
                fail_transition_to: Mutex::new(None),
                error_transition_to: Mutex::new(None),
                failure_attempted: tokio::sync::Notify::new(),
                failure_fence: Mutex::new(None),
                cleanup_claim_attempted: tokio::sync::Notify::new(),
                cleanup_claim_fence: Mutex::new(None),
                fail_next_load: AtomicUsize::new(0),
                load_failure_observed: tokio::sync::Notify::new(),
            }),
            rx,
        )
    }

    async fn install_insert_gate(
        &self,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        *self
            .insert_entered
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(entered);
        *self.insert_release.lock().await = Some(release);
    }

    fn fail_next_transition_to(&self, state: ContinuationState) {
        *self
            .fail_transition_to
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(state);
    }

    fn error_next_transition_to(&self, state: ContinuationState) {
        *self
            .error_transition_to
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(state);
    }

    fn fail_next_load(&self) {
        self.fail_next_load.store(1, Ordering::Release);
    }
}

#[async_trait]
impl ContinuationStore for ObservedStore {
    async fn insert_arming(
        &self,
        new: NewContinuation,
    ) -> Result<ContinuationRecord, ContStoreError> {
        if let Some(entered) = self
            .insert_entered
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = entered.send(());
        }
        if let Some(release) = self.insert_release.lock().await.take() {
            let _ = release.await;
        }
        self.inner.insert_arming(new).await
    }

    async fn load(
        &self,
        continuation_id: &str,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        if self.fail_next_load.swap(0, Ordering::AcqRel) == 1 {
            self.load_failure_observed.notify_waiters();
            return Ok(None);
        }
        self.inner.load(continuation_id).await
    }

    async fn load_active_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        self.inner
            .load_active_for_conversation(conversation_id)
            .await
    }

    async fn list_non_terminal(&self) -> Result<Vec<ContinuationRecord>, ContStoreError> {
        self.inner.list_non_terminal().await
    }

    async fn cas_transition(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        patch: ContinuationPatch,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        let should_error = {
            let mut target = self
                .error_transition_to
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if target.as_ref() == Some(&patch.state) {
                target.take();
                true
            } else {
                false
            }
        };
        if should_error {
            return Err(ContStoreError::InvalidRecord(
                "injected transition error".into(),
            ));
        }
        let should_fail = {
            let mut target = self
                .fail_transition_to
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if target.as_ref() == Some(&patch.state) {
                target.take();
                true
            } else {
                false
            }
        };
        if should_fail {
            return Ok(None);
        }
        let result = self
            .inner
            .cas_transition(
                continuation_id,
                generation,
                expected_version,
                expected_state,
                patch,
            )
            .await?;
        if let Some(record) = result.as_ref().filter(|row| {
            row.state == ContinuationState::WakePending
                && expected_state != ContinuationState::WakePending
        }) {
            self.wake_claim_wins.fetch_add(1, Ordering::Relaxed);
            if let Some(tx) = self
                .wake_pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = tx.send(record.clone());
            }
        }
        if result.as_ref().is_some_and(|record| {
            matches!(
                record.state,
                ContinuationState::Completed
                    | ContinuationState::Cancelled
                    | ContinuationState::Failed
            )
        }) {
            self.terminal.notify_waiters();
        }
        Ok(result)
    }

    async fn cas_fail_and_cancel_parent(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        failure_code: ContinuationFailureCode,
        finished_at: chrono::DateTime<Utc>,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        *self
            .failure_fence
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some((generation, expected_version, expected_state));
        self.failure_attempted.notify_waiters();
        let broker = self
            .drain_check_broker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let task = self
            .drain_check_task
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(broker) = broker {
            assert_eq!(
                broker.pending_count().await,
                0,
                "child registry drains before continuation terminal CAS"
            );
        }
        if let Some((task_store, task_id)) = task {
            let persisted = task_store.persisted(&task_id).await;
            assert_eq!(
                persisted.status,
                TaskStatus::Canceled,
                "child durable settle completes before continuation terminal CAS"
            );
            assert_eq!(persisted.error_code.as_deref(), Some("parent_turn_failed"));
            self.drain_verified.store(true, Ordering::Relaxed);
        }
        let result = self
            .inner
            .cas_fail_and_cancel_parent(
                continuation_id,
                generation,
                expected_version,
                expected_state,
                failure_code,
                finished_at,
            )
            .await?;
        if result.is_some() {
            self.terminal.notify_waiters();
        }
        Ok(result)
    }

    async fn cas_claim_cleanup(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        *self
            .cleanup_claim_fence
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some((generation, expected_version, expected_state));
        let result = self
            .inner
            .cas_claim_cleanup(
                continuation_id,
                generation,
                expected_version,
                expected_state,
            )
            .await;
        self.cleanup_claim_attempted.notify_waiters();
        result
    }

    async fn matches_admitted_marker(
        &self,
        conversation_id: i32,
        marker: &str,
    ) -> Result<bool, ContStoreError> {
        self.inner
            .matches_admitted_marker(conversation_id, marker)
            .await
    }

    async fn load_latest_failure_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        self.inner
            .load_latest_failure_for_conversation(conversation_id)
            .await
    }
}

struct GatedPort {
    suspend_started: Mutex<Option<tokio::sync::oneshot::Sender<SuspendRequest>>>,
    suspend_release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    admission_started: Mutex<Option<tokio::sync::oneshot::Sender<ContinuationPromptRequest>>>,
}

impl GatedPort {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<SuspendRequest>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<ContinuationPromptRequest>,
    ) {
        let (suspend_started_tx, suspend_started_rx) = tokio::sync::oneshot::channel();
        let (suspend_release_tx, suspend_release_rx) = tokio::sync::oneshot::channel();
        let (admission_started_tx, admission_started_rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                suspend_started: Mutex::new(Some(suspend_started_tx)),
                suspend_release: tokio::sync::Mutex::new(Some(suspend_release_rx)),
                admission_started: Mutex::new(Some(admission_started_tx)),
            }),
            suspend_started_rx,
            suspend_release_tx,
            admission_started_rx,
        )
    }
}

#[async_trait]
impl ParentContinuationPort for GatedPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        Ok(ParentTurnSnapshot {
            connection_id: connection_id.into(),
            conversation_id: 7,
            session_id: "session-7".into(),
            turn_generation: 3,
            turn_in_flight: true,
        })
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        let release = self.suspend_release.lock().await.take();
        if let Some(tx) = self
            .suspend_started
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = tx.send(request.clone());
        }
        if let Some(release) = release {
            let _ = release.await;
        }
        Ok(SuspensionAck {
            continuation_id: request.continuation_id,
            parent_turn_generation: request.parent_turn_generation,
        })
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        if let Some(tx) = self
            .admission_started
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = tx.send(request);
        }
        Ok(PromptAdmissionResult::Admitted)
    }

    async fn publish_waiting(
        &self,
        _connection_id: &str,
        _waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        Ok(())
    }

    async fn publish_failure(
        &self,
        _connection_id: &str,
        _code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        Ok(())
    }
}

struct RetryPort {
    attempts: AtomicUsize,
    attempt_tx: tokio::sync::mpsc::UnboundedSender<chrono::DateTime<Utc>>,
    succeed_on: usize,
}

impl RetryPort {
    fn new() -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<chrono::DateTime<Utc>>,
    ) {
        let (attempt_tx, attempt_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                attempt_tx,
                succeed_on: 4,
            }),
            attempt_rx,
        )
    }

    fn always_fail() -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<chrono::DateTime<Utc>>,
    ) {
        let (attempt_tx, attempt_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                attempt_tx,
                succeed_on: usize::MAX,
            }),
            attempt_rx,
        )
    }
}

#[async_trait]
impl ParentContinuationPort for RetryPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        Ok(ParentTurnSnapshot {
            connection_id: connection_id.into(),
            conversation_id: 7,
            session_id: "session-7".into(),
            turn_generation: 3,
            turn_in_flight: true,
        })
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        Ok(SuspensionAck {
            continuation_id: request.continuation_id,
            parent_turn_generation: request.parent_turn_generation,
        })
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.attempt_tx.send(request.admitted_at);
        if attempt < self.succeed_on {
            Err(ContinuationError::PromptDelivery(AcpError::ProcessExited))
        } else {
            Ok(PromptAdmissionResult::Admitted)
        }
    }

    async fn publish_waiting(
        &self,
        _connection_id: &str,
        _waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        Ok(())
    }

    async fn publish_failure(
        &self,
        _connection_id: &str,
        _code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        Ok(())
    }
}

struct FinalFailureGatePort {
    attempts: AtomicUsize,
    final_entered: Mutex<Option<tokio::sync::oneshot::Sender<ContinuationPromptRequest>>>,
    final_release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl FinalFailureGatePort {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<ContinuationPromptRequest>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                final_entered: Mutex::new(Some(entered_tx)),
                final_release: tokio::sync::Mutex::new(Some(release_rx)),
            }),
            entered_rx,
            release_tx,
        )
    }
}

#[async_trait]
impl ParentContinuationPort for FinalFailureGatePort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        ReadyPort.suspend_parent(request).await
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        if attempt == 4 {
            let release = self.final_release.lock().await.take();
            if let Some(entered) = self
                .final_entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = entered.send(request);
            }
            if let Some(release) = release {
                let _ = release.await;
            }
        }
        Err(ContinuationError::PromptDelivery(AcpError::ProcessExited))
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

struct PreSuspendDispatchFailurePort;

#[async_trait]
impl ParentContinuationPort for PreSuspendDispatchFailurePort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        _request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        Err(ContinuationError::SuspendDispatch(AcpError::ProcessExited))
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        ReadyPort.admit_continuation(request).await
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

struct ConflictAdmissionPort;

#[async_trait]
impl ParentContinuationPort for ConflictAdmissionPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        ReadyPort.suspend_parent(request).await
    }

    async fn admit_continuation(
        &self,
        _request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        Err(ContinuationError::StateConflict)
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

struct WaitingPublishFailurePort;

#[async_trait]
impl ParentContinuationPort for WaitingPublishFailurePort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        ReadyPort.suspend_parent(request).await
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        ReadyPort.admit_continuation(request).await
    }

    async fn publish_waiting(
        &self,
        _connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        if waiting.is_some() {
            Err(ContinuationError::ParentIdentityChanged)
        } else {
            Ok(())
        }
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

struct ParentIdentityDriftPort {
    snapshots: AtomicUsize,
}

#[async_trait]
impl ParentContinuationPort for ParentIdentityDriftPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        let mut snapshot = ReadyPort.snapshot_parent(connection_id).await?;
        if self.snapshots.fetch_add(1, Ordering::Relaxed) > 0 {
            snapshot.conversation_id += 1;
        }
        Ok(snapshot)
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        ReadyPort.suspend_parent(request).await
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        ReadyPort.admit_continuation(request).await
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

#[derive(Clone, Copy)]
enum SuspensionFailureCause {
    DrainTimeout,
    ParentConnectionLost,
    ParentStopRequested,
}

struct SuspensionFailurePort(SuspensionFailureCause);

#[async_trait]
impl ParentContinuationPort for SuspensionFailurePort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        _request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        Err(match self.0 {
            SuspensionFailureCause::DrainTimeout => ContinuationError::SuspendDrainTimeout,
            SuspensionFailureCause::ParentConnectionLost => ContinuationError::ParentConnectionLost,
            SuspensionFailureCause::ParentStopRequested => ContinuationError::ParentStopRequested,
        })
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        ReadyPort.admit_continuation(request).await
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

struct PostAdmissionReloadFailurePort {
    store: Arc<ObservedStore>,
}

#[async_trait]
impl ParentContinuationPort for PostAdmissionReloadFailurePort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        ReadyPort.snapshot_parent(connection_id).await
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        ReadyPort.suspend_parent(request).await
    }

    async fn admit_continuation(
        &self,
        _request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        self.store.fail_next_load();
        Ok(PromptAdmissionResult::Admitted)
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_waiting(connection_id, waiting).await
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        ReadyPort.publish_failure(connection_id, code).await
    }
}

#[tokio::test]
async fn continuation_coordinator_waiter_close_before_insert_creates_no_row() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake) = ObservedStore::new();
    let (insert_entered_tx, mut insert_entered_rx) = tokio::sync::oneshot::channel();
    let (insert_release_tx, insert_release_rx) = tokio::sync::oneshot::channel();
    store
        .install_insert_gate(insert_entered_tx, insert_release_rx)
        .await;
    let (port, snapshot_entered, snapshot_release) = SnapshotGatedPort::new();
    let coordinator = Arc::new(DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    ));
    let waiter_closed = CancellationToken::new();
    let arm = tokio::spawn({
        let coordinator = coordinator.clone();
        let waiter_closed = waiter_closed.clone();
        async move {
            coordinator
                .begin_arm_from_join(JoinArmRequest {
                    parent_connection_id: "parent".into(),
                    parent_conversation_id: 7,
                    task_ids: vec!["task-running".into()],
                    waiter_closed,
                })
                .await
        }
    });
    snapshot_entered
        .await
        .expect("snapshot gate establishes the pre-insert boundary");
    waiter_closed.cancel();
    snapshot_release.send(()).unwrap();

    let result = arm.await.unwrap();
    assert!(matches!(
        result,
        Err(ContinuationError::WaiterClosedBeforeInsert)
    ));
    assert_eq!(
        insert_entered_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty),
        "cancel observed before insert must never enter the store boundary"
    );
    drop(insert_release_tx);
    assert!(store.list_non_terminal().await.unwrap().is_empty());
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_waiter_close_after_insert_entry_keeps_owned_worker() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake) = ObservedStore::new();
    let (insert_entered_tx, insert_entered_rx) = tokio::sync::oneshot::channel();
    let (insert_release_tx, insert_release_rx) = tokio::sync::oneshot::channel();
    store
        .install_insert_gate(insert_entered_tx, insert_release_rx)
        .await;
    let coordinator = Arc::new(DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ReadyPort),
        Arc::new(SystemContinuationClock::new()),
    ));
    let waiter_closed = CancellationToken::new();
    let arm = tokio::spawn({
        let coordinator = coordinator.clone();
        let waiter_closed = waiter_closed.clone();
        async move {
            coordinator
                .begin_arm_from_join(JoinArmRequest {
                    parent_connection_id: "parent".into(),
                    parent_conversation_id: 7,
                    task_ids: vec!["task-running".into()],
                    waiter_closed,
                })
                .await
        }
    });
    insert_entered_rx
        .await
        .expect("insert entry establishes the durable ownership boundary");
    waiter_closed.cancel();
    assert!(store.list_non_terminal().await.unwrap().is_empty());
    insert_release_tx.send(()).unwrap();

    let outcome = arm.await.unwrap().unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("post-entry waiter close must keep the durable continuation")
    };
    completion.await.unwrap().unwrap();
    assert_eq!(coordinator.worker_count(), 1);
    complete_seeded_task(&broker, "task-running").await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while coordinator.worker_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned worker reaches terminal cleanup");
    assert_eq!(
        store.load(&continuation_id).await.unwrap().unwrap().state,
        ContinuationState::Completed
    );
}

#[tokio::test]
async fn continuation_coordinator_post_registration_completion_claims_before_suspend_ack() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, wake_pending) = ObservedStore::new();
    let (port, suspend_started, suspend_release, _admission_started) = GatedPort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );

    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming { mut completion, .. } = outcome else {
        panic!("running Join must arm a continuation")
    };
    let _suspend = tokio::select! {
        request = suspend_started => request.expect("suspension dispatched"),
        result = &mut completion => panic!("arm worker ended before dispatch: {result:?}"),
    };

    complete_seeded_task(&broker, "task-running").await;
    let claimed = wake_pending.await.expect("wake CAS won");
    assert_eq!(claimed.state, ContinuationState::WakePending);
    assert!(claimed.suspended_at.is_none());

    suspend_release.send(()).unwrap();
    let ack = completion.await.unwrap().unwrap();
    assert_eq!(ack.continuation_id, claimed.continuation_id);
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_checkpoint_uses_exact_logical_240_seconds() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, mut wake_pending) = ObservedStore::new();
    let (port, suspend_started, suspend_release, admission_started) = GatedPort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );

    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming { mut completion, .. } = outcome else {
        panic!("running Join must arm a continuation")
    };
    tokio::select! {
        request = suspend_started => { request.expect("suspension dispatched"); }
        result = &mut completion => panic!("arm worker ended before dispatch: {result:?}"),
    }
    suspend_release.send(()).unwrap();
    completion.await.unwrap().unwrap();

    tokio::time::advance(std::time::Duration::from_millis(
        CONTINUATION_CHECKPOINT_MS - 1,
    ))
    .await;
    assert!(matches!(
        wake_pending.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    let claimed = wake_pending.await.expect("checkpoint wake CAS won");
    assert_eq!(
        claimed.wake_reason,
        Some(ContinuationWakeReason::Checkpoint)
    );
    assert_eq!(
        claimed
            .wake_claimed_at
            .expect("claim timestamp")
            .signed_duration_since(claimed.armed_at)
            .num_milliseconds(),
        CONTINUATION_CHECKPOINT_MS as i64
    );
    let request = admission_started.await.expect("checkpoint prompt admitted");
    assert_eq!(request.origin.continuation_id(), claimed.continuation_id);
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_event_deadline_race_claims_once_and_clears_registry() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let (port, suspend_started, suspend_release, admission_started) = GatedPort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );

    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming { mut completion, .. } = outcome else {
        panic!("running Join must arm a continuation")
    };
    tokio::select! {
        request = suspend_started => { request.expect("suspension dispatched"); }
        result = &mut completion => panic!("arm worker ended before dispatch: {result:?}"),
    }
    suspend_release.send(()).unwrap();
    completion.await.unwrap().unwrap();

    tokio::join!(
        complete_seeded_task(&broker, "task-running"),
        tokio::time::advance(std::time::Duration::from_millis(CONTINUATION_CHECKPOINT_MS,)),
    );
    admission_started.await.expect("one admission attempt");
    terminal.await;

    assert_eq!(store.wake_claim_wins.load(Ordering::Relaxed), 1);
    assert_eq!(coordinator.worker_count(), 0);
}

async fn assert_post_ack_transition_failure_is_terminalized(target: ContinuationState) {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    store.fail_next_transition_to(target);
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ReadyPort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };

    if target == ContinuationState::Waiting {
        assert!(matches!(
            completion.await.unwrap(),
            Err(ContinuationError::StateConflict)
        ));
    } else {
        completion.await.unwrap().unwrap();
        complete_seeded_task(&broker, "task-running").await;
    }

    tokio::time::timeout(std::time::Duration::from_millis(250), &mut terminal)
        .await
        .expect("post-ack transition failure must reach a terminal row");
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Failed);
    assert_eq!(
        row.failure_code,
        Some(ContinuationFailureCode::StateConflict)
    );
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_post_ack_suspended_cas_failure_is_not_ownerless() {
    assert_post_ack_transition_failure_is_terminalized(ContinuationState::Waiting).await;
}

#[tokio::test]
async fn continuation_coordinator_post_ack_resuming_cas_failure_is_not_ownerless() {
    assert_post_ack_transition_failure_is_terminalized(ContinuationState::Resuming).await;
}

#[tokio::test]
async fn continuation_coordinator_post_admission_completion_cas_failure_is_not_ownerless() {
    assert_post_ack_transition_failure_is_terminalized(ContinuationState::Completed).await;
}

#[tokio::test]
async fn continuation_coordinator_post_admission_reload_failure_retains_owner_until_cleanup() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let load_failure = store.load_failure_observed.notified();
    tokio::pin!(load_failure);
    load_failure.as_mut().enable();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(PostAdmissionReloadFailurePort {
            store: store.clone(),
        }),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    completion.await.unwrap().unwrap();
    complete_seeded_task(&broker, "task-running").await;
    load_failure.await;

    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Resuming);
    assert_eq!(coordinator.worker_count(), 1);
    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_waiting_publication_failure_is_not_ownerless() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(WaitingPublishFailurePort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    assert!(matches!(
        completion.await.unwrap(),
        Err(ContinuationError::StateConflict)
    ));
    tokio::time::timeout(std::time::Duration::from_millis(250), &mut terminal)
        .await
        .expect("waiting publication failure must reach a terminal row");
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Failed);
    assert_eq!(
        row.failure_code,
        Some(ContinuationFailureCode::StateConflict)
    );
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_post_ack_parent_identity_drift_is_not_ownerless() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ParentIdentityDriftPort {
            snapshots: AtomicUsize::new(0),
        }),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    completion.await.unwrap().unwrap();
    complete_seeded_task(&broker, "task-running").await;
    tokio::time::timeout(std::time::Duration::from_millis(250), &mut terminal)
        .await
        .expect("parent identity drift must reach a terminal row");
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Failed);
    assert_eq!(
        row.failure_code,
        Some(ContinuationFailureCode::StateConflict)
    );
    assert_eq!(coordinator.worker_count(), 0);
}

async fn assert_suspension_cleanup_cause_stays_owned(cause: SuspensionFailureCause) {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(SuspensionFailurePort(cause)),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    let error = completion.await.unwrap().unwrap_err();
    match cause {
        SuspensionFailureCause::DrainTimeout => {
            assert!(matches!(error, ContinuationError::SuspendDrainTimeout));
        }
        SuspensionFailureCause::ParentConnectionLost => {
            assert!(matches!(error, ContinuationError::ParentConnectionLost));
        }
        SuspensionFailureCause::ParentStopRequested => {
            assert!(matches!(error, ContinuationError::ParentStopRequested));
        }
    }
    tokio::task::yield_now().await;

    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Arming);
    assert_eq!(row.failure_code, None);
    assert_eq!(coordinator.worker_count(), 1);

    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_suspend_drain_timeout_stays_owned_for_task8_cleanup() {
    assert_suspension_cleanup_cause_stays_owned(SuspensionFailureCause::DrainTimeout).await;
}

#[tokio::test]
async fn continuation_coordinator_parent_connection_loss_stays_owned_for_task8_cleanup() {
    assert_suspension_cleanup_cause_stays_owned(SuspensionFailureCause::ParentConnectionLost).await;
}

#[tokio::test]
async fn continuation_coordinator_parent_stop_rejection_stays_owned_for_task8_cleanup() {
    assert_suspension_cleanup_cause_stays_owned(SuspensionFailureCause::ParentStopRequested).await;
}

#[tokio::test]
async fn continuation_coordinator_local_suspend_rejection_uses_pre_suspension_owner() {
    struct LocalRejectionPort;

    #[async_trait]
    impl ParentContinuationPort for LocalRejectionPort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            ReadyPort.snapshot_parent(connection_id).await
        }

        async fn suspend_parent(
            &self,
            _request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            Err(ContinuationError::SuspendRejected(
                "suspend_turn_generation_mismatch".into(),
            ))
        }

        async fn admit_continuation(
            &self,
            request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            ReadyPort.admit_continuation(request).await
        }

        async fn publish_waiting(
            &self,
            connection_id: &str,
            waiting: Option<ContinuationWaitingProjection>,
        ) -> Result<(), ContinuationError> {
            ReadyPort.publish_waiting(connection_id, waiting).await
        }

        async fn publish_failure(
            &self,
            connection_id: &str,
            code: ContinuationFailureCode,
        ) -> Result<(), ContinuationError> {
            ReadyPort.publish_failure(connection_id, code).await
        }
    }

    let task_store = Arc::new(MockTaskStore::with_running("task-running", 99));
    let broker =
        Arc::new(test_broker().with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>));
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(LocalRejectionPort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    assert!(matches!(
        completion.await.unwrap(),
        Err(ContinuationError::SuspendRejected(_))
    ));
    terminal.await;

    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Failed);
    assert_eq!(row.failure_code, Some(ContinuationFailureCode::ArmFailed));
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(
        task_store.persisted("task-running").await.status,
        TaskStatus::Running
    );
    assert_eq!(coordinator.worker_count(), 0);
}

async fn assert_pre_suspension_failure_persistence_retains_owner(store_error: bool) {
    let task_store = Arc::new(MockTaskStore::with_running("task-running", 99));
    let broker =
        Arc::new(test_broker().with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>));
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    if store_error {
        store.error_next_transition_to(ContinuationState::Failed);
    } else {
        store.fail_next_transition_to(ContinuationState::Failed);
    }
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(PreSuspendDispatchFailurePort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    assert!(matches!(
        completion.await.unwrap(),
        Err(ContinuationError::SuspendDispatch(AcpError::ProcessExited))
    ));

    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Arming);
    assert_eq!(row.failure_code, None);
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(
        task_store.persisted("task-running").await.status,
        TaskStatus::Running
    );
    assert_eq!(coordinator.worker_count(), 1);

    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_pre_suspension_active_cas_loser_retains_owner() {
    assert_pre_suspension_failure_persistence_retains_owner(false).await;
}

#[tokio::test]
async fn continuation_coordinator_pre_suspension_store_error_retains_owner() {
    assert_pre_suspension_failure_persistence_retains_owner(true).await;
}

#[tokio::test]
async fn continuation_coordinator_stale_generation_and_version_cannot_wake_newer_row() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let (port, suspend_started, _suspend_release, _admission_started) = GatedPort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    suspend_started.await.expect("suspension dispatched");
    let owned = store.load(&continuation_id).await.unwrap().unwrap();
    let newer = store
        .cas_transition(
            &owned.continuation_id,
            owned.generation,
            owned.version,
            ContinuationState::Arming,
            ContinuationPatch {
                state: ContinuationState::Arming,
                wake_reason: super::store::FieldPatch::Keep,
                suspend_requested_at: super::store::FieldPatch::Set(Utc::now()),
                suspended_at: super::store::FieldPatch::Keep,
                wake_claimed_at: super::store::FieldPatch::Keep,
                prompt_admitted_at: super::store::FieldPatch::Keep,
                finished_at: super::store::FieldPatch::Keep,
                failure_code: super::store::FieldPatch::Keep,
            },
        )
        .await
        .unwrap()
        .unwrap();
    complete_seeded_task(&broker, "task-running").await;
    assert!(matches!(
        completion.await.unwrap(),
        Err(ContinuationError::StateConflict)
    ));
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.version, newer.version);
    assert_eq!(row.state, ContinuationState::Arming);
    assert_eq!(coordinator.worker_count(), 1);
    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test]
async fn continuation_coordinator_stale_generation_worker_cannot_drain_newer_row() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let (port, suspend_started, suspend_release, _admission_started) = GatedPort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );
    let first = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id: first_id,
        completion: first_completion,
    } = first
    else {
        panic!("must arm first generation")
    };
    suspend_started.await.expect("first suspension dispatched");
    tokio::task::yield_now().await;
    let first_row = store.load(&first_id).await.unwrap().unwrap();
    let cancelled = store
        .cas_transition(
            &first_row.continuation_id,
            first_row.generation,
            first_row.version,
            ContinuationState::Arming,
            ContinuationPatch {
                state: ContinuationState::Cancelled,
                wake_reason: super::store::FieldPatch::Keep,
                suspend_requested_at: super::store::FieldPatch::Keep,
                suspended_at: super::store::FieldPatch::Keep,
                wake_claimed_at: super::store::FieldPatch::Keep,
                prompt_admitted_at: super::store::FieldPatch::Keep,
                finished_at: super::store::FieldPatch::Set(Utc::now()),
                failure_code: super::store::FieldPatch::Keep,
            },
        )
        .await
        .unwrap()
        .unwrap();

    let second = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id: second_id,
        completion: second_completion,
    } = second
    else {
        panic!("must arm second generation")
    };
    second_completion.await.unwrap().unwrap();
    let second_waiting = store.load(&second_id).await.unwrap().unwrap();
    assert!(second_waiting.generation > cancelled.generation);
    assert_eq!(second_waiting.state, ContinuationState::Waiting);

    suspend_release.send(()).unwrap();
    assert!(matches!(
        first_completion.await.unwrap(),
        Err(ContinuationError::StateConflict)
    ));
    tokio::task::yield_now().await;
    let second_after_stale_exit = store.load(&second_id).await.unwrap().unwrap();
    assert_eq!(second_after_stale_exit.state, ContinuationState::Waiting);
    assert_eq!(second_after_stale_exit.version, second_waiting.version);
    assert_eq!(coordinator.worker_count(), 1);

    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_prompt_delivery_retries_exact_schedule() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake_pending) = ObservedStore::new();
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let (port, mut attempts) = RetryPort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port.clone(),
        Arc::new(SystemContinuationClock::new()),
    );

    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("running Join must arm a continuation")
    };
    completion.await.unwrap().unwrap();
    complete_seeded_task(&broker, "task-running").await;

    let first = attempts.recv().await.expect("initial attempt");
    for (delay_ms, expected_total_ms) in [(100, 100), (500, 600), (2_000, 2_600)] {
        tokio::time::advance(std::time::Duration::from_millis(delay_ms - 1)).await;
        assert!(matches!(
            attempts.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        let admitted_at = tokio::select! {
            biased;
            attempt = attempts.recv() => attempt.expect("scheduled retry"),
            _ = &mut terminal => {
                let row = store.load(&continuation_id).await.unwrap();
                panic!(
                    "continuation terminated before all retries: attempts={}, row={row:?}",
                    port.attempts.load(Ordering::Relaxed),
                )
            },
        };
        assert_eq!(
            admitted_at.signed_duration_since(first).num_milliseconds(),
            expected_total_ms
        );
    }
    terminal.await;
    assert_eq!(port.attempts.load(Ordering::Relaxed), 4);
    assert_eq!(coordinator.worker_count(), 0);
}

async fn seed_resuming_continuation(store: &InMemoryContinuationStore) -> ContinuationRecord {
    let now = Utc::now();
    let armed = store
        .insert_arming(NewContinuation {
            continuation_id: "continuation-admission".into(),
            parent_conversation_id: 7,
            parent_session_id: "session-7".into(),
            parent_connection_id: "parent".into(),
            parent_turn_generation: 3,
            task_ids: super::types::ContinuationTaskIds(vec!["task".into()]),
            armed_at: now,
            wake_at: now + ChronoDuration::seconds(240),
            internal_prompt_id: "prompt-admission".into(),
            internal_prompt_marker: internal_prompt_marker(
                "continuation-admission",
                "prompt-admission",
            ),
        })
        .await
        .unwrap();
    let mut wake = ContinuationPatch {
        state: ContinuationState::WakePending,
        wake_reason: super::store::FieldPatch::Set(ContinuationWakeReason::AllTerminal),
        suspend_requested_at: super::store::FieldPatch::Set(now),
        suspended_at: super::store::FieldPatch::Set(now),
        wake_claimed_at: super::store::FieldPatch::Set(now),
        prompt_admitted_at: super::store::FieldPatch::Keep,
        finished_at: super::store::FieldPatch::Keep,
        failure_code: super::store::FieldPatch::Keep,
    };
    wake.suspend_requested_at = super::store::FieldPatch::Keep;
    let wake = store
        .cas_transition(
            &armed.continuation_id,
            armed.generation,
            armed.version,
            ContinuationState::Arming,
            wake,
        )
        .await
        .unwrap()
        .unwrap();
    store
        .cas_transition(
            &wake.continuation_id,
            wake.generation,
            wake.version,
            ContinuationState::WakePending,
            ContinuationPatch {
                state: ContinuationState::Resuming,
                wake_reason: super::store::FieldPatch::Keep,
                suspend_requested_at: super::store::FieldPatch::Keep,
                suspended_at: super::store::FieldPatch::Keep,
                wake_claimed_at: super::store::FieldPatch::Keep,
                prompt_admitted_at: super::store::FieldPatch::Keep,
                finished_at: super::store::FieldPatch::Keep,
                failure_code: super::store::FieldPatch::Keep,
            },
        )
        .await
        .unwrap()
        .unwrap()
}

fn admission_request(record: &ContinuationRecord) -> ContinuationPromptRequest {
    ContinuationPromptRequest {
        parent_connection_id: "parent".into(),
        parent_conversation_id: 7,
        parent_session_id: "session-7".into(),
        suspended_turn_generation: 3,
        continuation_generation: record.generation,
        expected_version: record.version,
        admitted_at: Utc::now(),
        origin: super::DelegationContinuationOrigin::new(
            record.continuation_id.clone(),
            record.generation,
            ContinuationWakeReason::AllTerminal,
            record.internal_prompt_id.clone(),
            record.internal_prompt_marker.clone(),
        ),
        snapshot: crate::acp::delegation::types::DelegationStatusBatch::legacy(vec![]),
    }
}

#[tokio::test]
async fn continuation_coordinator_manager_ack_loss_marker_is_crash_safe() {
    let manager = Arc::new(crate::acp::manager::ConnectionManager::new());
    let store = Arc::new(InMemoryContinuationStore::default());
    manager.install_continuation_store(store.clone() as Arc<dyn ContinuationStore>);
    let mut command_rx = manager
        .insert_test_connection_live("parent", AgentType::ClaudeCode, None, EventEmitter::Noop)
        .await;
    let state = manager.get_state("parent").await.unwrap();
    {
        let mut state = state.write().await;
        state.conversation_id = Some(7);
        state.external_id = Some("session-7".into());
        state.parent_turn_generation = 3;
        state.last_suspended_turn_generation = Some(3);
    }
    let record = seed_resuming_continuation(&store).await;
    let marker = record.internal_prompt_marker.clone();

    let (caller_release, caller_hold) = tokio::sync::oneshot::channel::<()>();
    let caller = tokio::spawn({
        let manager = manager.clone();
        let request = admission_request(&record);
        async move {
            let admission = manager.admit_delegation_continuation(request).await;
            drop(manager);
            let _ = caller_hold.await;
            admission
        }
    });
    let command = command_rx.recv().await.expect("continuation command");
    assert!(
        !caller.is_finished(),
        "the admission caller must remain abortable after enqueue"
    );
    let durable_at_dequeue = store.matches_admitted_marker(7, &marker).await.unwrap();
    let prompt_text = match command {
        crate::acp::connection::ConnectionCommand::Prompt {
            blocks,
            user_message,
            mark_awaiting_reply,
            ..
        } => {
            assert!(user_message.is_none());
            assert!(mark_awaiting_reply);
            match blocks.into_iter().next().unwrap() {
                crate::acp::types::PromptInputBlock::Text { text } => text,
                _ => panic!("continuation prompt must be text"),
            }
        }
        _ => panic!("expected continuation prompt"),
    };
    assert!(durable_at_dequeue);

    caller.abort();
    drop(caller);
    drop(caller_release);
    drop(state);
    drop(command_rx);
    drop(manager);

    let cold_row = store.load(&record.continuation_id).await.unwrap().unwrap();
    assert!(cold_row.prompt_admitted_at.is_some());
    let mut turns = vec![MessageTurn {
        id: "internal".into(),
        role: TurnRole::User,
        blocks: vec![ContentBlock::Text { text: prompt_text }],
        timestamp: Utc::now(),
        usage: None,
        duration_ms: None,
        model: None,
        completed_at: None,
    }];
    filter_internal_continuation_turns(store.as_ref(), 7, &mut turns)
        .await
        .unwrap();
    assert!(turns.is_empty());
}

#[tokio::test]
async fn continuation_coordinator_manager_admission_replay_is_idempotent() {
    let manager = crate::acp::manager::ConnectionManager::new();
    let store = Arc::new(InMemoryContinuationStore::default());
    manager.install_continuation_store(store.clone() as Arc<dyn ContinuationStore>);
    let mut command_rx = manager
        .insert_test_connection_live("parent", AgentType::ClaudeCode, None, EventEmitter::Noop)
        .await;
    let state = manager.get_state("parent").await.unwrap();
    {
        let mut state = state.write().await;
        state.conversation_id = Some(7);
        state.external_id = Some("session-7".into());
        state.parent_turn_generation = 3;
        state.last_suspended_turn_generation = Some(3);
    }
    let record = seed_resuming_continuation(&store).await;

    let first = manager
        .admit_delegation_continuation(admission_request(&record))
        .await
        .unwrap();
    assert_eq!(first, PromptAdmissionResult::Admitted);
    let _ = command_rx.recv().await.expect("continuation command");
    let admitted = store.load(&record.continuation_id).await.unwrap().unwrap();
    let replay = manager
        .admit_delegation_continuation(admission_request(&admitted))
        .await
        .unwrap();
    assert_eq!(replay, PromptAdmissionResult::AlreadyAdmitted);
    assert!(matches!(
        command_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn continuation_coordinator_manager_admission_rejects_identity_drift_without_side_effects() {
    let manager = crate::acp::manager::ConnectionManager::new();
    let store = Arc::new(InMemoryContinuationStore::default());
    manager.install_continuation_store(store.clone() as Arc<dyn ContinuationStore>);
    let mut command_rx = manager
        .insert_test_connection_live("parent", AgentType::ClaudeCode, None, EventEmitter::Noop)
        .await;
    let state = manager.get_state("parent").await.unwrap();
    {
        let mut state = state.write().await;
        state.conversation_id = Some(7);
        state.external_id = Some("session-7".into());
        state.parent_turn_generation = 3;
        state.last_suspended_turn_generation = Some(3);
    }
    let record = seed_resuming_continuation(&store).await;

    let mut wrong_connection = admission_request(&record);
    wrong_connection.parent_connection_id = "other-parent".into();
    assert!(manager
        .admit_delegation_continuation(wrong_connection)
        .await
        .is_err());

    let mut wrong_conversation = admission_request(&record);
    wrong_conversation.parent_conversation_id = 8;
    assert!(manager
        .admit_delegation_continuation(wrong_conversation)
        .await
        .is_err());

    let mut wrong_session = admission_request(&record);
    wrong_session.parent_session_id = "other-session".into();
    assert!(manager
        .admit_delegation_continuation(wrong_session)
        .await
        .is_err());

    let mut wrong_suspended_generation = admission_request(&record);
    wrong_suspended_generation.suspended_turn_generation += 1;
    assert!(manager
        .admit_delegation_continuation(wrong_suspended_generation)
        .await
        .is_err());

    let mut wrong_generation = admission_request(&record);
    wrong_generation.continuation_generation += 1;
    assert!(manager
        .admit_delegation_continuation(wrong_generation)
        .await
        .is_err());

    let mut wrong_continuation = admission_request(&record);
    wrong_continuation.origin = super::DelegationContinuationOrigin::new(
        "other-continuation".into(),
        record.generation,
        ContinuationWakeReason::AllTerminal,
        record.internal_prompt_id.clone(),
        record.internal_prompt_marker.clone(),
    );
    assert!(manager
        .admit_delegation_continuation(wrong_continuation)
        .await
        .is_err());

    let mut wrong_origin_generation = admission_request(&record);
    wrong_origin_generation.origin = super::DelegationContinuationOrigin::new(
        record.continuation_id.clone(),
        record.generation + 1,
        ContinuationWakeReason::AllTerminal,
        record.internal_prompt_id.clone(),
        record.internal_prompt_marker.clone(),
    );
    assert!(manager
        .admit_delegation_continuation(wrong_origin_generation)
        .await
        .is_err());

    let mut wrong_internal_prompt = admission_request(&record);
    wrong_internal_prompt.origin = super::DelegationContinuationOrigin::new(
        record.continuation_id.clone(),
        record.generation,
        ContinuationWakeReason::AllTerminal,
        "other-prompt".into(),
        record.internal_prompt_marker.clone(),
    );
    assert!(manager
        .admit_delegation_continuation(wrong_internal_prompt)
        .await
        .is_err());

    let mut wrong_marker = admission_request(&record);
    wrong_marker.origin = super::DelegationContinuationOrigin::new(
        record.continuation_id.clone(),
        record.generation,
        ContinuationWakeReason::AllTerminal,
        record.internal_prompt_id.clone(),
        "other-marker".into(),
    );
    assert!(manager
        .admit_delegation_continuation(wrong_marker)
        .await
        .is_err());

    let mut wrong_wake_reason = admission_request(&record);
    wrong_wake_reason.origin = super::DelegationContinuationOrigin::new(
        record.continuation_id.clone(),
        record.generation,
        ContinuationWakeReason::Checkpoint,
        record.internal_prompt_id.clone(),
        record.internal_prompt_marker.clone(),
    );
    assert!(manager
        .admit_delegation_continuation(wrong_wake_reason)
        .await
        .is_err());

    let mut wrong_version = admission_request(&record);
    wrong_version.expected_version += 1;
    assert!(manager
        .admit_delegation_continuation(wrong_version)
        .await
        .is_err());

    state.write().await.parent_turn_generation = 4;
    assert!(manager
        .admit_delegation_continuation(admission_request(&record))
        .await
        .is_err());

    let durable = store.load(&record.continuation_id).await.unwrap().unwrap();
    assert!(durable.prompt_admitted_at.is_none());
    let state = state.read().await;
    assert_eq!(state.parent_turn_generation, 4);
    assert!(state.active_turn_generation.is_none());
    assert!(state.last_internal_prompt_admission.is_none());
    assert!(!state.turn_in_flight);
    drop(state);
    assert!(matches!(
        command_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_stop_cancels_worker_during_retry() {
    let broker = Arc::new(test_broker());
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake) = ObservedStore::new();
    let (port, mut attempts) = RetryPort::always_fail();
    let coordinator = DelegationContinuationCoordinator::new(
        store as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming { completion, .. } = outcome else {
        panic!("must arm")
    };
    completion.await.unwrap().unwrap();
    complete_seeded_task(&broker, "task-running").await;
    attempts.recv().await.expect("initial attempt");
    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
    assert_eq!(
        attempts.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    );
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_permanent_failure_drains_children_before_terminal_row() {
    let task_store = Arc::new(MockTaskStore::with_running("task-running", 99));
    let (settle_entered_tx, settle_entered_rx) = tokio::sync::oneshot::channel();
    let (settle_release_tx, settle_release_rx) = tokio::sync::oneshot::channel();
    task_store
        .install_settle_gate(settle_entered_tx, settle_release_rx)
        .await;
    let broker =
        Arc::new(test_broker().with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>));
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake) = ObservedStore::new();
    *store
        .drain_check_broker
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(broker.clone());
    *store
        .drain_check_task
        .lock()
        .unwrap_or_else(|error| error.into_inner()) =
        Some((task_store.clone(), "task-running".into()));
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let (port, _attempts) = RetryPort::always_fail();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker,
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    completion.await.unwrap().unwrap();
    tokio::time::advance(std::time::Duration::from_millis(
        CONTINUATION_CHECKPOINT_MS + 2_600,
    ))
    .await;
    settle_entered_rx
        .await
        .expect("child durable settle must start before continuation terminal CAS");
    assert_eq!(
        store.load(&continuation_id).await.unwrap().unwrap().state,
        ContinuationState::Resuming,
        "continuation must remain active while child durable settle is blocked"
    );
    assert!(!store.drain_verified.load(Ordering::Relaxed));
    settle_release_tx.send(()).unwrap();
    terminal.await;
    assert!(store.drain_verified.load(Ordering::Relaxed));
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Failed);
    assert_eq!(
        row.failure_code,
        Some(ContinuationFailureCode::PromptDeliveryFailed)
    );
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_stale_failure_worker_keeps_exact_resuming_fence() {
    let task_store = Arc::new(MockTaskStore::with_running("task-running", 99));
    let broker =
        Arc::new(test_broker().with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>));
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake) = ObservedStore::new();
    let (port, final_attempt, final_release) = FinalFailureGatePort::new();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        port,
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    completion.await.unwrap().unwrap();
    tokio::time::advance(std::time::Duration::from_millis(
        CONTINUATION_CHECKPOINT_MS + 2_600,
    ))
    .await;
    let request = final_attempt
        .await
        .expect("final admission attempt entered");

    let owned = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(owned.state, ContinuationState::Resuming);
    assert_eq!(request.expected_version, owned.version);
    let newer = store
        .cas_transition(
            &owned.continuation_id,
            owned.generation,
            owned.version,
            ContinuationState::Resuming,
            ContinuationPatch {
                state: ContinuationState::Resuming,
                wake_reason: super::store::FieldPatch::Keep,
                suspend_requested_at: super::store::FieldPatch::Keep,
                suspended_at: super::store::FieldPatch::Keep,
                wake_claimed_at: super::store::FieldPatch::Keep,
                prompt_admitted_at: super::store::FieldPatch::Set(Utc::now()),
                finished_at: super::store::FieldPatch::Keep,
                failure_code: super::store::FieldPatch::Keep,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(newer.version > owned.version);

    let cleanup_claim_attempted = store.cleanup_claim_attempted.notified();
    tokio::pin!(cleanup_claim_attempted);
    cleanup_claim_attempted.as_mut().enable();
    let failure_attempted = store.failure_attempted.notified();
    tokio::pin!(failure_attempted);
    failure_attempted.as_mut().enable();
    final_release.send(()).unwrap();
    tokio::select! {
        _ = &mut cleanup_claim_attempted => {}
        _ = &mut failure_attempted => {
            panic!("stale worker drained children before acquiring an exact cleanup claim")
        }
    }

    assert_eq!(
        *store
            .cleanup_claim_fence
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        Some((owned.generation, owned.version, ContinuationState::Resuming)),
        "the stale worker must claim the exact Resuming attempt verified before drain"
    );
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(
        task_store.persisted("task-running").await.status,
        TaskStatus::Running
    );
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Resuming);
    assert_eq!(row.version, newer.version);
    assert_eq!(coordinator.worker_count(), 1);

    assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
    tokio::task::yield_now().await;
    assert_eq!(coordinator.worker_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn continuation_coordinator_state_conflict_drains_children_with_distinct_failure_code() {
    let task_store = Arc::new(MockTaskStore::with_running("task-running", 99));
    let broker =
        Arc::new(test_broker().with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>));
    broker
        .seed_live_task_for_test("parent", "task-running")
        .await;
    let (store, _wake) = ObservedStore::new();
    let terminal = store.terminal.notified();
    tokio::pin!(terminal);
    terminal.as_mut().enable();
    let coordinator = DelegationContinuationCoordinator::new(
        store.clone() as Arc<dyn ContinuationStore>,
        broker.clone(),
        Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        Arc::new(ConflictAdmissionPort),
        Arc::new(SystemContinuationClock::new()),
    );
    let outcome = coordinator
        .begin_arm_from_join(JoinArmRequest {
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            task_ids: vec!["task-running".into()],
            waiter_closed: CancellationToken::new(),
        })
        .await
        .unwrap();
    let super::coordinator::JoinArmOutcome::Arming {
        continuation_id,
        completion,
    } = outcome
    else {
        panic!("must arm")
    };
    completion.await.unwrap().unwrap();

    tokio::time::advance(std::time::Duration::from_millis(CONTINUATION_CHECKPOINT_MS)).await;
    terminal.await;
    let row = store.load(&continuation_id).await.unwrap().unwrap();
    assert_eq!(row.state, ContinuationState::Failed);
    assert_eq!(
        row.failure_code,
        Some(ContinuationFailureCode::StateConflict)
    );
    assert_eq!(broker.pending_count().await, 0);
    let task = task_store.persisted("task-running").await;
    assert_eq!(task.status, TaskStatus::Canceled);
    assert_eq!(task.error_code.as_deref(), Some("parent_turn_failed"));
    assert_eq!(coordinator.worker_count(), 0);
}

fn waiting_projection(conversation_id: i32) -> ContinuationWaitingProjection {
    let armed_at = Utc::now();
    ContinuationWaitingProjection {
        conversation_id,
        state: ContinuationState::Waiting,
        generation: 4,
        armed_at,
        wake_at: armed_at + ChronoDuration::milliseconds(CONTINUATION_CHECKPOINT_MS as i64),
    }
}

fn session_for_conversation(conversation_id: i32) -> SessionState {
    let mut state = SessionState::new(
        "parent".into(),
        AgentType::ClaudeCode,
        None,
        "test".into(),
        None,
    );
    state.conversation_id = Some(conversation_id);
    state
}

#[tokio::test]
async fn continuation_waiting_manager_port_updates_matching_session_and_rejects_mismatch() {
    let manager = Arc::new(crate::acp::manager::ConnectionManager::new());
    manager
        .insert_test_connection("parent", AgentType::ClaudeCode, None, EventEmitter::Noop)
        .await;
    let state = manager.get_state("parent").await.unwrap();
    state.write().await.conversation_id = Some(7);
    let mut events = state.read().await.event_stream().subscribe();
    let port = Arc::new(ManagerContinuationPort::new(manager));
    let waiting = waiting_projection(7);

    port.publish_waiting("parent", Some(waiting.clone()))
        .await
        .unwrap();
    let published = events.recv().await.unwrap();
    assert!(matches!(
        &published.payload,
        AcpEvent::ContinuationWaitingChanged {
            conversation_id: 7,
            waiting: Some(value),
        } if value == &waiting
    ));
    assert_eq!(
        state.read().await.waiting_for_subagents,
        Some(waiting.clone()),
        "matching live state is updated before publication returns"
    );

    let mismatch = port
        .publish_waiting("parent", Some(waiting_projection(8)))
        .await;
    assert!(matches!(
        mismatch,
        Err(ContinuationError::ParentIdentityChanged)
    ));
    assert_eq!(
        state.read().await.waiting_for_subagents,
        Some(waiting.clone())
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    state.write().await.conversation_id = Some(8);
    assert!(matches!(
        port.publish_waiting("parent", None).await,
        Err(ContinuationError::ParentIdentityChanged)
    ));
    assert_eq!(
        state.read().await.waiting_for_subagents,
        Some(waiting.clone()),
        "a connection rebound to another conversation cannot clear the stored projection"
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    state.write().await.conversation_id = Some(7);

    let state_write = state.write().await;
    let clear = port.publish_waiting("parent", None);
    tokio::pin!(clear);
    tokio::select! {
        biased;
        result = &mut clear => {
            panic!("waiting clear must wait for the matching state lock: {result:?}")
        }
        _ = tokio::task::yield_now() => {}
    }
    drop(state_write);
    clear.await.unwrap();
    let cleared = events.recv().await.unwrap();
    assert!(matches!(
        cleared.payload,
        AcpEvent::ContinuationWaitingChanged {
            conversation_id: 7,
            waiting: None,
        }
    ));
    assert_eq!(state.read().await.waiting_for_subagents, None);

    state.write().await.conversation_id = Some(8);
    port.publish_waiting("parent", None).await.unwrap();
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn continuation_coordinator_manager_port_failure_is_redacted_and_terminal_by_code() {
    let manager = Arc::new(crate::acp::manager::ConnectionManager::new());
    manager
        .insert_test_connection(
            "parent-secret",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
    let state = manager.get_state("parent-secret").await.unwrap();
    state.write().await.conversation_id = Some(7);
    let mut events = state.read().await.event_stream().subscribe();
    let port = ManagerContinuationPort::new(manager);

    for (code, expected_terminal) in [
        (ContinuationFailureCode::PromptDeliveryFailed, false),
        (ContinuationFailureCode::StateConflict, false),
        (ContinuationFailureCode::ParentConnectionLost, true),
        (ContinuationFailureCode::SuspendDrainTimeout, true),
    ] {
        port.publish_failure("parent-secret", code).await.unwrap();
        let event = events.recv().await.unwrap();
        let AcpEvent::Error {
            message,
            code: published_code,
            terminal,
            ..
        } = &event.payload
        else {
            panic!("manager port must publish an ACP error event")
        };
        assert_eq!(message, "Delegation continuation failed");
        assert_eq!(published_code.as_deref(), Some(code.as_str()));
        assert_eq!(*terminal, expected_terminal);
        for secret in [
            "parent-secret",
            "continuation-secret",
            "task-secret",
            "marker-secret",
            "prompt-secret",
        ] {
            assert!(!message.contains(secret));
        }
    }
}

#[tokio::test]
async fn continuation_coordinator_manager_port_suspend_rechecks_parent_identity() {
    let manager = Arc::new(crate::acp::manager::ConnectionManager::new());
    manager
        .insert_test_connection("parent", AgentType::ClaudeCode, None, EventEmitter::Noop)
        .await;
    let state = manager.get_state("parent").await.unwrap();
    {
        let mut state = state.write().await;
        state.conversation_id = Some(7);
        state.external_id = Some("session-7".into());
        state.parent_turn_generation = 3;
        state.active_turn_generation = Some(3);
        state.turn_in_flight = true;
    }
    let port = ManagerContinuationPort::new(manager);

    for request in [
        SuspendRequest {
            continuation_id: "continuation".into(),
            parent_connection_id: "parent".into(),
            parent_conversation_id: 8,
            parent_session_id: "session-7".into(),
            parent_turn_generation: 3,
        },
        SuspendRequest {
            continuation_id: "continuation".into(),
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            parent_session_id: "other-session".into(),
            parent_turn_generation: 3,
        },
        SuspendRequest {
            continuation_id: "continuation".into(),
            parent_connection_id: "parent".into(),
            parent_conversation_id: 7,
            parent_session_id: "session-7".into(),
            parent_turn_generation: 4,
        },
    ] {
        assert!(matches!(
            port.suspend_parent(request).await,
            Err(ContinuationError::ParentIdentityChanged)
        ));
    }
}

#[tokio::test]
async fn continuation_coordinator_suspension_dispatch_preserves_installation_stage_causes() {
    let (closed_tx, closed_rx, _closed_liveness) = crate::acp::connection::connection_channel(1);
    drop(closed_rx);
    assert!(matches!(
        dispatch_suspension_control(closed_tx, "continuation", 3).await,
        Err(ContinuationError::SuspendDispatch(AcpError::ProcessExited))
    ));

    for (reply_error, expected_code) in [
        (
            Some(AcpError::protocol("suspend_no_active_turn")),
            "pre_suspension_rejected",
        ),
        (
            Some(AcpError::protocol("suspend_turn_generation_mismatch")),
            "pre_suspension_rejected",
        ),
        (
            Some(AcpError::protocol("suspend_already_pending")),
            "pre_suspension_rejected",
        ),
        (
            Some(AcpError::protocol("suspend_session_fence_mismatch")),
            "pre_suspension_rejected",
        ),
        (
            Some(AcpError::protocol("suspend_turn_ended_before_cancel")),
            "pre_suspension_rejected",
        ),
        (
            Some(AcpError::protocol("suspend_drain_timeout")),
            "suspend_drain_timeout",
        ),
        (
            Some(AcpError::protocol("suspend_parent_disconnected")),
            "parent_connection_lost",
        ),
        (
            Some(AcpError::protocol("suspend_prompt_response_failed")),
            "parent_connection_lost",
        ),
        (
            Some(AcpError::protocol("suspend_cancelled_by_user")),
            "parent_stop_requested",
        ),
        (
            Some(AcpError::protocol("future_unknown_suspension_rejection")),
            "parent_connection_lost",
        ),
        (Some(AcpError::ProcessExited), "parent_connection_lost"),
        (None, "parent_connection_lost"),
    ] {
        let (control_tx, mut control_rx, _control_liveness) =
            crate::acp::connection::connection_channel(1);
        let dispatch = dispatch_suspension_control(control_tx, "continuation", 3);
        tokio::pin!(dispatch);
        let control = tokio::select! {
            control = control_rx.recv() => control.expect("suspension control dispatched"),
            result = &mut dispatch => panic!("dispatch ended before control receipt: {result:?}"),
        };
        let ConnectionControl::SuspendForDelegation { reply, .. } = control else {
            panic!("expected suspension control")
        };
        if let Some(error) = reply_error {
            reply.send(Err(error)).unwrap();
        } else {
            drop(reply);
        }
        let error = dispatch.await.unwrap_err();
        assert_eq!(
            match error {
                ContinuationError::SuspendDrainTimeout => "suspend_drain_timeout",
                ContinuationError::ParentConnectionLost => "parent_connection_lost",
                ContinuationError::ParentStopRequested => "parent_stop_requested",
                ContinuationError::SuspendRejected(_) => "pre_suspension_rejected",
                other => panic!("unexpected mapped suspension error: {other:?}"),
            },
            expected_code
        );
    }
}

#[test]
fn continuation_waiting_event_round_trips_snapshot() {
    let mut state = session_for_conversation(7);
    let waiting = waiting_projection(7);

    state.apply_event(&AcpEvent::ContinuationWaitingChanged {
        conversation_id: 7,
        waiting: Some(waiting.clone()),
    });

    assert_eq!(state.to_snapshot().waiting_for_subagents, Some(waiting));
}

#[test]
fn continuation_waiting_event_ignores_other_conversation() {
    let mut state = session_for_conversation(7);

    state.apply_event(&AcpEvent::ContinuationWaitingChanged {
        conversation_id: 8,
        waiting: Some(waiting_projection(8)),
    });

    assert_eq!(state.to_snapshot().waiting_for_subagents, None);
}

#[test]
fn continuation_waiting_terminal_event_clears_snapshot() {
    let mut state = session_for_conversation(7);
    state.apply_event(&AcpEvent::ContinuationWaitingChanged {
        conversation_id: 7,
        waiting: Some(waiting_projection(7)),
    });

    state.apply_event(&AcpEvent::ContinuationWaitingChanged {
        conversation_id: 7,
        waiting: None,
    });

    assert_eq!(state.to_snapshot().waiting_for_subagents, None);
}
