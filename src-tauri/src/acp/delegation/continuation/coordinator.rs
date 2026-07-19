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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentConnectionExitCause {
    Disconnected,
    SuspensionDrainTimeout,
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
            let conversation_id = match waiting.as_ref() {
                Some(projection) => projection.conversation_id,
                None => match live.waiting_for_subagents.as_ref() {
                    Some(projection) => projection.conversation_id,
                    None => return Ok(()),
                },
            };
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
    #[error("parent stop cleanup owns suspension rejection")]
    ParentStopRequested,
    #[error("parent suspension was rejected before continuation ownership: {0}")]
    SuspendRejected(String),
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
    pub(crate) fn worker_count(&self) -> usize {
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

    pub(crate) async fn handle_parent_stop(
        &self,
        parent_connection_id: &str,
        parent_conversation_id: i32,
    ) -> Result<usize, ContinuationError> {
        // Detect a durable admission winner before cancelling any worker. Once
        // `prompt_admitted_at` is set the row must finish `Completed` and Stop
        // follows the ordinary active-turn Cancel path.
        let active = match self
            .store
            .load_active_for_conversation(parent_conversation_id)
            .await
        {
            Ok(active) => active,
            Err(error) => {
                self.cancel_workers_for_parent(parent_connection_id);
                self.broker
                    .cancel_by_parent_turn(
                        parent_connection_id,
                        ParentTurnEndReason::ParentCanceled,
                    )
                    .await;
                return Err(error.into());
            }
        };
        if active.as_ref().is_some_and(|record| {
            record.parent_connection_id.as_deref() == Some(parent_connection_id)
                && record.prompt_admitted_at.is_some()
        }) {
            return Ok(0);
        }

        self.cancel_workers_for_parent(parent_connection_id);
        if !active.as_ref().is_some_and(|record| {
            record.parent_connection_id.as_deref() == Some(parent_connection_id)
        }) {
            return Ok(0);
        }

        self.broker
            .cancel_by_parent_turn(parent_connection_id, ParentTurnEndReason::ParentCanceled)
            .await;
        let Some(current) = self
            .store
            .load_active_for_conversation(parent_conversation_id)
            .await?
        else {
            return Ok(0);
        };
        if current.parent_connection_id.as_deref() != Some(parent_connection_id) {
            return Ok(0);
        }
        // Re-check after the drain: a concurrent admission winner must not be
        // overwritten as Cancelled (direct coordinator calls without prompt_lock).
        if current.prompt_admitted_at.is_some() {
            return Ok(0);
        }
        let mut cancelled = keep_patch(ContinuationState::Cancelled);
        cancelled.finished_at = FieldPatch::Set(self.clock.now_utc());
        let Some(winner) = self
            .store
            .cas_transition(
                &current.continuation_id,
                current.generation,
                current.version,
                current.state,
                cancelled,
            )
            .await?
        else {
            return Ok(0);
        };
        self.metrics.record_continuation_cancelled(current.state);
        if let Err(error) = self.port.publish_waiting(parent_connection_id, None).await {
            tracing::warn!(
                parent_connection_id,
                continuation_id = %winner.continuation_id,
                "failed to clear continuation waiting projection after user stop: {error}"
            );
        }
        Ok(1)
    }

    pub(crate) async fn handle_parent_connection_exit(
        &self,
        parent_connection_id: &str,
        parent_conversation_id: Option<i32>,
        cause: ParentConnectionExitCause,
    ) {
        self.cancel_workers_for_parent(parent_connection_id);
        let had_matching_active = match parent_conversation_id {
            Some(conversation_id) => match self
                .store
                .load_active_for_conversation(conversation_id)
                .await
            {
                Ok(Some(record)) => {
                    record.parent_connection_id.as_deref() == Some(parent_connection_id)
                }
                Ok(None) => false,
                Err(error) => {
                    tracing::warn!(
                        parent_connection_id,
                        conversation_id,
                        "failed to load continuation during parent connection cleanup: {error}"
                    );
                    false
                }
            },
            None => false,
        };

        self.broker.cancel_by_parent(parent_connection_id).await;
        if !had_matching_active {
            return;
        }
        let Some(conversation_id) = parent_conversation_id else {
            return;
        };
        let current = match self
            .store
            .load_active_for_conversation(conversation_id)
            .await
        {
            Ok(Some(record))
                if record.parent_connection_id.as_deref() == Some(parent_connection_id) =>
            {
                record
            }
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(
                    parent_connection_id,
                    conversation_id,
                    "failed to reload continuation after parent disconnect drain: {error}"
                );
                return;
            }
        };
        let failure_code = match cause {
            ParentConnectionExitCause::Disconnected => {
                ContinuationFailureCode::ParentConnectionLost
            }
            ParentConnectionExitCause::SuspensionDrainTimeout => {
                ContinuationFailureCode::SuspendDrainTimeout
            }
        };
        match self
            .store
            .cas_fail_and_cancel_parent(
                &current.continuation_id,
                current.generation,
                current.version,
                current.state,
                failure_code,
                self.clock.now_utc(),
            )
            .await
        {
            Ok(Some(_)) => {
                self.metrics
                    .record_continuation_failed(current.state, failure_code);
                if let Err(error) = self.port.publish_waiting(parent_connection_id, None).await {
                    tracing::warn!(
                        parent_connection_id,
                        conversation_id,
                        "failed to clear continuation waiting projection after disconnect: {error}"
                    );
                }
                if let Err(error) = self
                    .port
                    .publish_failure(parent_connection_id, failure_code)
                    .await
                {
                    tracing::warn!(
                        parent_connection_id,
                        conversation_id,
                        code = failure_code.as_str(),
                        "failed to publish continuation disconnect failure: {error}"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                parent_connection_id,
                conversation_id,
                "failed to persist continuation disconnect failure: {error}"
            ),
        }
    }

    pub async fn reconcile_on_startup(&self) -> Result<usize, ContStoreError> {
        let winners = self
            .store
            .fail_non_terminal_on_startup(self.clock.now_utc())
            .await?;
        for winner in &winners {
            // Label the prior active phase from the store CAS result — never
            // infer from the terminal Failed record.
            self.metrics
                .record_continuation_reconciled(winner.prior_state);
            let connection_id = winner
                .record
                .parent_connection_id
                .as_deref()
                .unwrap_or_default();
            let code = winner
                .record
                .failure_code
                .unwrap_or(ContinuationFailureCode::ParentConnectionLost);
            if let Err(error) = self.port.publish_failure(connection_id, code).await {
                tracing::warn!(
                    parent_connection_id = connection_id,
                    conversation_id = winner.record.parent_conversation_id,
                    code = code.as_str(),
                    "failed to publish startup continuation failure: {error}"
                );
            }
        }
        Ok(winners.len())
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
    match context
        .store
        .cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            record.state,
            patch,
        )
        .await
    {
        Ok(Some(_)) => {
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
        Ok(None) => retain_unless_exact_terminal(context, record).await,
        Err(_) => retain_until_cancelled(context).await,
    }
}

#[allow(dead_code, reason = "Task 7 activates post-suspension cleanup")]
async fn retain_until_cancelled(context: &WorkerContext) {
    context.cancel.cancelled().await;
}

fn is_terminal_state(state: ContinuationState) -> bool {
    matches!(
        state,
        ContinuationState::Completed | ContinuationState::Cancelled | ContinuationState::Failed
    )
}

async fn retain_if_active(context: &WorkerContext, continuation_id: &str) {
    let terminal = context
        .store
        .load(continuation_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|record| is_terminal_state(record.state));
    if !terminal {
        retain_until_cancelled(context).await;
    }
}

async fn retain_unless_exact_terminal(context: &WorkerContext, owned: &ContinuationRecord) {
    let exact_terminal = context
        .store
        .load(&owned.continuation_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|record| {
            record.generation == owned.generation && is_terminal_state(record.state)
        });
    if !exact_terminal {
        retain_until_cancelled(context).await;
    }
}

#[allow(dead_code, reason = "Task 7 activates post-suspension cleanup")]
async fn fail_after_suspension(
    context: &WorkerContext,
    owned: &ContinuationRecord,
    code: ContinuationFailureCode,
) {
    let claimed = match context
        .store
        .cas_claim_cleanup(
            &owned.continuation_id,
            owned.generation,
            owned.version,
            owned.state,
        )
        .await
    {
        Ok(Some(claimed)) => claimed,
        Ok(None) => {
            retain_unless_exact_terminal(context, owned).await;
            return;
        }
        Err(_) => {
            retain_until_cancelled(context).await;
            return;
        }
    };
    let connection_id = owned.parent_connection_id.clone().unwrap_or_default();
    context
        .broker
        .cancel_by_parent_turn_inline(&connection_id, ParentTurnEndReason::ParentTurnFailed)
        .await;
    match context
        .store
        .cas_fail_and_cancel_parent(
            &claimed.continuation_id,
            claimed.generation,
            claimed.version,
            claimed.state,
            code,
            context.clock.now_utc(),
        )
        .await
    {
        Ok(Some(_)) => {
            context
                .metrics
                .record_continuation_failed(claimed.state, code);
            let _ = context.port.publish_waiting(&connection_id, None).await;
            let _ = context.port.publish_failure(&connection_id, code).await;
        }
        Ok(None) | Err(_) => {
            retain_unless_exact_terminal(context, &claimed).await;
        }
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
    if context.cancel.is_cancelled() {
        let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
        return;
    }
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

    if context.cancel.is_cancelled() {
        let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
        return;
    }
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
            retain_unless_exact_terminal(context, &record).await;
            return;
        }
        Err(_) => {
            let _ = completion.send(Err(ContinuationError::StateConflict));
            fail_before_suspension(context, &record, ContinuationFailureCode::ArmFailed).await;
            return;
        }
    };

    if context.cancel.is_cancelled() {
        let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
        return;
    }
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
            Some(reason) => {
                if context.cancel.is_cancelled() {
                    let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                    return;
                }
                match claim_wake(context, record.clone(), reason).await {
                    Ok(claimed) => Some(claimed),
                    Err(error) => {
                        let _ = completion.send(Err(error));
                        retain_if_active(context, &record.continuation_id).await;
                        return;
                    }
                }
            }
            None => None,
        },
        JoinEvaluation::Waiting(_) => None,
    };

    let ack = if claimed.is_some() {
        tokio::select! {
            biased;
            _ = context.cancel.cancelled() => {
                let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                return;
            }
            result = &mut suspend => result,
        }
    } else {
        loop {
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => {
                    let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                    return;
                }
                result = &mut suspend => break result,
                _ = &mut notified => {
                    notified = Box::pin(notifier.notified());
                    notified.as_mut().enable();
                    if context.cancel.is_cancelled() {
                        let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                        return;
                    }
                    let evaluation = context.broker.evaluate_join_snapshot(
                        record.parent_connection_id.as_deref().unwrap_or_default(),
                        record.parent_conversation_id,
                        &record.task_ids.0,
                    ).await;
                    if let JoinEvaluation::Ready(batch) = evaluation {
                        if let Some(reason) = wake_reason(&batch) {
                            if context.cancel.is_cancelled() {
                                let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                                return;
                            }
                            match claim_wake(context, record.clone(), reason).await {
                                Ok(winner) => {
                                    claimed = Some(winner);
                                    break tokio::select! {
                                        biased;
                                        _ = context.cancel.cancelled() => {
                                            let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                                            return;
                                        }
                                        result = &mut suspend => result,
                                    };
                                }
                                Err(error) => {
                                    let _ = completion.send(Err(error));
                                    retain_if_active(context, &record.continuation_id).await;
                                    return;
                                }
                            }
                        }
                    }
                }
                _ = context.clock.sleep_until(record.wake_at) => {
                    if context.cancel.is_cancelled() {
                        let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                        return;
                    }
                    match claim_wake(
                        context,
                        record.clone(),
                        ContinuationWakeReason::Checkpoint,
                    ).await {
                        Ok(winner) => {
                            claimed = Some(winner);
                            break tokio::select! {
                                biased;
                                _ = context.cancel.cancelled() => {
                                    let _ = completion.send(Err(ContinuationError::ArmWorkerDropped));
                                    return;
                                }
                                result = &mut suspend => result,
                            };
                        }
                        Err(error) => {
                            let _ = completion.send(Err(error));
                            retain_if_active(context, &record.continuation_id).await;
                            return;
                        }
                    }
                }
            }
        }
    };

    let ack = match ack {
        Ok(ack) => ack,
        Err(error) => {
            if matches!(
                &error,
                ContinuationError::SuspendDrainTimeout
                    | ContinuationError::ParentConnectionLost
                    | ContinuationError::ParentStopRequested
            ) {
                let _ = completion.send(Err(error));
                retain_until_cancelled(context).await;
                return;
            }
            let failed_record = claimed.as_ref().unwrap_or(&record);
            let failure_code = if matches!(&error, ContinuationError::SuspendDispatch(_)) {
                ContinuationFailureCode::SuspendDispatchFailed
            } else {
                ContinuationFailureCode::ArmFailed
            };
            let _ = completion.send(Err(error));
            fail_before_suspension(context, failed_record, failure_code).await;
            return;
        }
    };
    if ack.continuation_id != record.continuation_id
        || ack.parent_turn_generation != record.parent_turn_generation
    {
        let failed_record = claimed.as_ref().unwrap_or(&record);
        let _ = completion.send(Err(ContinuationError::ParentIdentityChanged));
        fail_after_suspension(
            context,
            failed_record,
            ContinuationFailureCode::StateConflict,
        )
        .await;
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
                fail_after_suspension(context, &claimed, ContinuationFailureCode::StateConflict)
                    .await;
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
                fail_after_suspension(context, &record, ContinuationFailureCode::StateConflict)
                    .await;
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
        fail_after_suspension(context, &record, ContinuationFailureCode::StateConflict).await;
        return;
    }
    let _ = completion.send(Ok(ack));

    if record.state == ContinuationState::Waiting {
        loop {
            if context.cancel.is_cancelled() {
                return;
            }
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
                        fail_after_suspension(
                            context,
                            &record,
                            ContinuationFailureCode::StateConflict,
                        )
                        .await;
                        return;
                    };
                    if context.cancel.is_cancelled() {
                        return;
                    }
                    match claim_wake(context, record.clone(), reason).await {
                        Ok(claimed) => {
                            record = claimed;
                            break;
                        }
                        Err(_) => {
                            fail_after_suspension(
                                context,
                                &record,
                                ContinuationFailureCode::StateConflict,
                            )
                            .await;
                            return;
                        }
                    }
                }
                JoinEvaluation::Waiting(_) => {}
            }
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => return,
                _ = &mut notified => {}
                _ = context.clock.sleep_until(record.wake_at) => {
                    if context.cancel.is_cancelled() {
                        return;
                    }
                    match claim_wake(
                        context,
                        record.clone(),
                        ContinuationWakeReason::Checkpoint,
                    ).await {
                        Ok(claimed) => {
                            record = claimed;
                            break;
                        }
                        Err(_) => {
                            fail_after_suspension(
                                context,
                                &record,
                                ContinuationFailureCode::StateConflict,
                            )
                            .await;
                            return;
                        }
                    }
                }
            }
        }
    }

    if context.cancel.is_cancelled() {
        return;
    }
    resume_and_finish(context, record).await;
}

