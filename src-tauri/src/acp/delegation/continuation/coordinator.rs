use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::prompt::{internal_prompt_marker, DelegationContinuationOrigin};
use super::store::{
    ContStoreError, ContinuationPatch, ContinuationRecord, ContinuationStore, FieldPatch,
    NewContinuation,
};
use super::types::{
    ContinuationFailureCode, ContinuationState, ContinuationTaskIds, ContinuationWaitingProjection,
    ContinuationWakeReason, CONTINUATION_CHECKPOINT_MS,
};
use crate::acp::connection::SuspensionAck;
use crate::acp::delegation::broker::{DelegationBroker, JoinEvaluation};
use crate::acp::delegation::metrics::DelegationMetrics;
use crate::acp::delegation::types::{
    DelegationStatusBatch, DelegationWakeReason, ParentTurnEndReason,
};
use crate::acp::error::AcpError;
use crate::acp::manager::ConnectionManager;
use crate::acp::types::AcpEvent;
use crate::web::event_bridge::emit_with_state;

#[allow(
    dead_code,
    reason = "Task 7 activates the coordinator runtime entry point"
)]
pub(crate) trait ContinuationClock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    fn sleep_until(&self, deadline: DateTime<Utc>)
        -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

#[allow(dead_code, reason = "Task 7 activates logical coordinator time")]
pub(crate) struct SystemContinuationClock {
    base_utc: DateTime<Utc>,
    base_instant: tokio::time::Instant,
}

impl SystemContinuationClock {
    pub(crate) fn new() -> Self {
        Self {
            base_utc: Utc::now(),
            base_instant: tokio::time::Instant::now(),
        }
    }
}

impl Default for SystemContinuationClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ContinuationClock for SystemContinuationClock {
    fn now_utc(&self) -> DateTime<Utc> {
        let elapsed = self.base_instant.elapsed();
        self.base_utc + chrono::Duration::from_std(elapsed).unwrap_or(chrono::Duration::MAX)
    }

    fn sleep_until(
        &self,
        deadline: DateTime<Utc>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let delay = deadline
            .signed_duration_since(self.now_utc())
            .to_std()
            .unwrap_or_default();
        Box::pin(tokio::time::sleep(delay))
    }
}

#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "Task 7 passes this through the Join coordinator entry"
)]
pub(crate) struct ParentTurnSnapshot {
    pub connection_id: String,
    pub conversation_id: i32,
    pub session_id: String,
    pub turn_generation: u64,
    pub turn_in_flight: bool,
}

#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "Task 7 dispatches this through the coordinator worker"
)]
pub(crate) struct SuspendRequest {
    pub continuation_id: String,
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    pub parent_session_id: String,
    pub parent_turn_generation: u64,
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "Task 7 dispatches this through the coordinator worker"
)]
pub(crate) struct ContinuationPromptRequest {
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    pub parent_session_id: String,
    pub suspended_turn_generation: u64,
    pub continuation_generation: u64,
    pub expected_version: u64,
    pub admitted_at: DateTime<Utc>,
    pub origin: DelegationContinuationOrigin,
    pub snapshot: DelegationStatusBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "Task 7 consumes coordinator prompt admission results"
)]
pub(crate) enum PromptAdmissionResult {
    Admitted,
    AlreadyAdmitted,
}

#[async_trait]
#[allow(dead_code, reason = "Task 7 activates the coordinator parent port")]
pub(crate) trait ParentContinuationPort: Send + Sync {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError>;
    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError>;
    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError>;
    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError>;
    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError>;
}

#[allow(dead_code, reason = "Task 7 activates the manager-backed parent port")]
pub(crate) struct ManagerContinuationPort {
    manager: Arc<ConnectionManager>,
}