#[allow(dead_code, reason = "Task 7 activates coordinator prompt resumption")]
async fn resume_and_finish(context: &WorkerContext, mut record: ContinuationRecord) {
    if context.cancel.is_cancelled() {
        return;
    }
    let connection_id = record.parent_connection_id.clone().unwrap_or_default();
    match context.port.snapshot_parent(&connection_id).await {
        Ok(snapshot)
            if snapshot.connection_id == connection_id
                && snapshot.conversation_id == record.parent_conversation_id
                && snapshot.session_id == record.parent_session_id => {}
        _ => {
            fail_after_suspension(context, &record, ContinuationFailureCode::StateConflict).await;
            return;
        }
    }
    if context.cancel.is_cancelled() {
        return;
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
        _ => {
            fail_after_suspension(context, &record, ContinuationFailureCode::StateConflict).await;
            return;
        }
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
                biased;
                _ = context.cancel.cancelled() => return,
                _ = context.clock.sleep_until(deadline) => {}
            }
            context.metrics.record_continuation_prompt_delivery_retry();
        } else if context.cancel.is_cancelled() {
            return;
        }

        let current = match context.store.load(&record.continuation_id).await {
            Ok(Some(current))
                if current.continuation_id == record.continuation_id
                    && current.generation == record.generation
                    && current.state == ContinuationState::Resuming =>
            {
                current
            }
            _ => {
                retain_until_cancelled(context).await;
                return;
            }
        };
        if context.cancel.is_cancelled() {
            return;
        }
        match context.port.snapshot_parent(&connection_id).await {
            Ok(snapshot)
                if snapshot.connection_id == connection_id
                    && snapshot.conversation_id == current.parent_conversation_id
                    && snapshot.session_id == current.parent_session_id => {}
            _ => {
                fail_after_suspension(context, &current, ContinuationFailureCode::StateConflict)
                    .await;
                return;
            }
        }
        if context.cancel.is_cancelled() {
            return;
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

        if context.cancel.is_cancelled() {
            return;
        }
        let admission = tokio::select! {
            biased;
            _ = context.cancel.cancelled() => {
                return;
            }
            result = context.port.admit_continuation(request) => result,
        };

        match admission {
            Ok(
                result @ (PromptAdmissionResult::Admitted | PromptAdmissionResult::AlreadyAdmitted),
            ) => {
                // Admission's no-await tail has already won. Finish Completed even
                // if Stop later observes the worker token; item 1 owns arbitration.
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
                    _ => {
                        retain_until_cancelled(context).await;
                        return;
                    }
                };
                let mut completed = keep_patch(ContinuationState::Completed);
                completed.finished_at = FieldPatch::Set(context.clock.now_utc());
                match context
                    .store
                    .cas_transition(
                        &admitted.continuation_id,
                        admitted.generation,
                        admitted.version,
                        ContinuationState::Resuming,
                        completed,
                    )
                    .await
                {
                    Ok(Some(_)) => {
                        let _ = context.port.publish_waiting(&connection_id, None).await;
                    }
                    Ok(None) | Err(_) => {
                        fail_after_suspension(
                            context,
                            &admitted,
                            ContinuationFailureCode::StateConflict,
                        )
                        .await;
                    }
                }
                return;
            }
            Err(ContinuationError::PromptDelivery(_)) if attempt < retry_delays_ms.len() => {}
            Err(ContinuationError::PromptDelivery(_)) => {
                terminal_failure = Some((ContinuationFailureCode::PromptDeliveryFailed, current));
                break;
            }
            Err(ContinuationError::StateConflict) => {
                terminal_failure = Some((ContinuationFailureCode::StateConflict, current));
                break;
            }
            Err(ContinuationError::ParentUnavailable)
            | Err(ContinuationError::ParentConnectionLost) => {
                retain_until_cancelled(context).await;
                return;
            }
            Err(_) => {
                fail_after_suspension(context, &current, ContinuationFailureCode::StateConflict)
                    .await;
                return;
            }
        }
    }
    let Some((failure_code, failed_record)) = terminal_failure else {
        return;
    };
    fail_after_suspension(context, &failed_record, failure_code).await;
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::acp::connection::SuspensionAck;
    use crate::acp::delegation::broker::ConversationDepthLookup;
    use crate::acp::delegation::continuation::store::InMemoryContinuationStore;
    use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
    use crate::acp::delegation::types::DelegationError;
    use crate::models::AgentType;

    #[derive(Default)]
    struct RecordingPort {
        failures: tokio::sync::Mutex<Vec<(String, ContinuationFailureCode)>>,
        waiting_clears: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ParentContinuationPort for RecordingPort {
        async fn snapshot_parent(
            &self,
            _connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            Err(ContinuationError::ParentUnavailable)
        }

        async fn suspend_parent(
            &self,
            _request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            Err(ContinuationError::ParentUnavailable)
        }

        async fn admit_continuation(
            &self,
            _request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            Err(ContinuationError::ParentUnavailable)
        }

        async fn publish_waiting(
            &self,
            connection_id: &str,
            waiting: Option<ContinuationWaitingProjection>,
        ) -> Result<(), ContinuationError> {
            assert!(waiting.is_none());
            self.waiting_clears
                .lock()
                .await
                .push(connection_id.to_string());
            Ok(())
        }

        async fn publish_failure(
            &self,
            connection_id: &str,
            code: ContinuationFailureCode,
        ) -> Result<(), ContinuationError> {
            self.failures
                .lock()
                .await
                .push((connection_id.to_string(), code));
            Ok(())
        }
    }

    struct EmptyDepth;

    #[async_trait]
    impl ConversationDepthLookup for EmptyDepth {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    fn cleanup_coordinator(
        store: Arc<dyn ContinuationStore>,
        port: Arc<RecordingPort>,
    ) -> DelegationContinuationCoordinator {
        cleanup_coordinator_with_metrics(store, port, Arc::new(DelegationMetrics::default()))
    }

    fn cleanup_coordinator_with_metrics(
        store: Arc<dyn ContinuationStore>,
        port: Arc<RecordingPort>,
        metrics: Arc<DelegationMetrics>,
    ) -> DelegationContinuationCoordinator {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationContinuationCoordinator::new(
            store,
            broker,
            metrics,
            port,
            Arc::new(SystemContinuationClock::new()),
        )
    }

    fn cleanup_new(id: &str, connection_id: &str, conversation_id: i32) -> NewContinuation {
        let now = Utc::now();
        NewContinuation {
            continuation_id: id.to_string(),
            parent_conversation_id: conversation_id,
            parent_session_id: "parent-session".to_string(),
            parent_connection_id: connection_id.to_string(),
            parent_turn_generation: 1,
            task_ids: ContinuationTaskIds(vec!["task-1".to_string()]),
            armed_at: now,
            wake_at: now,
            internal_prompt_id: format!("prompt-{id}"),
            internal_prompt_marker: format!("marker-{id}"),
        }
    }

    fn cleanup_patch(state: ContinuationState) -> ContinuationPatch {
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

    async fn advance_to(
        store: &InMemoryContinuationStore,
        mut record: ContinuationRecord,
        target: ContinuationState,
    ) -> ContinuationRecord {
        for state in [
            ContinuationState::Waiting,
            ContinuationState::WakePending,
            ContinuationState::Resuming,
            ContinuationState::Completed,
        ] {
            if record.state == target {
                break;
            }
            record = store
                .cas_transition(
                    &record.continuation_id,
                    record.generation,
                    record.version,
                    record.state,
                    cleanup_patch(state),
                )
                .await
                .unwrap()
                .unwrap();
        }
        record
    }

    #[tokio::test]
    async fn continuation_cleanup_stop_cancels_each_active_phase_and_registered_worker() {
        for (index, phase) in [
            ContinuationState::Arming,
            ContinuationState::Waiting,
            ContinuationState::WakePending,
            ContinuationState::Resuming,
        ]
        .into_iter()
        .enumerate()
        {
            let store = Arc::new(InMemoryContinuationStore::default());
            let record = store
                .insert_arming(cleanup_new(&format!("stop-{index}"), "parent", 1))
                .await
                .unwrap();
            let record = advance_to(&store, record, phase).await;
            let port = Arc::new(RecordingPort::default());
            let coordinator = cleanup_coordinator(store.clone(), port);
            let worker_cancel = CancellationToken::new();
            coordinator.workers.lock().unwrap().insert(
                (record.continuation_id.clone(), record.generation),
                WorkerRegistration {
                    instance_id: Uuid::new_v4(),
                    parent_connection_id: "parent".to_string(),
                    cancel: worker_cancel.clone(),
                },
            );

            assert_eq!(
                coordinator.handle_parent_stop("parent", 1).await.unwrap(),
                1
            );
            assert!(worker_cancel.is_cancelled());
            assert_eq!(
                store
                    .load(&record.continuation_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                ContinuationState::Cancelled
            );
            assert_eq!(
                coordinator
                    .metrics
                    .snapshot()
                    .continuation_cancelled
                    .get(phase.as_str()),
                Some(&1)
            );
        }
    }

    #[tokio::test]
    async fn continuation_cleanup_stop_skips_completed_prompt_admission() {
        let store = Arc::new(InMemoryContinuationStore::default());
        let record = store
            .insert_arming(cleanup_new("completed", "parent", 1))
            .await
            .unwrap();
        let completed = advance_to(&store, record, ContinuationState::Completed).await;
        let coordinator = cleanup_coordinator(store.clone(), Arc::new(RecordingPort::default()));

        assert_eq!(
            coordinator.handle_parent_stop("parent", 1).await.unwrap(),
            0
        );
        assert_eq!(
            store
                .load(&completed.continuation_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ContinuationState::Completed
        );
        assert!(coordinator
            .metrics
            .snapshot()
            .continuation_cancelled
            .is_empty());
    }

    #[tokio::test]
    async fn continuation_cleanup_connection_exit_fences_parent_and_maps_typed_cause() {
        for (index, cause, expected) in [
            (
                0,
                ParentConnectionExitCause::Disconnected,
                ContinuationFailureCode::ParentConnectionLost,
            ),
            (
                1,
                ParentConnectionExitCause::SuspensionDrainTimeout,
                ContinuationFailureCode::SuspendDrainTimeout,
            ),
        ] {
            let store = Arc::new(InMemoryContinuationStore::default());
            let record = store
                .insert_arming(cleanup_new(&format!("exit-{index}"), "parent", 1))
                .await
                .unwrap();
            let port = Arc::new(RecordingPort::default());
            let coordinator = cleanup_coordinator(store.clone(), port.clone());

            coordinator
                .handle_parent_connection_exit("parent", Some(1), cause)
                .await;

            let failed = store.load(&record.continuation_id).await.unwrap().unwrap();
            assert_eq!(failed.state, ContinuationState::Failed);
            assert_eq!(failed.failure_code, Some(expected));
            assert_eq!(
                port.failures.lock().await.as_slice(),
                &[("parent".to_string(), expected)]
            );
            assert_eq!(port.waiting_clears.lock().await.as_slice(), &["parent"]);
        }

        let store = Arc::new(InMemoryContinuationStore::default());
        let record = store
            .insert_arming(cleanup_new("mismatch", "other-parent", 1))
            .await
            .unwrap();
        let coordinator = cleanup_coordinator(store.clone(), Arc::new(RecordingPort::default()));
        coordinator
            .handle_parent_connection_exit(
                "parent",
                Some(1),
                ParentConnectionExitCause::Disconnected,
            )
            .await;
        assert_eq!(
            store
                .load(&record.continuation_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ContinuationState::Arming
        );
    }

    #[tokio::test]
    async fn continuation_cleanup_startup_publishes_winners_once() {
        let store = Arc::new(InMemoryContinuationStore::default());
        store
            .insert_arming(cleanup_new("startup", "parent", 1))
            .await
            .unwrap();
        let port = Arc::new(RecordingPort::default());
        let coordinator = cleanup_coordinator(store, port.clone());

        assert_eq!(coordinator.reconcile_on_startup().await.unwrap(), 1);
        assert_eq!(coordinator.reconcile_on_startup().await.unwrap(), 0);
        assert_eq!(
            port.failures.lock().await.as_slice(),
            &[(
                "parent".to_string(),
                ContinuationFailureCode::ParentConnectionLost
            )]
        );
    }

    #[tokio::test]
    async fn continuation_cleanup_startup_records_prior_active_phase_metrics() {
        let store = Arc::new(InMemoryContinuationStore::default());
        let phases = [
            ContinuationState::Arming,
            ContinuationState::Waiting,
            ContinuationState::WakePending,
            ContinuationState::Resuming,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let conversation_id = (index + 1) as i32;
            let record = store
                .insert_arming(cleanup_new(
                    &format!("startup-phase-{index}"),
                    "parent",
                    conversation_id,
                ))
                .await
                .unwrap();
            advance_to(&store, record, phase).await;
        }
        let metrics = Arc::new(DelegationMetrics::default());
        let coordinator = cleanup_coordinator_with_metrics(
            store,
            Arc::new(RecordingPort::default()),
            metrics.clone(),
        );

        assert_eq!(coordinator.reconcile_on_startup().await.unwrap(), 4);
        let snapshot = metrics.snapshot();
        let reconciled: Vec<_> = snapshot
            .continuation_reconciled
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            reconciled,
            ["arming", "resuming", "waiting", "wake_pending"],
            "startup metrics must label the prior active phase, not the terminal failed state"
        );
        for phase in ["arming", "waiting", "wake_pending", "resuming"] {
            assert_eq!(snapshot.continuation_reconciled.get(phase), Some(&1));
        }
        assert!(
            !snapshot.continuation_reconciled.contains_key("failed"),
            "terminal failed must not appear as a reconcile label"
        );
    }

    /// Gated port that durably stamps `prompt_admitted_at` then blocks before
    /// returning from admission, leaving the worker between admission and the
    /// `Completed` CAS.
    struct PostAdmissionGatePort {
        store: Arc<InMemoryContinuationStore>,
        entered: Mutex<Option<oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        admissions: AtomicUsize,
    }

    impl PostAdmissionGatePort {
        fn new(
            store: Arc<InMemoryContinuationStore>,
        ) -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    store,
                    entered: Mutex::new(Some(entered_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    admissions: AtomicUsize::new(0),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait]
    impl ParentContinuationPort for PostAdmissionGatePort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            Ok(ParentTurnSnapshot {
                connection_id: connection_id.into(),
                conversation_id: 1,
                session_id: "parent-session".into(),
                turn_generation: 1,
                turn_in_flight: false,
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
            self.admissions.fetch_add(1, Ordering::SeqCst);
            let current = self
                .store
                .load(request.origin.continuation_id())
                .await
                .map_err(|_| ContinuationError::StateConflict)?
                .ok_or(ContinuationError::StateConflict)?;
            let mut patch = cleanup_patch(ContinuationState::Resuming);
            patch.prompt_admitted_at = FieldPatch::Set(request.admitted_at);
            self.store
                .cas_transition(
                    &current.continuation_id,
                    current.generation,
                    current.version,
                    ContinuationState::Resuming,
                    patch,
                )
                .await
                .map_err(|_| ContinuationError::StateConflict)?
                .ok_or(ContinuationError::StateConflict)?;
            if let Some(tx) = self
                .entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
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

    /// Blocks inside `suspend_parent` until released so cancellation can race
    /// the direct claimed-suspension await.
    struct SuspendGatePort {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        admit_calls: AtomicUsize,
    }

    impl SuspendGatePort {
        fn new() -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    entered: Mutex::new(Some(entered_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    admit_calls: AtomicUsize::new(0),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait]
    impl ParentContinuationPort for SuspendGatePort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            Ok(ParentTurnSnapshot {
                connection_id: connection_id.into(),
                conversation_id: 1,
                session_id: "parent-session".into(),
                turn_generation: 1,
                turn_in_flight: true,
            })
        }

        async fn suspend_parent(
            &self,
            request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            if let Some(tx) = self
                .entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
            }
            Ok(SuspensionAck {
                continuation_id: request.continuation_id,
                parent_turn_generation: request.parent_turn_generation,
            })
        }

        async fn admit_continuation(
            &self,
            _request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            self.admit_calls.fetch_add(1, Ordering::SeqCst);
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

    /// Blocks at attempt-zero admission before returning so Stop can cancel a
    /// worker queued for prompt admission.
    struct AdmitGatePort {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        admit_calls: AtomicUsize,
        /// Set only after the gated release completes, proving the admit future
        /// was not dropped by a cancellation-first select.
        admit_future_completed: AtomicUsize,
    }

    impl AdmitGatePort {
        fn new() -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    entered: Mutex::new(Some(entered_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    admit_calls: AtomicUsize::new(0),
                    admit_future_completed: AtomicUsize::new(0),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait]
    impl ParentContinuationPort for AdmitGatePort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            Ok(ParentTurnSnapshot {
                connection_id: connection_id.into(),
                conversation_id: 1,
                session_id: "parent-session".into(),
                turn_generation: 1,
                turn_in_flight: false,
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
            self.admit_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(tx) = self
                .entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
            }
            // Reached only when the admit future was not cancelled mid-await.
            self.admit_future_completed.fetch_add(1, Ordering::SeqCst);
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

    /// Observes CAS targets so race tests can wait for explicit state changes
    /// without yield-polling.
    struct TransitionNotifyStore {
        inner: Arc<InMemoryContinuationStore>,
        wake_pending: Mutex<Option<oneshot::Sender<()>>>,
        completed: Mutex<Option<oneshot::Sender<()>>>,
        cancelled: Mutex<Option<oneshot::Sender<()>>>,
        /// Gates insert so a join can become Ready before the worker evaluates
        /// its post-insert snapshot (forces claimed suspend path).
        insert_gate: tokio::sync::Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
        /// Gates the first Arming CAS that sets `suspend_requested_at` so cancel
        /// can race before the first suspension dispatch.
        suspend_request_gate:
            tokio::sync::Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    }

    impl TransitionNotifyStore {
        fn new(inner: Arc<InMemoryContinuationStore>) -> Arc<Self> {
            Arc::new(Self {
                inner,
                wake_pending: Mutex::new(None),
                completed: Mutex::new(None),
                cancelled: Mutex::new(None),
                insert_gate: tokio::sync::Mutex::new(None),
                suspend_request_gate: tokio::sync::Mutex::new(None),
            })
        }

        fn watch_wake_pending(&self) -> oneshot::Receiver<()> {
            let (tx, rx) = oneshot::channel();
            *self
                .wake_pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(tx);
            rx
        }

        fn watch_completed(&self) -> oneshot::Receiver<()> {
            let (tx, rx) = oneshot::channel();
            *self
                .completed
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(tx);
            rx
        }

        fn watch_cancelled(&self) -> oneshot::Receiver<()> {
            let (tx, rx) = oneshot::channel();
            *self
                .cancelled
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(tx);
            rx
        }

        async fn install_insert_gate(
            &self,
            entered: oneshot::Sender<()>,
            release: oneshot::Receiver<()>,
        ) {
            *self.insert_gate.lock().await = Some((entered, release));
        }

        async fn install_suspend_request_gate(
            &self,
            entered: oneshot::Sender<()>,
            release: oneshot::Receiver<()>,
        ) {
            *self.suspend_request_gate.lock().await = Some((entered, release));
        }
    }

    #[async_trait]
    impl ContinuationStore for TransitionNotifyStore {
        async fn insert_arming(
            &self,
            new: NewContinuation,
        ) -> Result<ContinuationRecord, ContStoreError> {
            if let Some((entered, release)) = self.insert_gate.lock().await.take() {
                let _ = entered.send(());
                let _ = release.await;
            }
            self.inner.insert_arming(new).await
        }

        async fn load(
            &self,
            continuation_id: &str,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
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
            if expected_state == ContinuationState::Arming
                && patch.state == ContinuationState::Arming
                && matches!(patch.suspend_requested_at, FieldPatch::Set(_))
            {
                if let Some((entered, release)) = self.suspend_request_gate.lock().await.take() {
                    let _ = entered.send(());
                    let _ = release.await;
                }
            }
            let result = self
                .inner
                .cas_transition(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    patch.clone(),
                )
                .await?;
            if result.is_some() {
                match patch.state {
                    ContinuationState::WakePending => {
                        if let Some(tx) = self
                            .wake_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .take()
                        {
                            let _ = tx.send(());
                        }
                    }
                    ContinuationState::Completed => {
                        if let Some(tx) = self
                            .completed
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .take()
                        {
                            let _ = tx.send(());
                        }
                    }
                    ContinuationState::Cancelled => {
                        if let Some(tx) = self
                            .cancelled
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .take()
                        {
                            let _ = tx.send(());
                        }
                    }
                    _ => {}
                }
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
            self.inner
                .cas_claim_cleanup(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                )
                .await
        }

        async fn cas_fail_and_cancel_parent(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: ContinuationState,
            failure_code: ContinuationFailureCode,
            finished_at: DateTime<Utc>,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            self.inner
                .cas_fail_and_cancel_parent(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    failure_code,
                    finished_at,
                )
                .await
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

    /// Clock that never fires far-future checkpoint sleeps and completes short
    /// retry/backoff sleeps immediately so cancel can race the retry admit path
    /// without wall-clock waits.
    struct InstantRetryClock {
        base: SystemContinuationClock,
        short_sleeps: AtomicUsize,
    }

    impl InstantRetryClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: SystemContinuationClock::new(),
                short_sleeps: AtomicUsize::new(0),
            })
        }
    }

    impl ContinuationClock for InstantRetryClock {
        fn now_utc(&self) -> DateTime<Utc> {
            self.base.now_utc()
        }

        fn sleep_until(
            &self,
            deadline: DateTime<Utc>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let delay = deadline
                .signed_duration_since(self.now_utc())
                .to_std()
                .unwrap_or_default();
            // Retry delays are 100/500/2000ms; checkpoint sleeps are minutes.
            if delay <= std::time::Duration::from_secs(3) {
                self.short_sleeps.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            } else {
                // Never fire the far-future checkpoint arm; join notify drives wake.
                Box::pin(std::future::pending())
            }
        }
    }

    /// Gates resume-path `snapshot_parent` after the arm-time snapshot so cancel
    /// can race WakePending → Resuming.
    struct ResumeSnapshotGatePort {
        snapshots: AtomicUsize,
        resume_entered: Mutex<Option<oneshot::Sender<()>>>,
        resume_release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        admit_calls: AtomicUsize,
        suspend_calls: AtomicUsize,
    }

    impl ResumeSnapshotGatePort {
        fn new() -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    snapshots: AtomicUsize::new(0),
                    resume_entered: Mutex::new(Some(entered_tx)),
                    resume_release: tokio::sync::Mutex::new(Some(release_rx)),
                    admit_calls: AtomicUsize::new(0),
                    suspend_calls: AtomicUsize::new(0),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait]
    impl ParentContinuationPort for ResumeSnapshotGatePort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            let n = self.snapshots.fetch_add(1, Ordering::SeqCst) + 1;
            // Snapshot #1 is begin_arm_from_join; later calls are resume_and_finish.
            if n >= 2 {
                if let Some(tx) = self
                    .resume_entered
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    let _ = tx.send(());
                }
                if let Some(release) = self.resume_release.lock().await.take() {
                    let _ = release.await;
                }
            }
            Ok(ParentTurnSnapshot {
                connection_id: connection_id.into(),
                conversation_id: 1,
                session_id: "parent-session".into(),
                turn_generation: 1,
                turn_in_flight: n == 1,
            })
        }

        async fn suspend_parent(
            &self,
            request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            self.suspend_calls.fetch_add(1, Ordering::SeqCst);
            Ok(SuspensionAck {
                continuation_id: request.continuation_id,
                parent_turn_generation: request.parent_turn_generation,
            })
        }

        async fn admit_continuation(
            &self,
            _request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            self.admit_calls.fetch_add(1, Ordering::SeqCst);
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

    /// Fails attempt-zero admission, then gates attempt-one so cancel can race
    /// the retry/backoff path.
    struct RetryAdmitGatePort {
        attempts: AtomicUsize,
        retry_entered: Mutex<Option<oneshot::Sender<()>>>,
        retry_release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        retry_future_completed: AtomicUsize,
    }

    impl RetryAdmitGatePort {
        fn new() -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    attempts: AtomicUsize::new(0),
                    retry_entered: Mutex::new(Some(entered_tx)),
                    retry_release: tokio::sync::Mutex::new(Some(release_rx)),
                    retry_future_completed: AtomicUsize::new(0),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait]
    impl ParentContinuationPort for RetryAdmitGatePort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            Ok(ParentTurnSnapshot {
                connection_id: connection_id.into(),
                conversation_id: 1,
                session_id: "parent-session".into(),
                turn_generation: 1,
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
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 1 {
                return Err(ContinuationError::PromptDelivery(AcpError::ProcessExited));
            }
            if let Some(tx) = self
                .retry_entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            if let Some(release) = self.retry_release.lock().await.take() {
                let _ = release.await;
            }
            self.retry_future_completed.fetch_add(1, Ordering::SeqCst);
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

    /// Immediate suspend for arm, then admit via RetryAdmitGatePort.
    struct CompositeRetryPort {
        admit: Arc<RetryAdmitGatePort>,
    }

    #[async_trait]
    impl ParentContinuationPort for CompositeRetryPort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            self.admit.snapshot_parent(connection_id).await
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
            self.admit.admit_continuation(request).await
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

    async fn complete_cleanup_task(broker: &DelegationBroker, task_id: &str) {
        broker
            .complete_call(
                task_id,
                crate::acp::delegation::types::DelegationOutcome::Ok(
                    crate::acp::delegation::types::DelegationSuccess {
                        text: "done".into(),
                        child_conversation_id: 99,
                        child_agent_type: AgentType::Codex,
                        turn_count: 1,
                        duration_ms: 1,
                        token_usage: None,
                    },
                ),
            )
            .await;
    }

    #[tokio::test]
    async fn continuation_cleanup_stop_after_admission_before_completed_preserves_completed() {
        let inner = Arc::new(InMemoryContinuationStore::default());
        let store = TransitionNotifyStore::new(inner.clone());
        let completed_rx = store.watch_completed();
        let (port, admitted_rx, release_tx) = PostAdmissionGatePort::new(inner.clone());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        // Arm while Waiting so a worker is spawned; then complete the join.
        broker.seed_live_task_for_test("parent", "task-1").await;
        // Arm-time identity requires turn_in_flight; admit port stamps prompt_admitted_at.
        let (arm_port, _suspend_entered, suspend_release) = SuspendGatePort::new();
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store.clone() as Arc<dyn ContinuationStore>,
            broker.clone(),
            Arc::new(DelegationMetrics::default()),
            Arc::new(CompositeAdmitPort {
                suspend: arm_port,
                admit: port.clone(),
            }) as Arc<dyn ParentContinuationPort>,
            Arc::new(SystemContinuationClock::new()),
        ));
        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        suspend_release.send(()).unwrap();
        completion.await.unwrap().unwrap();
        complete_cleanup_task(&broker, "task-1").await;
        admitted_rx
            .await
            .expect("worker must reach durable admission before Completed");

        let mid = store.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(mid.state, ContinuationState::Resuming);
        assert!(mid.prompt_admitted_at.is_some());

        assert_eq!(
            coordinator.handle_parent_stop("parent", 1).await.unwrap(),
            0,
            "admitted winner must skip Cancelled CAS"
        );
        let still = store.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(still.state, ContinuationState::Resuming);
        assert!(still.prompt_admitted_at.is_some());
        assert!(coordinator
            .metrics
            .snapshot()
            .continuation_cancelled
            .is_empty());

        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
            .await
            .expect("worker must CAS Completed after admission release within bound")
            .expect("Completed CAS notification channel closed unexpectedly");
        let done = store.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(done.state, ContinuationState::Completed);
        assert!(done.prompt_admitted_at.is_some());
        assert_eq!(coordinator.worker_count(), 0);
        assert_eq!(port.admissions.load(Ordering::SeqCst), 1);
    }

    /// Suspends via one port and admits via another so arm-time identity and
    /// post-admission barriers can be controlled independently.
    struct CompositeAdmitPort {
        suspend: Arc<SuspendGatePort>,
        admit: Arc<PostAdmissionGatePort>,
    }

    #[async_trait]
    impl ParentContinuationPort for CompositeAdmitPort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            self.suspend.snapshot_parent(connection_id).await
        }

        async fn suspend_parent(
            &self,
            request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            self.suspend.suspend_parent(request).await
        }

        async fn admit_continuation(
            &self,
            request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            self.admit.admit_continuation(request).await
        }

        async fn publish_waiting(
            &self,
            connection_id: &str,
            waiting: Option<ContinuationWaitingProjection>,
        ) -> Result<(), ContinuationError> {
            self.admit.publish_waiting(connection_id, waiting).await
        }

        async fn publish_failure(
            &self,
            connection_id: &str,
            code: ContinuationFailureCode,
        ) -> Result<(), ContinuationError> {
            self.admit.publish_failure(connection_id, code).await
        }
    }

    #[tokio::test]
    async fn continuation_cleanup_cancel_fences_direct_claimed_suspension_await() {
        let inner = Arc::new(InMemoryContinuationStore::default());
        let store = TransitionNotifyStore::new(inner);
        let wake_pending_rx = store.watch_wake_pending();
        let (port, entered_rx, release_tx) = SuspendGatePort::new();
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker.seed_live_task_for_test("parent", "task-1").await;
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store.clone() as Arc<dyn ContinuationStore>,
            broker.clone(),
            Arc::new(DelegationMetrics::default()),
            port.clone() as Arc<dyn ParentContinuationPort>,
            Arc::new(SystemContinuationClock::new()),
        ));
        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        entered_rx.await.expect("suspend entered");
        // Complete the join while suspension is still gated so the worker claims
        // wake and re-enters the cancel-vs-suspend select with claimed=Some.
        complete_cleanup_task(&broker, "task-1").await;
        wake_pending_rx
            .await
            .expect("worker must claim wake before cancel races the claimed suspend await");
        assert_eq!(
            store.load(&continuation_id).await.unwrap().unwrap().state,
            ContinuationState::WakePending,
            "worker must claim wake before cancel races the claimed suspend await"
        );
        assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
        let arm_result = tokio::time::timeout(std::time::Duration::from_secs(2), completion)
            .await
            .expect("cancel must interrupt claimed suspend await without release")
            .unwrap();
        assert!(matches!(
            arm_result,
            Err(ContinuationError::ArmWorkerDropped)
        ));
        let _ = release_tx.send(());
        assert_eq!(port.admit_calls.load(Ordering::SeqCst), 0);
        let row = store.load(&continuation_id).await.unwrap().unwrap();
        assert!(
            !matches!(
                row.state,
                ContinuationState::Completed | ContinuationState::Failed
            ),
            "cancelled pre-admission worker must not invent a terminal failure/completion: {row:?}"
        );
        assert_eq!(coordinator.worker_count(), 0);
    }

    #[tokio::test]
    async fn continuation_cleanup_cancel_fences_attempt_zero_admission_wait() {
        let inner = Arc::new(InMemoryContinuationStore::default());
        let store = TransitionNotifyStore::new(inner);
        let cancelled_rx = store.watch_cancelled();
        let (port, entered_rx, release_tx) = AdmitGatePort::new();
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker.seed_live_task_for_test("parent", "task-1").await;
        // AdmitGatePort reports turn_in_flight=false; arm needs in-flight.
        let (suspend_port, _entered, suspend_release) = SuspendGatePort::new();
        let composite = Arc::new(CompositeAttemptPort {
            suspend: suspend_port,
            admit: port.clone(),
        });
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store.clone() as Arc<dyn ContinuationStore>,
            broker.clone(),
            Arc::new(DelegationMetrics::default()),
            composite as Arc<dyn ParentContinuationPort>,
            Arc::new(SystemContinuationClock::new()),
        ));
        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        suspend_release.send(()).unwrap();
        completion.await.unwrap().unwrap();
        complete_cleanup_task(&broker, "task-1").await;
        entered_rx.await.expect("attempt-zero admission entered");

        assert_eq!(
            coordinator.handle_parent_stop("parent", 1).await.unwrap(),
            1
        );
        cancelled_rx
            .await
            .expect("Stop must CAS Cancelled before admission can complete");
        // Keep the admit gate held. Cancellation-first select must drop the admit
        // future and exit the worker without ever returning Admitted.
        let worker_gone = async {
            loop {
                if coordinator.worker_count() == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), worker_gone)
            .await
            .expect("cancelled attempt-zero worker must exit while admit is still gated");
        assert_eq!(
            port.admit_future_completed.load(Ordering::SeqCst),
            0,
            "admit future must be cancelled before it can return Admitted"
        );
        // Receiver should already be dropped with the cancelled admit future.
        let _ = release_tx.send(());
        let row = store.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(row.state, ContinuationState::Cancelled);
        assert!(
            row.prompt_admitted_at.is_none(),
            "durable admission must never complete once Stop owns cleanup"
        );
        assert_ne!(row.state, ContinuationState::Completed);
        assert_eq!(
            port.admit_calls.load(Ordering::SeqCst),
            1,
            "admission may enter the port while Stop races it"
        );
        assert_eq!(
            port.admit_future_completed.load(Ordering::SeqCst),
            0,
            "post-release must not revive a cancelled admit future"
        );
        assert_eq!(coordinator.worker_count(), 0);
    }

    #[tokio::test]
    async fn continuation_cleanup_cancel_fences_before_first_suspension_dispatch() {
        let inner = Arc::new(InMemoryContinuationStore::default());
        let store = TransitionNotifyStore::new(inner);
        let (insert_entered_tx, insert_entered_rx) = oneshot::channel();
        let (insert_release_tx, insert_release_rx) = oneshot::channel();
        store
            .install_insert_gate(insert_entered_tx, insert_release_rx)
            .await;
        let (cas_entered_tx, cas_entered_rx) = oneshot::channel();
        let (cas_release_tx, cas_release_rx) = oneshot::channel();
        store
            .install_suspend_request_gate(cas_entered_tx, cas_release_rx)
            .await;
        let (port, mut suspend_entered_rx, _suspend_release) = SuspendGatePort::new();
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker.seed_live_task_for_test("parent", "task-1").await;
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store.clone() as Arc<dyn ContinuationStore>,
            broker.clone(),
            Arc::new(DelegationMetrics::default()),
            port.clone() as Arc<dyn ParentContinuationPort>,
            Arc::new(SystemContinuationClock::new()),
        ));
        // Hold insert so the join can become Ready before the worker's post-insert
        // evaluation. That forces the claimed suspend path: unfixed production
        // uses bare suspend.await (no cancel arm) and hangs on the unreleased
        // SuspendGatePort, while the post-CAS cancel fence returns immediately.
        let arm = tokio::spawn({
            let coordinator = coordinator.clone();
            async move {
                coordinator
                    .begin_arm_from_join(JoinArmRequest {
                        parent_connection_id: "parent".into(),
                        parent_conversation_id: 1,
                        task_ids: vec!["task-1".into()],
                        waiter_closed: CancellationToken::new(),
                    })
                    .await
            }
        });
        insert_entered_rx
            .await
            .expect("arm must enter insert_arming gate");
        complete_cleanup_task(&broker, "task-1").await;
        insert_release_tx.send(()).unwrap();
        let outcome = arm.await.unwrap().unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        cas_entered_rx
            .await
            .expect("worker must reach pre-suspension suspend_requested CAS gate");
        assert_eq!(
            store
                .load(&continuation_id)
                .await
                .unwrap()
                .unwrap()
                .suspend_requested_at,
            None,
            "gate holds the suspend_requested CAS before it commits"
        );
        assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
        // Release only the pre-suspension store gate. Leave SuspendGatePort's
        // release intentionally unsent so a regression that dispatches
        // suspend_parent on the claimed path hangs until this timeout fires.
        cas_release_tx.send(()).unwrap();
        let arm_result = tokio::time::timeout(std::time::Duration::from_secs(2), completion)
            .await
            .expect(
                "cancel before first suspension dispatch must drop the arm worker without \
                 entering an unreleased claimed suspend_parent gate",
            )
            .unwrap();
        assert!(
            matches!(arm_result, Err(ContinuationError::ArmWorkerDropped)),
            "cancel before first suspension dispatch must drop the arm worker: {arm_result:?}"
        );
        assert!(
            matches!(
                suspend_entered_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "suspend_parent must never be dispatched after pre-suspension cancel"
        );
        assert_eq!(port.admit_calls.load(Ordering::SeqCst), 0);
        let row = store.load(&continuation_id).await.unwrap().unwrap();
        assert!(
            row.suspended_at.is_none(),
            "first suspension must never complete after pre-dispatch cancel: {row:?}"
        );
        assert!(
            !matches!(
                row.state,
                ContinuationState::Completed | ContinuationState::Failed
            ),
            "pre-suspension cancel must not invent terminal failure/completion: {row:?}"
        );
        assert_eq!(coordinator.worker_count(), 0);
    }

    #[tokio::test]
    async fn continuation_cleanup_cancel_fences_wake_pending_to_resuming() {
        let inner = Arc::new(InMemoryContinuationStore::default());
        let store = TransitionNotifyStore::new(inner);
        let wake_pending_rx = store.watch_wake_pending();
        let (port, resume_entered_rx, resume_release_tx) = ResumeSnapshotGatePort::new();
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker.seed_live_task_for_test("parent", "task-1").await;
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store.clone() as Arc<dyn ContinuationStore>,
            broker.clone(),
            Arc::new(DelegationMetrics::default()),
            port.clone() as Arc<dyn ParentContinuationPort>,
            Arc::new(SystemContinuationClock::new()),
        ));
        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        completion.await.unwrap().unwrap();
        complete_cleanup_task(&broker, "task-1").await;
        wake_pending_rx
            .await
            .expect("worker must reach WakePending before resume snapshot gate");
        resume_entered_rx
            .await
            .expect("resume snapshot must gate WakePending→Resuming");
        assert_eq!(
            store.load(&continuation_id).await.unwrap().unwrap().state,
            ContinuationState::WakePending
        );
        assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
        resume_release_tx.send(()).unwrap();
        // Wait for the worker registry drop via a short join on worker_count by
        // observing the cancel-path exit: no Resuming and no admit.
        let worker_gone = async {
            loop {
                if coordinator.worker_count() == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), worker_gone)
            .await
            .expect("cancelled resume worker must exit");
        let row = store.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(
            row.state,
            ContinuationState::WakePending,
            "cancel during WakePending→Resuming must not win Resuming CAS: {row:?}"
        );
        assert_eq!(port.admit_calls.load(Ordering::SeqCst), 0);
        assert_eq!(coordinator.worker_count(), 0);
    }

    #[tokio::test]
    async fn continuation_cleanup_cancel_fences_retry_backoff_admission() {
        let store = Arc::new(InMemoryContinuationStore::default());
        let (admit_port, retry_entered_rx, retry_release_tx) = RetryAdmitGatePort::new();
        let clock = InstantRetryClock::new();
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker.seed_live_task_for_test("parent", "task-1").await;
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store.clone() as Arc<dyn ContinuationStore>,
            broker.clone(),
            Arc::new(DelegationMetrics::default()),
            Arc::new(CompositeRetryPort {
                admit: admit_port.clone(),
            }) as Arc<dyn ParentContinuationPort>,
            clock.clone() as Arc<dyn ContinuationClock>,
        ));
        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        completion.await.unwrap().unwrap();
        complete_cleanup_task(&broker, "task-1").await;

        // Attempt zero fails PromptDelivery; InstantRetryClock completes the
        // backoff sleep immediately; attempt one enters the retry admit gate.
        retry_entered_rx
            .await
            .expect("worker must reach retry admission after backoff");
        assert_eq!(admit_port.attempts.load(Ordering::SeqCst), 2);
        assert!(
            clock.short_sleeps.load(Ordering::SeqCst) >= 1,
            "retry path must observe at least one backoff sleep"
        );

        // Cancel during the post-backoff retry admission await.
        assert_eq!(coordinator.cancel_workers_for_parent("parent"), 1);
        let _ = retry_release_tx.send(());
        let worker_gone = async {
            loop {
                if coordinator.worker_count() == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), worker_gone)
            .await
            .expect("cancelled retry worker must exit");
        assert_eq!(
            admit_port.retry_future_completed.load(Ordering::SeqCst),
            0,
            "cancel during retry admit must drop the future before Admitted returns"
        );
        let row = store.load(&continuation_id).await.unwrap().unwrap();
        assert!(
            row.prompt_admitted_at.is_none(),
            "retry/backoff cancel must not durable-admit"
        );
        assert_ne!(row.state, ContinuationState::Completed);
        assert_eq!(coordinator.worker_count(), 0);
    }

    struct CompositeAttemptPort {
        suspend: Arc<SuspendGatePort>,
        admit: Arc<AdmitGatePort>,
    }

    #[async_trait]
    impl ParentContinuationPort for CompositeAttemptPort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            self.suspend.snapshot_parent(connection_id).await
        }

        async fn suspend_parent(
            &self,
            request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            self.suspend.suspend_parent(request).await
        }

        async fn admit_continuation(
            &self,
            request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            self.admit.admit_continuation(request).await
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
}