impl ManagerContinuationPort {
    pub(crate) fn new(manager: Arc<ConnectionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ParentContinuationPort for ManagerContinuationPort {
    async fn snapshot_parent(
        &self,
        connection_id: &str,
    ) -> Result<ParentTurnSnapshot, ContinuationError> {
        let state = self
            .manager
            .get_state(connection_id)
            .await
            .ok_or(ContinuationError::ParentUnavailable)?;
        let state = state.read().await;
        Ok(ParentTurnSnapshot {
            connection_id: state.connection_id.clone(),
            conversation_id: state
                .conversation_id
                .ok_or(ContinuationError::ParentUnavailable)?,
            session_id: state
                .external_id
                .clone()
                .ok_or(ContinuationError::ParentUnavailable)?,
            turn_generation: state
                .active_turn_generation
                .unwrap_or(state.parent_turn_generation),
            turn_in_flight: state.turn_in_flight,
        })
    }

    async fn suspend_parent(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionAck, ContinuationError> {
        let parent = self.snapshot_parent(&request.parent_connection_id).await?;
        if parent.conversation_id != request.parent_conversation_id
            || parent.session_id != request.parent_session_id
            || parent.turn_generation != request.parent_turn_generation
            || !parent.turn_in_flight
        {
            return Err(ContinuationError::ParentIdentityChanged);
        }
        self.manager
            .suspend_for_delegation(
                &request.parent_connection_id,
                request.continuation_id,
                request.parent_turn_generation,
            )
            .await
            .map_err(ContinuationError::SuspendDispatch)
    }

    async fn admit_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        self.manager.admit_delegation_continuation(request).await
    }

    async fn publish_waiting(
        &self,
        connection_id: &str,
        waiting: Option<ContinuationWaitingProjection>,
    ) -> Result<(), ContinuationError> {
        let (state, emitter) = self
            .manager
            .get_state_and_emitter(connection_id)
            .await
            .ok_or(ContinuationError::ParentUnavailable)?;
        let conversation_id = {
            let mut live = state.write().await;
            let conversation_id = waiting
                .as_ref()
                .map(|projection| projection.conversation_id)
                .or(live.conversation_id)
                .ok_or(ContinuationError::ParentIdentityChanged)?;
            if live.conversation_id != Some(conversation_id) {
                return Err(ContinuationError::ParentIdentityChanged);
            }
            live.waiting_for_subagents = waiting.clone();
            conversation_id
        };
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::ContinuationWaitingChanged {
                conversation_id,
                waiting,
            },
        )
        .await;
        Ok(())
    }

    async fn publish_failure(
        &self,
        connection_id: &str,
        code: ContinuationFailureCode,
    ) -> Result<(), ContinuationError> {
        let (state, emitter) = self
            .manager
            .get_state_and_emitter(connection_id)
            .await
            .ok_or(ContinuationError::ParentUnavailable)?;
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::Error {
                message: "Delegation continuation failed".to_string(),
                agent_type: "codeg".to_string(),
                code: Some(code.as_str().to_string()),
                terminal: matches!(
                    code,
                    ContinuationFailureCode::ParentConnectionLost
                        | ContinuationFailureCode::SuspendDrainTimeout
                ),
            },
        )
        .await;
        Ok(())
    }
}

#[allow(dead_code, reason = "Task 7 consumes Join arm outcomes")]
pub(crate) enum JoinArmOutcome {
    Immediate(DelegationStatusBatch),
    Arming {
        continuation_id: String,
        completion: oneshot::Receiver<Result<SuspensionAck, ContinuationError>>,
    },
}

#[allow(dead_code, reason = "Task 7 constructs Join arm requests")]
pub(crate) struct JoinArmRequest {
    pub parent_connection_id: String,
    pub parent_conversation_id: i32,
    pub task_ids: Vec<String>,
    pub waiter_closed: CancellationToken,
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code, reason = "Task 7 and Task 8 consume coordinator errors")]
pub(crate) enum ContinuationError {
    #[error(transparent)]
    Store(#[from] ContStoreError),
    #[error("parent connection is unavailable")]
    ParentUnavailable,
    #[error("parent connection identity changed")]
    ParentIdentityChanged,
    #[error("parent suspension dispatch failed: {0}")]
    SuspendDispatch(#[source] AcpError),
    #[error("parent suspension did not drain before timeout")]
    SuspendDrainTimeout,
    #[error("parent connection was lost")]
    ParentConnectionLost,
    #[error("continuation prompt delivery failed: {0}")]
    PromptDelivery(#[source] AcpError),
    #[error("continuation state changed before this operation committed")]
    StateConflict,
    #[error("status waiter closed before continuation persistence")]
    WaiterClosedBeforeInsert,
    #[error("continuation arm worker ended before reporting suspension")]
    ArmWorkerDropped,
}

#[allow(dead_code, reason = "Task 7 activates coordinator worker ownership")]
struct WorkerRegistration {
    instance_id: Uuid,
    parent_connection_id: String,
    cancel: CancellationToken,
}

#[allow(dead_code, reason = "Task 7 activates coordinator worker ownership")]
struct WorkerContext {
    store: Arc<dyn ContinuationStore>,
    broker: Arc<DelegationBroker>,
    port: Arc<dyn ParentContinuationPort>,
    clock: Arc<dyn ContinuationClock>,
    metrics: Arc<DelegationMetrics>,
    workers: Arc<Mutex<HashMap<(String, u64), WorkerRegistration>>>,
    instance_id: Uuid,
    cancel: CancellationToken,
}

#[allow(dead_code, reason = "Task 7 activates coordinator worker ownership")]
struct WorkerRegistryGuard {
    workers: Arc<Mutex<HashMap<(String, u64), WorkerRegistration>>>,
    key: (String, u64),
    instance_id: Uuid,
}

impl Drop for WorkerRegistryGuard {
    fn drop(&mut self) {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if workers
            .get(&self.key)
            .is_some_and(|registration| registration.instance_id == self.instance_id)
        {
            workers.remove(&self.key);
        }
    }
}

#[allow(dead_code, reason = "Task 7 activates the bootstrapped coordinator")]
pub struct DelegationContinuationCoordinator {
    store: Arc<dyn ContinuationStore>,
    broker: Arc<DelegationBroker>,
    metrics: Arc<DelegationMetrics>,
    port: Arc<dyn ParentContinuationPort>,
    clock: Arc<dyn ContinuationClock>,
    workers: Arc<Mutex<HashMap<(String, u64), WorkerRegistration>>>,
}

impl DelegationContinuationCoordinator {
    pub(crate) fn new(
        store: Arc<dyn ContinuationStore>,
        broker: Arc<DelegationBroker>,
        metrics: Arc<DelegationMetrics>,
        port: Arc<dyn ParentContinuationPort>,
        clock: Arc<dyn ContinuationClock>,
    ) -> Self {
        Self {
            store,
            broker,
            metrics,
            port,
            clock,
            workers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub(super) fn worker_count(&self) -> usize {
        self.workers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    #[allow(dead_code, reason = "Task 8 invokes ordered parent cleanup")]
    pub(crate) fn cancel_workers_for_parent(&self, parent_connection_id: &str) -> usize {
        let workers = self
            .workers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut cancelled = 0;
        for registration in workers.values() {
            if registration.parent_connection_id == parent_connection_id {
                registration.cancel.cancel();
                cancelled += 1;
            }
        }
        cancelled
    }

    #[allow(dead_code, reason = "Task 7 invokes the Join coordinator entry")]
    pub(crate) async fn begin_arm_from_join(
        &self,
        request: JoinArmRequest,
    ) -> Result<JoinArmOutcome, ContinuationError> {
        match self
            .broker
            .evaluate_join_snapshot(
                &request.parent_connection_id,
                request.parent_conversation_id,
                &request.task_ids,
            )
            .await
        {
            JoinEvaluation::Ready(batch) => return Ok(JoinArmOutcome::Immediate(batch)),
            JoinEvaluation::Waiting(_) => {}
        }

        let parent = self
            .port
            .snapshot_parent(&request.parent_connection_id)
            .await?;
        if parent.connection_id != request.parent_connection_id
            || parent.conversation_id != request.parent_conversation_id
            || !parent.turn_in_flight
        {
            return Err(ContinuationError::ParentIdentityChanged);
        }

        let continuation_id = Uuid::new_v4().to_string();
        let internal_prompt_id = Uuid::new_v4().to_string();
        let marker = internal_prompt_marker(&continuation_id, &internal_prompt_id);
        let armed_at = self.clock.now_utc();
        let wake_at = armed_at + chrono::Duration::milliseconds(CONTINUATION_CHECKPOINT_MS as i64);

        if request.waiter_closed.is_cancelled() {
            return Err(ContinuationError::WaiterClosedBeforeInsert);
        }
        let record = self
            .store
            .insert_arming(NewContinuation {
                continuation_id: continuation_id.clone(),
                parent_conversation_id: request.parent_conversation_id,
                parent_session_id: parent.session_id,
                parent_connection_id: request.parent_connection_id.clone(),
                parent_turn_generation: parent.turn_generation,
                task_ids: ContinuationTaskIds(request.task_ids),
                armed_at,
                wake_at,
                internal_prompt_id,
                internal_prompt_marker: marker,
            })
            .await?;
        self.metrics.record_continuation_armed();

        let instance_id = Uuid::new_v4();
        let cancel = CancellationToken::new();
        self.workers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (record.continuation_id.clone(), record.generation),
                WorkerRegistration {
                    instance_id,
                    parent_connection_id: request.parent_connection_id,
                    cancel: cancel.clone(),
                },
            );
        let (completion_tx, completion) = oneshot::channel();
        let context = WorkerContext {
            store: self.store.clone(),
            broker: self.broker.clone(),
            port: self.port.clone(),
            clock: self.clock.clone(),
            metrics: self.metrics.clone(),
            workers: self.workers.clone(),
            instance_id,
            cancel,
        };
        tokio::spawn(run_worker(context, record, completion_tx));

        Ok(JoinArmOutcome::Arming {
            continuation_id,
            completion,
        })
    }
}

#[allow(dead_code, reason = "Task 7 activates coordinator state transitions")]
fn keep_patch(state: ContinuationState) -> ContinuationPatch {
    ContinuationPatch {
        state,
        wake_reason: FieldPatch::Keep,
        suspend_requested_at: FieldPatch::Keep,
        suspended_at: FieldPatch::Keep,
        wake_claimed_at: FieldPatch::Keep,
        prompt_admitted_at: FieldPatch::Keep,
        finished_at: FieldPatch::Keep,
        failure_code: FieldPatch::Keep,
    }
}

#[allow(dead_code, reason = "Task 7 activates coordinator wake evaluation")]
fn wake_reason(batch: &DelegationStatusBatch) -> Option<ContinuationWakeReason> {
    match batch.wake_reason {
        Some(DelegationWakeReason::AllTerminal) => Some(ContinuationWakeReason::AllTerminal),
        Some(DelegationWakeReason::AttentionRequired) => {
            Some(ContinuationWakeReason::AttentionRequired)
        }
        Some(DelegationWakeReason::Unavailable) => Some(ContinuationWakeReason::Unavailable),
        None => None,
    }
}

#[allow(dead_code, reason = "Task 7 activates waiting publication")]
fn waiting_projection(record: &ContinuationRecord) -> ContinuationWaitingProjection {
    ContinuationWaitingProjection {
        conversation_id: record.parent_conversation_id,
        state: record.state,
        generation: record.generation,
        armed_at: record.armed_at,
        wake_at: record.wake_at,
    }
}

#[allow(dead_code, reason = "Task 7 activates coordinator wake claims")]
async fn claim_wake(
    context: &WorkerContext,
    record: ContinuationRecord,
    reason: ContinuationWakeReason,
) -> Result<ContinuationRecord, ContinuationError> {
    let mut patch = keep_patch(ContinuationState::WakePending);
    patch.wake_reason = FieldPatch::Set(reason);
    patch.wake_claimed_at = FieldPatch::Set(context.clock.now_utc());
    let claimed = context
        .store
        .cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            record.state,
            patch,
        )
        .await?;
    let Some(claimed) = claimed else {
        context
            .metrics
            .record_continuation_duplicate_claim_suppressed();
        return Err(ContinuationError::StateConflict);
    };
    let duration = context
        .clock
        .now_utc()
        .signed_duration_since(record.armed_at)
        .to_std()
        .unwrap_or_default();
    context
        .metrics
        .record_continuation_wake_claimed(reason, duration);
    Ok(claimed)
}

#[allow(dead_code, reason = "Task 7 activates coordinator arm cleanup")]
async fn fail_before_suspension(
    context: &WorkerContext,
    record: &ContinuationRecord,
    code: ContinuationFailureCode,
) {
    let mut patch = keep_patch(ContinuationState::Failed);
    patch.failure_code = FieldPatch::Set(code);
    patch.finished_at = FieldPatch::Set(context.clock.now_utc());
    if context
        .store
        .cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            record.state,
            patch,
        )
        .await
        .ok()
        .flatten()
        .is_some()
    {
        context
            .metrics
            .record_continuation_failed(record.state, code);
        let _ = context
            .port
            .publish_waiting(
                &record.parent_connection_id.clone().unwrap_or_default(),
                None,
            )
            .await;
        let _ = context
            .port
            .publish_failure(
                &record.parent_connection_id.clone().unwrap_or_default(),
                code,
            )
            .await;
    }
}

#[allow(dead_code, reason = "Task 7 activates coordinator workers")]
async fn run_worker(
    context: WorkerContext,
    record: ContinuationRecord,
    completion: oneshot::Sender<Result<SuspensionAck, ContinuationError>>,
) {
    let _guard = WorkerRegistryGuard {
        workers: context.workers.clone(),
        key: (record.continuation_id.clone(), record.generation),
        instance_id: context.instance_id,
    };
    run_worker_owned(&context, record, completion).await;
}

#[allow(dead_code, reason = "Task 7 activates coordinator workers")]
async fn run_worker_owned(
    context: &WorkerContext,
    mut record: ContinuationRecord,
    completion: oneshot::Sender<Result<SuspensionAck, ContinuationError>>,
) {
    let notifier = context.broker.join_notifier();
    let mut notified = Box::pin(notifier.notified());
    notified.as_mut().enable();
    let post_insert = context
        .broker
        .evaluate_join_snapshot(
            record.parent_connection_id.as_deref().unwrap_or_default(),
            record.parent_conversation_id,
            &record.task_ids.0,
        )
        .await;

    let mut suspend_patch = keep_patch(ContinuationState::Arming);
    suspend_patch.suspend_requested_at = FieldPatch::Set(context.clock.now_utc());
    record = match context
        .store
        .cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            ContinuationState::Arming,
            suspend_patch,
        )
        .await
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            let _ = completion.send(Err(ContinuationError::StateConflict));
            return;
        }
        Err(_) => {
            fail_before_suspension(context, &record, ContinuationFailureCode::ArmFailed).await;
            let _ = completion.send(Err(ContinuationError::StateConflict));
            return;
        }
    };

    let suspend_request = SuspendRequest {
        continuation_id: record.continuation_id.clone(),
        parent_connection_id: record.parent_connection_id.clone().unwrap_or_default(),
        parent_conversation_id: record.parent_conversation_id,
        parent_session_id: record.parent_session_id.clone(),
        parent_turn_generation: record.parent_turn_generation,
    };
    let suspend = context.port.suspend_parent(suspend_request);
    tokio::pin!(suspend);

    let mut claimed = match post_insert {
        JoinEvaluation::Ready(batch) => match wake_reason(&batch) {
            Some(reason) => match claim_wake(context, record.clone(), reason).await {
                Ok(claimed) => Some(claimed),
                Err(error) => {
                    let _ = completion.send(Err(error));
                    return;
                }
            },
            None => None,
        },
        JoinEvaluation::Waiting(_) => None,
    };

    let ack = if claimed.is_some() {
        suspend.await
    } else {
        loop {
            tokio::select! {
                result = &mut suspend => break result,
                _ = &mut notified => {
                    notified = Box::pin(notifier.notified());
                    notified.as_mut().enable();
                    let evaluation = context.broker.evaluate_join_snapshot(
                        record.parent_connection_id.as_deref().unwrap_or_default(),
                        record.parent_conversation_id,
                        &record.task_ids.0,
                    ).await;
                    if let JoinEvaluation::Ready(batch) = evaluation {
                        if let Some(reason) = wake_reason(&batch) {
                            match claim_wake(context, record.clone(), reason).await {
                                Ok(winner) => {
                                    claimed = Some(winner);
                                    break suspend.await;
                                }
                                Err(error) => {
                                    let _ = completion.send(Err(error));
                                    return;
                                }
                            }
                        }
                    }
                }
                _ = context.clock.sleep_until(record.wake_at) => {
                    match claim_wake(
                        context,
                        record.clone(),
                        ContinuationWakeReason::Checkpoint,
                    ).await {
                        Ok(winner) => {
                            claimed = Some(winner);
                            break suspend.await;
                        }
                        Err(error) => {
                            let _ = completion.send(Err(error));
                            return;
                        }
                    }
                }
                _ = context.cancel.cancelled() => {
                    let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                    return;
                }
            }
        }
    };

    let ack = match ack {
        Ok(ack) => ack,
        Err(error) => {
            let failed_record = claimed.as_ref().unwrap_or(&record);
            fail_before_suspension(
                context,
                failed_record,
                ContinuationFailureCode::SuspendDispatchFailed,
            )
            .await;
            let _ = completion.send(Err(error));
            return;
        }
    };
    if ack.continuation_id != record.continuation_id
        || ack.parent_turn_generation != record.parent_turn_generation
    {
        let failed_record = claimed.as_ref().unwrap_or(&record);
        fail_before_suspension(context, failed_record, ContinuationFailureCode::ArmFailed).await;
        let _ = completion.send(Err(ContinuationError::ParentIdentityChanged));
        return;
    }

    let suspended_at = context.clock.now_utc();
    record = if let Some(claimed) = claimed {
        let mut patch = keep_patch(ContinuationState::WakePending);
        patch.suspended_at = FieldPatch::Set(suspended_at);
        match context
            .store
            .cas_transition(
                &claimed.continuation_id,
                claimed.generation,
                claimed.version,
                ContinuationState::WakePending,
                patch,
            )
            .await
        {
            Ok(Some(record)) => record,
            _ => {
                let _ = completion.send(Err(ContinuationError::StateConflict));
                return;
            }
        }
    } else {
        let mut patch = keep_patch(ContinuationState::Waiting);
        patch.suspended_at = FieldPatch::Set(suspended_at);
        match context
            .store
            .cas_transition(
                &record.continuation_id,
                record.generation,
                record.version,
                ContinuationState::Arming,
                patch,
            )
            .await
        {
            Ok(Some(record)) => record,
            _ => {
                let _ = completion.send(Err(ContinuationError::StateConflict));
                return;
            }
        }
    };

    let connection_id = record.parent_connection_id.clone().unwrap_or_default();
    let suspend_duration = record
        .suspend_requested_at
        .map(|requested| {
            suspended_at
                .signed_duration_since(requested)
                .to_std()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    context
        .metrics
        .record_continuation_suspended(suspend_duration);
    if context
        .port
        .publish_waiting(&connection_id, Some(waiting_projection(&record)))
        .await
        .is_err()
    {
        let _ = completion.send(Err(ContinuationError::StateConflict));
        return;
    }
    let _ = completion.send(Ok(ack));

    if record.state == ContinuationState::Waiting {
        loop {
            let notified = notifier.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match context
                .broker
                .evaluate_join_snapshot(
                    &connection_id,
                    record.parent_conversation_id,
                    &record.task_ids.0,
                )
                .await
            {
                JoinEvaluation::Ready(batch) => {
                    let Some(reason) = wake_reason(&batch) else {
                        return;
                    };
                    match claim_wake(context, record, reason).await {
                        Ok(claimed) => {
                            record = claimed;
                            break;
                        }
                        Err(_) => return,
                    }
                }
                JoinEvaluation::Waiting(_) => {}
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = context.clock.sleep_until(record.wake_at) => {
                    match claim_wake(context, record, ContinuationWakeReason::Checkpoint).await {
                        Ok(claimed) => {
                            record = claimed;
                            break;
                        }
                        Err(_) => return,
                    }
                }
                _ = context.cancel.cancelled() => return,
            }
        }
    }

    resume_and_finish(context, record).await;
}

#[allow(dead_code, reason = "Task 7 activates coordinator prompt resumption")]
async fn resume_and_finish(context: &WorkerContext, mut record: ContinuationRecord) {
    let connection_id = record.parent_connection_id.clone().unwrap_or_default();
    match context.port.snapshot_parent(&connection_id).await {
        Ok(snapshot)
            if snapshot.connection_id == connection_id
                && snapshot.conversation_id == record.parent_conversation_id
                && snapshot.session_id == record.parent_session_id => {}
        _ => return,
    }
    let patch = keep_patch(ContinuationState::Resuming);
    record = match context
        .store
        .cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            ContinuationState::WakePending,
            patch,
        )
        .await
    {
        Ok(Some(record)) => record,
        _ => return,
    };

    let reason = record
        .wake_reason
        .unwrap_or(ContinuationWakeReason::Unavailable);
    let retry_delays_ms = [100_u64, 500, 2_000];
    let mut terminal_failure = None;
    for attempt in 0..=retry_delays_ms.len() {
        if attempt > 0 {
            let deadline = context.clock.now_utc()
                + chrono::Duration::milliseconds(retry_delays_ms[attempt - 1] as i64);
            tokio::select! {
                _ = context.clock.sleep_until(deadline) => {}
                _ = context.cancel.cancelled() => return,
            }
            context.metrics.record_continuation_prompt_delivery_retry();
        }

        let current = match context.store.load(&record.continuation_id).await {
            Ok(Some(current))
                if current.continuation_id == record.continuation_id
                    && current.generation == record.generation
                    && current.state == ContinuationState::Resuming =>
            {
                current
            }
            _ => return,
        };
        match context.port.snapshot_parent(&connection_id).await {
            Ok(snapshot)
                if snapshot.connection_id == connection_id
                    && snapshot.conversation_id == current.parent_conversation_id
                    && snapshot.session_id == current.parent_session_id => {}
            _ => return,
        }
        let snapshot = match context
            .broker
            .evaluate_join_snapshot(
                &connection_id,
                current.parent_conversation_id,
                &current.task_ids.0,
            )
            .await
        {
            JoinEvaluation::Ready(batch) | JoinEvaluation::Waiting(batch) => batch,
        };
        let origin = DelegationContinuationOrigin::new(
            current.continuation_id.clone(),
            current.generation,
            reason,
            current.internal_prompt_id.clone(),
            current.internal_prompt_marker.clone(),
        );
        let request = ContinuationPromptRequest {
            parent_connection_id: connection_id.clone(),
            parent_conversation_id: current.parent_conversation_id,
            parent_session_id: current.parent_session_id.clone(),
            suspended_turn_generation: current.parent_turn_generation,
            continuation_generation: current.generation,
            expected_version: current.version,
            admitted_at: context.clock.now_utc(),
            origin,
            snapshot,
        };

        match context.port.admit_continuation(request).await {
            Ok(
                result @ (PromptAdmissionResult::Admitted | PromptAdmissionResult::AlreadyAdmitted),
            ) => {
                if result == PromptAdmissionResult::Admitted {
                    context.metrics.record_continuation_prompt_admitted();
                }
                let admitted = match context.store.load(&record.continuation_id).await {
                    Ok(Some(admitted))
                        if admitted.generation == record.generation
                            && admitted.state == ContinuationState::Resuming =>
                    {
                        admitted
                    }
                    _ => return,
                };
                let mut completed = keep_patch(ContinuationState::Completed);
                completed.finished_at = FieldPatch::Set(context.clock.now_utc());
                if context
                    .store
                    .cas_transition(
                        &admitted.continuation_id,
                        admitted.generation,
                        admitted.version,
                        ContinuationState::Resuming,
                        completed,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    let _ = context.port.publish_waiting(&connection_id, None).await;
                }
                return;
            }
            Err(ContinuationError::PromptDelivery(_)) if attempt < retry_delays_ms.len() => {}
            Err(ContinuationError::PromptDelivery(_)) => {
                terminal_failure = Some(ContinuationFailureCode::PromptDeliveryFailed);
                break;
            }
            Err(ContinuationError::StateConflict) => {
                terminal_failure = Some(ContinuationFailureCode::StateConflict);
                break;
            }
            Err(_) => return,
        }
    }
    let Some(failure_code) = terminal_failure else {
        return;
    };

    context
        .broker
        .cancel_by_parent_turn_inline(&connection_id, ParentTurnEndReason::ParentTurnFailed)
        .await;
    let current = match context.store.load(&record.continuation_id).await {
        Ok(Some(current)) => current,
        _ => return,
    };
    if context
        .store
        .cas_fail_and_cancel_parent(
            &current.continuation_id,
            current.generation,
            current.version,
            current.state,
            failure_code,
            context.clock.now_utc(),
        )
        .await
        .ok()
        .flatten()
        .is_some()
    {
        context
            .metrics
            .record_continuation_failed(current.state, failure_code);
        let _ = context.port.publish_waiting(&connection_id, None).await;
        let _ = context
            .port
            .publish_failure(&connection_id, failure_code)
            .await;
    }
}
