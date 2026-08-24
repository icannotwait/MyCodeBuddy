mod dto;
mod error;
mod metrics;

pub use dto::*;
pub use error::{validate_client_label, SharedSessionError};
pub use metrics::{SharedSessionMetrics, SharedSessionMetricsSnapshot};

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Mutex, Notify, RwLock};

use crate::{
    acp::session_state::SessionState,
    web::{
        event_bridge::{emit_with_state, EventEmitter},
        ws_attach::DetachReason,
    },
};

use error::validate_failure_code;

pub(crate) struct SharedPromptAdmission {
    pub queue_item_id: String,
    pub events: Vec<crate::acp::types::AcpEvent>,
    pub publication: Arc<tokio::sync::OnceCell<()>>,
    pub publication_invalidated: Arc<AtomicBool>,
    pub notify: Arc<Notify>,
}

pub(crate) struct SharedPromptMutation {
    pub events: Vec<crate::acp::types::AcpEvent>,
    pub notify: Arc<Notify>,
}

pub struct SharedStopRequest {
    pub guard: SharedMutationGuard,
    pub turn_id: String,
}

pub struct SharedInteractionRequest<T> {
    pub guard: SharedMutationGuard,
    pub interaction_id: String,
    pub answer: T,
}

#[derive(Clone)]
pub(crate) struct SharedInteractionClaim {
    connection_id: String,
    generation: u64,
    kind: SharedInteractionKind,
    interaction_id: String,
    claim_id: uuid::Uuid,
}

struct ActiveInteractionClaim {
    interaction_id: String,
    claim_id: uuid::Uuid,
}

#[derive(Clone)]
pub(crate) struct SharedStopClaim {
    connection_id: String,
    generation: u64,
    turn_id: String,
}

pub(crate) enum SharedStopClaimDecision {
    Claimed(SharedStopClaim),
    Resolving(watch::Receiver<Option<StopAdmissionResolution>>),
    Requested,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopAdmissionResolution {
    DefinitelyNotAdmitted,
    Requested,
}

pub(crate) enum DispatchHeadDecision {
    Blocked,
    Failed(FailedSharedPrompt),
    Claimed(ClaimedSharedPrompt),
}

pub(crate) struct FailedSharedPrompt {
    pub events: Vec<crate::acp::types::AcpEvent>,
    pub notify: Arc<Notify>,
}

pub(crate) struct ClaimedSharedPrompt {
    pub blocks: Vec<crate::acp::types::PromptInputBlock>,
    pub folder_id: Option<i32>,
    pub conversation_id: Option<i32>,
    pub client_message_id: String,
    pub capture: Option<crate::auto_title::PromptCaptureContext>,
    pub events: Vec<crate::acp::types::AcpEvent>,
}

pub(crate) struct SharedRuntimeSubscription {
    pub notify: Arc<Notify>,
    pub lifecycle: watch::Receiver<SharedLifecycleState>,
    pub registration: watch::Receiver<SharedRegistrationState>,
}

#[derive(Clone)]
pub struct SharedSessionBroker {
    index: Arc<Mutex<SharedSessionIndex>>,
    index_epoch: Arc<watch::Sender<u64>>,
    metrics: Arc<SharedSessionMetrics>,
    accepting: Arc<AtomicBool>,
    lease_ttl_secs: Arc<AtomicU64>,
    limits: BrokerLimits,
    #[cfg(test)]
    idle_final_cas_barrier: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedAuthoritativeSnapshot {
    pub purpose: crate::auto_title::ConnectionPurpose,
    pub canonical_conversation_id: Option<i32>,
    pub generation: u64,
    pub phase: SharedSessionPhase,
    pub event_seq: u64,
    pub folder_id: Option<i32>,
    pub agent_type: crate::models::AgentType,
}

/// Exact authoritative facts that prevent a shared root from becoming idle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SharedIdleBlockers {
    pub lease: bool,
    pub non_ready_phase: bool,
    pub non_connected_status: bool,
    pub runtime_turn: bool,
    pub active_turn: bool,
    pub permission: bool,
    pub question: bool,
    pub plan_approval: bool,
    pub queued_prompt: bool,
    pub continuation_wait: bool,
    pub active_delegation: bool,
    pub background_work: bool,
    pub host_work: bool,
}

#[derive(Clone, serde::Serialize)]
pub struct SharedSessionDiagnostic {
    pub connection_id: String,
    pub conversation_id: Option<i32>,
    pub generation: u64,
    pub phase: SharedSessionPhase,
    pub agent_category: String,
    pub lease_count: usize,
    pub queue_depth: usize,
    pub queue_bytes: usize,
    pub idle_blockers: Vec<&'static str>,
    pub cleanup_state: &'static str,
    pub bootstrap_duration_ms: u64,
    pub cleanup_duration_ms: u64,
}

impl SharedIdleBlockers {
    fn from_record(record: &SharedSessionRecord, snapshot: &SharedRuntimeWorkSnapshot) -> Self {
        Self::from_record_only(record).with_runtime(snapshot)
    }

    fn from_record_only(record: &SharedSessionRecord) -> Self {
        Self {
            lease: !record.active_leases.is_empty(),
            non_ready_phase: record.phase != SharedSessionPhase::Ready,
            non_connected_status: false,
            runtime_turn: false,
            active_turn: record.active_turn.is_some(),
            permission: record.interactions.permission.is_some(),
            question: record.interactions.question.is_some(),
            plan_approval: record.interactions.plan_approval.is_some(),
            queued_prompt: !record.waiting_prompts.is_empty(),
            continuation_wait: false,
            active_delegation: false,
            background_work: false,
            host_work: !record.host_owned_work.is_empty(),
        }
    }

    fn with_runtime(mut self, snapshot: &SharedRuntimeWorkSnapshot) -> Self {
        self.non_connected_status =
            snapshot.status != crate::acp::types::ConnectionStatus::Connected;
        self.runtime_turn = snapshot.turn_in_flight;
        self.permission |= snapshot.pending_permission_id.is_some();
        self.question |= snapshot.pending_question_id.is_some();
        self.plan_approval |= snapshot.pending_plan_approval_id.is_some();
        self.continuation_wait = snapshot.continuation_wait;
        self.active_delegation = snapshot.active_delegations != 0;
        self.background_work = snapshot.background_outstanding != 0;
        self
    }

    fn is_empty(self) -> bool {
        self == Self::default()
    }

    fn stable_names(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        for (blocked, name) in [
            (self.lease, "lease"),
            (self.non_ready_phase, "non_ready_phase"),
            (self.non_connected_status, "non_connected_status"),
            (self.runtime_turn, "runtime_turn"),
            (self.active_turn, "active_turn"),
            (self.permission, "permission"),
            (self.question, "question"),
            (self.plan_approval, "plan_approval"),
            (self.queued_prompt, "queued_prompt"),
            (self.continuation_wait, "continuation_wait"),
            (self.active_delegation, "active_delegation"),
            (self.background_work, "background_work"),
            (self.host_work, "host_work"),
        ] {
            if blocked {
                names.push(name);
            }
        }
        names
    }
}

/// Owning token for host-side work not represented in `SessionState`.
///
/// The permit deliberately cannot be cloned. Explicit completion and `Drop`
/// both execute the same generation-fenced removal.
pub struct SharedHostWorkPermit {
    broker_index: Weak<Mutex<SharedSessionIndex>>,
    runtime: tokio::runtime::Handle,
    identity: Option<(String, u64, uuid::Uuid)>,
}

pub(crate) struct SharedSweepCandidate {
    record: Arc<Mutex<SharedSessionRecord>>,
    pub connection_id: String,
    pub generation: u64,
    pub kind: SharedSweepCandidateKind,
    failed_zero_since: Option<tokio::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedSweepCandidateKind {
    Ready,
    Failed,
    AbandonedEphemeral,
}

pub(crate) struct SharedClosingTransition {
    pub candidate: SharedSweepCandidate,
    pub events: Vec<crate::acp::types::AcpEvent>,
    pub force_abort: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SharedSweepReport {
    pub removed: bool,
    pub removed_count: usize,
    pub cleanup_incomplete: usize,
}

/// Authorizes one same-generation state preparation and driver replacement.
/// The opaque token prevents a stale or duplicate committer from publishing.
#[derive(Clone)]
pub(crate) struct RegisteredReplacementPermit {
    connection_id: String,
    generation: u64,
    previous_incarnation: String,
    token: String,
}

struct ActiveRegisteredReplacement {
    previous_incarnation: String,
    token: String,
}

impl Default for SharedSessionBroker {
    fn default() -> Self {
        let (index_epoch, _) = watch::channel(0);
        Self {
            index: Arc::new(Mutex::new(SharedSessionIndex::default())),
            index_epoch: Arc::new(index_epoch),
            metrics: Arc::new(SharedSessionMetrics::default()),
            accepting: Arc::new(AtomicBool::new(true)),
            lease_ttl_secs: Arc::new(AtomicU64::new(DEFAULT_CLIENT_LEASE_TTL.as_secs())),
            limits: BrokerLimits::default(),
            #[cfg(test)]
            idle_final_cas_barrier: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl SharedSessionBroker {
    pub fn metrics(&self) -> &SharedSessionMetrics {
        &self.metrics
    }

    fn lease_ttl(&self) -> Duration {
        Duration::from_secs(self.lease_ttl_secs.load(Ordering::Relaxed))
    }

    fn ensure_accepting(&self) -> Result<(), SharedSessionError> {
        if self.accepting.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(SharedSessionError::Closing)
        }
    }

    pub async fn begin_shutdown(&self) {
        self.accepting.swap(false, Ordering::AcqRel);
        let records: Vec<_> = self
            .index
            .lock()
            .await
            .sessions
            .iter()
            .map(|(key, record)| (key.clone(), record.clone()))
            .collect();
        let mut publications = Vec::new();

        for (key, record) in records {
            loop {
                let transition = {
                    let index = self.index.lock().await;
                    if !index
                        .sessions
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &record))
                    {
                        break;
                    }
                    let mut current = match record.try_lock() {
                        Ok(current) => current,
                        Err(_) => {
                            drop(index);
                            tokio::task::yield_now().await;
                            continue;
                        }
                    };
                    if current.phase == SharedSessionPhase::Closing {
                        break;
                    }
                    let public_state = current.state.clone();
                    let mut state = match public_state.as_ref() {
                        Some(state) => match state.try_write() {
                            Ok(state) => Some(state),
                            Err(_) => {
                                drop(current);
                                drop(index);
                                tokio::task::yield_now().await;
                                continue;
                            }
                        },
                        None => None,
                    };
                    let generation = current.generation;
                    let mut events =
                        fail_all_prompt_work(&mut current, "session_unavailable", &self.metrics);
                    current.begin_cleanup(tokio::time::Instant::now());
                    current.phase = SharedSessionPhase::Closing;
                    current.cleanup_complete = false;
                    current.idle_zero_since = None;
                    current.failed_zero_since = None;
                    current.host_owned_work.clear();
                    if let Some(state) = state.as_mut() {
                        update_public_shared_phase(state, generation, SharedSessionPhase::Closing);
                    }
                    let handles = match (current.state.as_ref(), current.emitter.as_ref()) {
                        (Some(state), Some(emitter)) => Some((state.clone(), emitter.clone())),
                        _ => None,
                    };
                    let registration = SharedRegistrationState {
                        phase: SharedSessionPhase::Closing,
                        state: current.state.clone(),
                        emitter: current.emitter.clone(),
                        driver_incarnation: current.driver_incarnation.clone(),
                    };
                    let registration_tx = current.registration_tx.clone();
                    let lifecycle_tx = current.lifecycle_tx.clone();
                    let notify = current.notify.clone();
                    events.push(crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                        generation,
                        phase: SharedSessionPhase::Closing,
                    });
                    drop(state);
                    drop(current);
                    drop(index);
                    Some((
                        registration_tx,
                        lifecycle_tx,
                        notify,
                        registration,
                        handles,
                        events,
                    ))
                };

                if let Some((
                    registration_tx,
                    lifecycle_tx,
                    notify,
                    registration,
                    handles,
                    events,
                )) = transition
                {
                    registration_tx.send_replace(registration);
                    lifecycle_tx.send_replace(SharedLifecycleState::Closing);
                    notify.notify_waiters();
                    if let Some((state, emitter)) = handles {
                        publications.push((state, emitter, events));
                    }
                }
                break;
            }
        }

        for (state, emitter, events) in publications {
            for event in events {
                emit_with_state(&state, &emitter, event).await;
            }
        }
    }

    pub(crate) fn configure_client_lease_ttl(&self, lease_ttl: Duration) {
        self.lease_ttl_secs
            .store(lease_ttl.as_secs(), Ordering::Relaxed);
    }

    pub async fn begin_host_work(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<SharedHostWorkPermit, SharedSessionError> {
        let permit_id = uuid::Uuid::new_v4();
        let notify = self
            .with_authoritative_record(connection_id, |record| {
                if record.generation != generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                if !matches!(
                    record.phase,
                    SharedSessionPhase::Bootstrapping | SharedSessionPhase::Ready
                ) {
                    return Err(SharedSessionError::SessionUnavailable);
                }
                record.host_owned_work.insert(permit_id);
                record.idle_zero_since = None;
                Ok(record.notify.clone())
            })
            .await?
            .ok_or(SharedSessionError::SessionUnavailable)?;
        notify.notify_one();
        Ok(SharedHostWorkPermit {
            broker_index: Arc::downgrade(&self.index),
            runtime: tokio::runtime::Handle::current(),
            identity: Some((connection_id.to_string(), generation, permit_id)),
        })
    }

    pub async fn end_host_work(&self, mut permit: SharedHostWorkPermit) -> bool {
        let Some(identity) = permit.identity.take() else {
            return false;
        };
        release_host_work(self.index.clone(), identity).await
    }

    pub(crate) async fn managed_connection_ids(&self) -> HashSet<String> {
        self.index
            .lock()
            .await
            .by_connection
            .keys()
            .cloned()
            .collect()
    }

    pub async fn diagnostics(&self) -> Vec<SharedSessionDiagnostic> {
        let records: Vec<_> = {
            let index = self.index.lock().await;
            index
                .sessions
                .iter()
                .map(|(key, record)| {
                    let conversation_id = match key {
                        SharedSessionKey::Conversation(id) => Some(*id),
                        SharedSessionKey::ExternalSession { .. }
                        | SharedSessionKey::Ephemeral(_) => None,
                    };
                    (conversation_id, record.clone())
                })
                .collect()
        };
        let mut diagnostics = Vec::with_capacity(records.len());
        for (conversation_id, record) in records {
            let (
                state,
                connection_id,
                generation,
                phase,
                agent_category,
                lease_count,
                queue_depth,
                queue_bytes,
                record_blockers,
                cleanup_state,
                bootstrap_duration_ms,
                cleanup_duration_ms,
            ) = {
                let current = record.lock().await;
                let now = tokio::time::Instant::now();
                let cleanup_state = match current.phase {
                    SharedSessionPhase::Closing
                    | SharedSessionPhase::Failed {
                        cleanup_complete: false,
                        ..
                    } => "in_progress",
                    SharedSessionPhase::Failed {
                        cleanup_complete: true,
                        ..
                    } => "complete",
                    SharedSessionPhase::Reserved
                    | SharedSessionPhase::Bootstrapping
                    | SharedSessionPhase::Ready => "not_started",
                };
                (
                    current.state.clone(),
                    current.connection_id.clone(),
                    current.generation,
                    current.phase.clone(),
                    bounded_agent_category(current.launch_identity.agent_type),
                    current.active_leases.len(),
                    current.waiting_prompts.len(),
                    current.waiting_bytes,
                    SharedIdleBlockers::from_record_only(&current),
                    cleanup_state,
                    duration_millis(current.bootstrap_duration(now)),
                    duration_millis(current.cleanup_duration(now)),
                )
            };
            let runtime_snapshot = match state {
                Some(state) => state.read().await.shared_runtime_work_snapshot(None),
                None => SharedRuntimeWorkSnapshot {
                    event_seq: 0,
                    status: crate::acp::types::ConnectionStatus::Disconnected,
                    turn_in_flight: false,
                    pending_permission_id: None,
                    pending_question_id: None,
                    pending_plan_approval_id: None,
                    continuation_wait: false,
                    active_delegations: 0,
                    background_outstanding: 0,
                    conversation_write_error: None,
                },
            };
            diagnostics.push(SharedSessionDiagnostic {
                connection_id,
                conversation_id,
                generation,
                phase,
                agent_category,
                lease_count,
                queue_depth,
                queue_bytes,
                idle_blockers: record_blockers
                    .with_runtime(&runtime_snapshot)
                    .stable_names(),
                cleanup_state,
                bootstrap_duration_ms,
                cleanup_duration_ms,
            });
        }
        diagnostics.sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
        diagnostics
    }

    pub(crate) async fn remove_shutdown_session(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> bool {
        loop {
            let mut index = self.index.lock().await;
            let Some(key) = index.by_connection.get(connection_id).cloned() else {
                return false;
            };
            let Some(record) = index.sessions.get(&key).cloned() else {
                return false;
            };
            let current = match record.try_lock() {
                Ok(current) => current,
                Err(_) => {
                    drop(index);
                    tokio::task::yield_now().await;
                    continue;
                }
            };
            if current.generation != generation || current.phase != SharedSessionPhase::Closing {
                return false;
            }
            let lifecycle_tx = current.lifecycle_tx.clone();
            let notify = current.notify.clone();
            let active_leases = current.active_leases.len();
            let waiting_prompts = current.waiting_prompts.len();
            let waiting_bytes = current.waiting_bytes;
            drop(current);
            index.remove_canonical_session(&key);
            index.by_connection.remove(connection_id);
            drop(index);
            self.index_epoch
                .send_modify(|epoch| *epoch = epoch.saturating_add(1));
            lifecycle_tx.send_replace(SharedLifecycleState::Removed);
            notify.notify_waiters();
            self.metrics.remove_active_leases(active_leases);
            self.metrics.remove_waiting(waiting_prompts, waiting_bytes);
            self.metrics.remove_live_session();
            return true;
        }
    }

    pub(crate) async fn evaluate_idle(
        &self,
        idle_grace: Option<Duration>,
        failed_grace: Duration,
    ) -> Vec<SharedSweepCandidate> {
        let records: Vec<_> = self
            .index
            .lock()
            .await
            .sessions
            .iter()
            .map(|(key, record)| (key.clone(), record.clone()))
            .collect();
        let now = tokio::time::Instant::now();
        let mut candidates = Vec::new();

        for (key, record) in records {
            let mut current = record.lock().await;
            if matches!(key, SharedSessionKey::Ephemeral(_))
                && matches!(
                    current.phase,
                    SharedSessionPhase::Reserved
                        | SharedSessionPhase::Bootstrapping
                        | SharedSessionPhase::Ready
                )
            {
                current.failed_zero_since = None;
                if current.has_broker_occupants() {
                    current.idle_zero_since = None;
                    continue;
                }
                let zero_since = current.idle_zero_since.get_or_insert(now);
                if now.duration_since(*zero_since) >= failed_grace {
                    candidates.push(SharedSweepCandidate {
                        record: record.clone(),
                        connection_id: current.connection_id.clone(),
                        generation: current.generation,
                        kind: SharedSweepCandidateKind::AbandonedEphemeral,
                        failed_zero_since: None,
                    });
                }
                continue;
            }
            match current.phase {
                SharedSessionPhase::Ready => {
                    current.failed_zero_since = None;
                    let Some(idle_grace) = idle_grace else {
                        current.idle_zero_since = None;
                        continue;
                    };
                    let Some(state) = current.state.clone() else {
                        current.idle_zero_since = None;
                        continue;
                    };
                    let state = state.read().await;
                    let snapshot = state.shared_runtime_work_snapshot(None);
                    if !SharedIdleBlockers::from_record(&current, &snapshot).is_empty() {
                        current.idle_zero_since = None;
                        continue;
                    }
                    let zero_since = current.idle_zero_since.get_or_insert(now);
                    if now.duration_since(*zero_since) >= idle_grace {
                        self.metrics.record_idle_candidate();
                        candidates.push(SharedSweepCandidate {
                            record: record.clone(),
                            connection_id: current.connection_id.clone(),
                            generation: current.generation,
                            kind: SharedSweepCandidateKind::Ready,
                            failed_zero_since: None,
                        });
                    }
                }
                SharedSessionPhase::Failed { .. }
                    if current.cleanup_complete && current.active_leases.is_empty() =>
                {
                    current.idle_zero_since = None;
                    let zero_since = *current.failed_zero_since.get_or_insert(now);
                    if now.duration_since(zero_since) >= failed_grace {
                        candidates.push(SharedSweepCandidate {
                            record: record.clone(),
                            connection_id: current.connection_id.clone(),
                            generation: current.generation,
                            kind: SharedSweepCandidateKind::Failed,
                            failed_zero_since: Some(zero_since),
                        });
                    }
                }
                _ => {
                    current.idle_zero_since = None;
                    current.failed_zero_since = None;
                }
            }
        }
        candidates
    }

    pub(crate) async fn begin_idle_reclaim(
        &self,
        candidate: SharedSweepCandidate,
        idle_grace: Duration,
    ) -> Option<SharedClosingTransition> {
        #[cfg(test)]
        self.wait_idle_final_cas_barrier_for_test().await;

        let mut record = candidate.record.lock().await;
        if record.connection_id != candidate.connection_id
            || record.generation != candidate.generation
            || record.phase != SharedSessionPhase::Ready
        {
            self.metrics.record_idle_cas_lost();
            return None;
        }
        let Some(state) = record.state.clone() else {
            record.idle_zero_since = None;
            self.metrics.record_idle_cas_lost();
            return None;
        };
        let mut state = state.write().await;
        let snapshot = state.shared_runtime_work_snapshot(None);
        let old_enough = record.idle_zero_since.is_some_and(|zero_since| {
            tokio::time::Instant::now().duration_since(zero_since) >= idle_grace
        });
        if !old_enough || !SharedIdleBlockers::from_record(&record, &snapshot).is_empty() {
            record.idle_zero_since = None;
            self.metrics.record_idle_cas_lost();
            return None;
        }

        record.phase = SharedSessionPhase::Closing;
        record.cleanup_complete = false;
        record.begin_cleanup(tokio::time::Instant::now());
        record.idle_zero_since = None;
        update_public_shared_phase(
            &mut state,
            candidate.generation,
            SharedSessionPhase::Closing,
        );
        let registration = SharedRegistrationState {
            phase: SharedSessionPhase::Closing,
            state: record.state.clone(),
            emitter: record.emitter.clone(),
            driver_incarnation: record.driver_incarnation.clone(),
        };
        let registration_tx = record.registration_tx.clone();
        let lifecycle_tx = record.lifecycle_tx.clone();
        let notify = record.notify.clone();
        drop(state);
        drop(record);

        registration_tx.send_replace(registration);
        lifecycle_tx.send_replace(SharedLifecycleState::Closing);
        notify.notify_waiters();
        let generation = candidate.generation;
        Some(SharedClosingTransition {
            candidate,
            events: vec![crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                generation,
                phase: SharedSessionPhase::Closing,
            }],
            force_abort: false,
        })
    }

    pub(crate) async fn begin_abandoned_ephemeral_reclaim(
        &self,
        candidate: SharedSweepCandidate,
        unoccupied_grace: Duration,
    ) -> Option<SharedClosingTransition> {
        loop {
            let index = self.index.lock().await;
            let key = index.by_connection.get(&candidate.connection_id).cloned()?;
            if !matches!(key, SharedSessionKey::Ephemeral(_)) {
                return None;
            }
            let authoritative = index
                .sessions
                .get(&key)
                .is_some_and(|record| Arc::ptr_eq(record, &candidate.record));
            if !authoritative {
                return None;
            }
            let mut record = match candidate.record.try_lock() {
                Ok(current) => current,
                Err(_) => {
                    drop(index);
                    tokio::task::yield_now().await;
                    continue;
                }
            };
            drop(index);
            if record.connection_id != candidate.connection_id
                || record.generation != candidate.generation
                || !matches!(
                    record.phase,
                    SharedSessionPhase::Reserved
                        | SharedSessionPhase::Bootstrapping
                        | SharedSessionPhase::Ready
                )
            {
                return None;
            }
            let old_enough = record.idle_zero_since.is_some_and(|zero_since| {
                tokio::time::Instant::now().duration_since(zero_since) >= unoccupied_grace
            });
            if !old_enough || record.has_broker_occupants() {
                record.idle_zero_since = None;
                return None;
            }

            let force_abort = record.phase != SharedSessionPhase::Ready;
            record.phase = SharedSessionPhase::Closing;
            record.cleanup_complete = false;
            record.begin_cleanup(tokio::time::Instant::now());
            record.idle_zero_since = None;
            if let Some(state) = record.state.clone() {
                let mut state = state.write().await;
                update_public_shared_phase(
                    &mut state,
                    candidate.generation,
                    SharedSessionPhase::Closing,
                );
            }
            let registration = SharedRegistrationState {
                phase: SharedSessionPhase::Closing,
                state: record.state.clone(),
                emitter: record.emitter.clone(),
                driver_incarnation: record.driver_incarnation.clone(),
            };
            let registration_tx = record.registration_tx.clone();
            let lifecycle_tx = record.lifecycle_tx.clone();
            let notify = record.notify.clone();
            drop(record);

            registration_tx.send_replace(registration);
            lifecycle_tx.send_replace(SharedLifecycleState::Closing);
            notify.notify_waiters();
            let generation = candidate.generation;
            return Some(SharedClosingTransition {
                candidate,
                events: vec![crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                    generation,
                    phase: SharedSessionPhase::Closing,
                }],
                force_abort,
            });
        }
    }

    pub(crate) async fn begin_termination(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<SharedClosingTransition, SharedSessionError> {
        let record = {
            let index = self.index.lock().await;
            match index.record_for_connection(connection_id).cloned() {
                Some(record) => record,
                None if index.is_replaced_connection(connection_id, generation) => {
                    return Err(SharedSessionError::GenerationStale)
                }
                None => return Err(SharedSessionError::SessionUnavailable),
            }
        };
        let mut current = record.lock().await;
        if current.generation != generation {
            return Err(SharedSessionError::GenerationStale);
        }
        let force_abort = match current.phase {
            SharedSessionPhase::Reserved => return Err(SharedSessionError::SessionUnavailable),
            SharedSessionPhase::Closing => return Err(SharedSessionError::Closing),
            SharedSessionPhase::Ready => false,
            SharedSessionPhase::Bootstrapping | SharedSessionPhase::Failed { .. } => true,
        };

        let public_state = current.state.clone();
        let mut state = match public_state.as_ref() {
            Some(state) => Some(state.write().await),
            None => None,
        };
        let mut events = fail_all_prompt_work(&mut current, "session_unavailable", &self.metrics);
        current.begin_cleanup(tokio::time::Instant::now());
        current.phase = SharedSessionPhase::Closing;
        current.cleanup_complete = false;
        current.idle_zero_since = None;
        current.failed_zero_since = None;
        current.host_owned_work.clear();
        if let Some(state) = state.as_mut() {
            update_public_shared_phase(state, generation, SharedSessionPhase::Closing);
        }
        let registration = SharedRegistrationState {
            phase: SharedSessionPhase::Closing,
            state: current.state.clone(),
            emitter: current.emitter.clone(),
            driver_incarnation: current.driver_incarnation.clone(),
        };
        let registration_tx = current.registration_tx.clone();
        let lifecycle_tx = current.lifecycle_tx.clone();
        let notify = current.notify.clone();
        drop(state);
        drop(current);

        registration_tx.send_replace(registration);
        lifecycle_tx.send_replace(SharedLifecycleState::Closing);
        notify.notify_waiters();
        events.push(crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
            generation,
            phase: SharedSessionPhase::Closing,
        });
        Ok(SharedClosingTransition {
            candidate: SharedSweepCandidate {
                record,
                connection_id: connection_id.to_string(),
                generation,
                kind: SharedSweepCandidateKind::Ready,
                failed_zero_since: None,
            },
            events,
            force_abort,
        })
    }

    pub(crate) async fn remove_sweep_candidate(&self, candidate: &SharedSweepCandidate) -> bool {
        loop {
            let mut index = self.index.lock().await;
            let Some(key) = index.by_connection.get(&candidate.connection_id).cloned() else {
                return false;
            };
            let authoritative = index
                .sessions
                .get(&key)
                .is_some_and(|record| Arc::ptr_eq(record, &candidate.record));
            if !authoritative {
                return false;
            }
            let current = match candidate.record.try_lock() {
                Ok(current) => current,
                Err(_) => {
                    drop(index);
                    tokio::task::yield_now().await;
                    continue;
                }
            };
            let removable = current.generation == candidate.generation
                && match candidate.kind {
                    SharedSweepCandidateKind::Ready
                    | SharedSweepCandidateKind::AbandonedEphemeral => {
                        current.phase == SharedSessionPhase::Closing
                    }
                    SharedSweepCandidateKind::Failed => {
                        matches!(
                            current.phase,
                            SharedSessionPhase::Failed {
                                cleanup_complete: true,
                                ..
                            }
                        ) && current.active_leases.is_empty()
                            && current.failed_zero_since == candidate.failed_zero_since
                    }
                };
            if !removable {
                return false;
            }
            let lifecycle_tx = current.lifecycle_tx.clone();
            let notify = current.notify.clone();
            let active_leases = current.active_leases.len();
            let waiting_prompts = current.waiting_prompts.len();
            let waiting_bytes = current.waiting_bytes;
            drop(current);
            index.remove_canonical_session(&key);
            index.by_connection.remove(&candidate.connection_id);
            drop(index);
            self.index_epoch
                .send_modify(|epoch| *epoch = epoch.saturating_add(1));
            lifecycle_tx.send_replace(SharedLifecycleState::Removed);
            notify.notify_waiters();
            self.metrics.remove_active_leases(active_leases);
            self.metrics.remove_waiting(waiting_prompts, waiting_bytes);
            self.metrics.remove_live_session();
            if matches!(
                candidate.kind,
                SharedSweepCandidateKind::Ready | SharedSweepCandidateKind::AbandonedEphemeral
            ) {
                self.metrics.record_idle_reclaimed();
            }
            return true;
        }
    }

    pub(crate) fn record_cleanup_incomplete(&self) {
        self.metrics.record_cleanup_incomplete();
    }

    pub(crate) fn record_cleanup_duration(&self, elapsed: Duration) {
        self.metrics.record_cleanup_duration(elapsed);
    }

    #[cfg(test)]
    pub(crate) fn install_idle_final_cas_barrier_for_test(&self, parties: usize) {
        *self
            .idle_final_cas_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some(Arc::new(tokio::sync::Barrier::new(parties)));
    }

    #[cfg(test)]
    async fn wait_idle_final_cas_barrier_for_test(&self) {
        let barrier = self
            .idle_final_cas_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
    }

    pub(crate) async fn enqueue_prompt(
        &self,
        request: SharedPromptRequest,
    ) -> Result<SharedPromptAdmission, SharedSessionError> {
        self.ensure_accepting()?;
        validate_prompt_request(&request)?;
        let canonical =
            canonical_prompt_bytes(&request).map_err(|_| SharedSessionError::SessionUnavailable)?;
        let payload_hash: [u8; 32] = Sha256::digest(&canonical).into();
        let identity = PromptIdentity {
            generation: request.guard.generation,
            client_instance_id: request.client_instance_id.clone(),
            client_request_id: request.client_request_id.clone(),
        };
        let waiting_bytes = canonical.len();
        let mut request = Some(request);
        let connection_id = request
            .as_ref()
            .expect("request available")
            .guard
            .connection_id
            .clone();

        let result = self
            .with_authoritative_record(&connection_id, |record| {
                self.ensure_accepting()?;
                let request_ref = request.as_ref().expect("request available");
                self.validate_prompt_guard(record, &request_ref.guard)?;
                if !matches!(
                    record.phase,
                    SharedSessionPhase::Bootstrapping | SharedSessionPhase::Ready
                ) {
                    return Err(SharedSessionError::SessionUnavailable);
                }

                if let Some(entry) = record.prompt_ledger.get(&identity) {
                    if entry.payload_hash != payload_hash {
                        return Err(SharedSessionError::IdempotencyKeyConflict);
                    }
                    return Ok(SharedPromptAdmission {
                        queue_item_id: entry.queue_item_id.clone(),
                        events: entry.admission_events.clone(),
                        publication: entry.admission_publication.clone(),
                        publication_invalidated: entry.admission_invalidated.clone(),
                        notify: record.notify.clone(),
                    });
                }
                if record.prompt_ledger.len() >= self.limits.max_prompt_ledger_entries {
                    return Err(SharedSessionError::PromptLedgerCapacityExceeded);
                }
                if record.waiting_prompts.len() >= self.limits.max_waiting_prompts
                    || record
                        .waiting_bytes
                        .checked_add(waiting_bytes)
                        .is_none_or(|bytes| bytes > self.limits.max_waiting_bytes)
                {
                    return Err(SharedSessionError::PromptQueueFull);
                }

                let request = request.take().expect("new admission consumes request");
                let queue_item_id = uuid::Uuid::new_v4().to_string();
                let enqueue_seq = record.next_enqueue_seq;
                record.next_enqueue_seq = record
                    .next_enqueue_seq
                    .checked_add(1)
                    .ok_or(SharedSessionError::SessionUnavailable)?;
                let summary = SharedQueuedPromptSummary::from_prompt(
                    queue_item_id.clone(),
                    enqueue_seq,
                    request.client_message_id.clone(),
                    &request.blocks,
                    request.capture.as_ref(),
                    request.submitted_at,
                    SharedQueuedPromptState::Queued,
                );
                record.waiting_bytes += waiting_bytes;
                record.waiting_prompts.push_back(QueuedPromptRecord {
                    identity: identity.clone(),
                    summary: summary.clone(),
                    blocks: request.blocks,
                    folder_id: request.folder_id,
                    conversation_id: request.conversation_id,
                    client_message_id: request.client_message_id,
                    capture: request.capture,
                    waiting_bytes,
                });
                let events = vec![
                    crate::acp::types::AcpEvent::PromptQueued {
                        generation: record.generation,
                        item: summary,
                    },
                    queue_depth_event(record),
                ];
                let publication = Arc::new(tokio::sync::OnceCell::new());
                let publication_invalidated = Arc::new(AtomicBool::new(false));
                record.prompt_ledger.insert(
                    identity.clone(),
                    PromptLedgerEntry {
                        payload_hash,
                        queue_item_id: queue_item_id.clone(),
                        enqueue_seq,
                        state: InternalPromptState::Queued,
                        frozen_result: None,
                        admission_events: events.clone(),
                        admission_publication: publication.clone(),
                        admission_invalidated: publication_invalidated.clone(),
                        admission_published: false,
                    },
                );
                self.metrics.record_enqueue(waiting_bytes);
                Ok(SharedPromptAdmission {
                    queue_item_id,
                    events,
                    publication,
                    publication_invalidated,
                    notify: record.notify.clone(),
                })
            })
            .await?
            .ok_or(SharedSessionError::SessionUnavailable);
        if result
            .as_ref()
            .is_err_and(SharedSessionError::is_capacity_error)
        {
            self.metrics.record_capacity_rejection();
        }
        result
    }

    pub(crate) async fn mark_prompt_admission_published(
        &self,
        connection_id: &str,
        generation: u64,
        queue_item_id: &str,
    ) -> Result<bool, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            let live_phase = matches!(
                record.phase,
                SharedSessionPhase::Bootstrapping | SharedSessionPhase::Ready
            );
            let entry = record
                .prompt_ledger
                .values_mut()
                .find(|entry| entry.queue_item_id == queue_item_id)
                .ok_or(SharedSessionError::QueueItemNotFound)?;
            if !live_phase
                || entry.state != InternalPromptState::Queued
                || entry.admission_invalidated.load(Ordering::Acquire)
            {
                return Ok(false);
            }
            entry.admission_published = true;
            Ok(true)
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn finalize_enqueue_response(
        &self,
        connection_id: &str,
        generation: u64,
        queue_item_id: &str,
    ) -> Result<PromptEnqueueResult, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            let entry = record
                .prompt_ledger
                .values_mut()
                .find(|entry| entry.queue_item_id == queue_item_id)
                .ok_or(SharedSessionError::QueueItemNotFound)?;
            if let Some(result) = entry.frozen_result.as_ref() {
                return Ok(result.clone());
            }
            let state = if entry.state == InternalPromptState::Queued {
                SharedQueuedPromptState::Queued
            } else {
                SharedQueuedPromptState::Dispatching
            };
            let result = PromptEnqueueResult {
                queue_item_id: entry.queue_item_id.clone(),
                enqueue_seq: entry.enqueue_seq,
                state,
            };
            entry.frozen_result = Some(result.clone());
            Ok(result)
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn cancel_queued_prompt(
        &self,
        guard: &SharedMutationGuard,
        queue_item_id: &str,
    ) -> Result<SharedPromptMutation, SharedSessionError> {
        self.with_authoritative_record(&guard.connection_id, |record| {
            self.validate_prompt_guard(record, guard)?;
            let identity = record
                .prompt_ledger
                .iter()
                .find_map(|(identity, entry)| {
                    (entry.queue_item_id == queue_item_id).then(|| identity.clone())
                })
                .ok_or(SharedSessionError::QueueItemNotFound)?;
            let (state, response_froze_as_dispatching) = record
                .prompt_ledger
                .get(&identity)
                .map(|entry| {
                    (
                        entry.state,
                        entry.frozen_result.as_ref().is_some_and(|result| {
                            result.state == SharedQueuedPromptState::Dispatching
                        }),
                    )
                })
                .ok_or(SharedSessionError::QueueItemNotFound)?;
            if state == InternalPromptState::Dispatching
                || response_froze_as_dispatching
                || record
                    .active_turn
                    .as_ref()
                    .is_some_and(|active| active.projection.queue_item_id == queue_item_id)
            {
                return Err(SharedSessionError::QueueItemAlreadyDispatching);
            }
            if state != InternalPromptState::Queued {
                return Err(SharedSessionError::QueueItemNotFound);
            }
            let position = record
                .waiting_prompts
                .iter()
                .position(|queued| queued.summary.queue_item_id == queue_item_id)
                .ok_or(SharedSessionError::QueueItemNotFound)?;
            let queued = record
                .waiting_prompts
                .remove(position)
                .ok_or(SharedSessionError::QueueItemNotFound)?;
            record.waiting_bytes = record
                .waiting_bytes
                .checked_sub(queued.waiting_bytes)
                .expect("queued prompt bytes are included in waiting total");
            record
                .prompt_ledger
                .get_mut(&identity)
                .expect("queued item has ledger entry")
                .invalidate_admission(InternalPromptState::Cancelled);
            self.metrics.record_cancel(queued.waiting_bytes);
            Ok(SharedPromptMutation {
                events: vec![
                    crate::acp::types::AcpEvent::PromptQueueItemCancelled {
                        generation: record.generation,
                        queue_item_id: queue_item_id.to_string(),
                    },
                    queue_depth_event(record),
                ],
                notify: record.notify.clone(),
            })
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn claim_dispatchable_head(
        &self,
        connection_id: &str,
        generation: u64,
        turn_id: &str,
        snapshot: &SharedRuntimeWorkSnapshot,
    ) -> Result<DispatchHeadDecision, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            if record.phase != SharedSessionPhase::Ready
                || snapshot.status != crate::acp::types::ConnectionStatus::Connected
                || snapshot.turn_in_flight
                || record.active_turn.is_some()
                || record.interactions.has_any()
                || snapshot.pending_permission_id.is_some()
                || snapshot.pending_question_id.is_some()
                || snapshot.pending_plan_approval_id.is_some()
                || snapshot.continuation_wait
                || snapshot.active_delegations != 0
                || snapshot.background_outstanding != 0
                || !record.host_owned_work.is_empty()
                || record.waiting_prompts.is_empty()
            {
                return Ok(DispatchHeadDecision::Blocked);
            }

            let head_is_published = record
                .waiting_prompts
                .front()
                .and_then(|queued| record.prompt_ledger.get(&queued.identity))
                .is_some_and(|entry| entry.admission_published);
            if !head_is_published {
                return Ok(DispatchHeadDecision::Blocked);
            }

            let queued = record
                .waiting_prompts
                .pop_front()
                .expect("non-empty queue has head");
            record.waiting_bytes = record
                .waiting_bytes
                .checked_sub(queued.waiting_bytes)
                .expect("FIFO head bytes are included in waiting total");
            self.metrics.remove_waiting(1, queued.waiting_bytes);
            if let Some(error_code) = snapshot.conversation_write_error {
                record
                    .prompt_ledger
                    .get_mut(&queued.identity)
                    .expect("queued item has ledger entry")
                    .state = InternalPromptState::Failed;
                self.metrics.record_queue_items_failed(1);
                return Ok(DispatchHeadDecision::Failed(FailedSharedPrompt {
                    events: vec![
                        crate::acp::types::AcpEvent::PromptQueueItemFailed {
                            generation,
                            queue_item_id: queued.summary.queue_item_id,
                            error_code: error_code.into(),
                        },
                        queue_depth_event(record),
                    ],
                    notify: record.notify.clone(),
                }));
            }

            self.metrics.record_dispatch();
            record
                .prompt_ledger
                .get_mut(&queued.identity)
                .expect("queued item has ledger entry")
                .state = InternalPromptState::Dispatching;
            let projection = SharedActiveTurnProjection {
                turn_id: turn_id.to_string(),
                queue_item_id: queued.summary.queue_item_id.clone(),
                enqueue_seq: queued.summary.enqueue_seq,
                client_message_id: queued.client_message_id.clone(),
                stop_requested: false,
            };
            record.active_turn = Some(BrokerActiveTurn {
                identity: queued.identity,
                projection: projection.clone(),
                stop_admission: StopAdmissionState::Open,
            });
            Ok(DispatchHeadDecision::Claimed(ClaimedSharedPrompt {
                blocks: queued.blocks,
                folder_id: queued.folder_id,
                conversation_id: queued.conversation_id,
                client_message_id: queued.client_message_id,
                capture: queued.capture,
                events: vec![
                    crate::acp::types::AcpEvent::PromptDispatchStarted {
                        generation,
                        turn: projection,
                    },
                    queue_depth_event(record),
                ],
            }))
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn fail_claimed_item(
        &self,
        connection_id: &str,
        generation: u64,
        turn_id: &str,
        error_code: &'static str,
    ) -> Result<FailedSharedPrompt, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            let active = record
                .active_turn
                .as_ref()
                .filter(|active| active.projection.turn_id == turn_id)
                .ok_or(SharedSessionError::StaleTurn)?;
            let identity = active.identity.clone();
            let queue_item_id = active.projection.queue_item_id.clone();
            record
                .prompt_ledger
                .get_mut(&identity)
                .expect("active turn has ledger entry")
                .state = InternalPromptState::Failed;
            let mut active = record
                .active_turn
                .take()
                .expect("active turn remains present");
            active.resolve_stop_waiters_as_requested();
            record.interactions.clear();
            self.metrics.record_queue_items_failed(1);
            Ok(FailedSharedPrompt {
                events: vec![
                    crate::acp::types::AcpEvent::PromptQueueItemFailed {
                        generation,
                        queue_item_id,
                        error_code: error_code.to_string(),
                    },
                    crate::acp::types::AcpEvent::SharedTurnSettled {
                        generation,
                        turn_id: turn_id.to_string(),
                        outcome: SharedTurnOutcome::Failed,
                    },
                ],
                notify: record.notify.clone(),
            })
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    #[cfg(test)]
    pub(crate) async fn settle_active_turn(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        stop_reason: &str,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.settle_active_turn_at_seq(
            connection_id,
            generation,
            driver_incarnation,
            stop_reason,
            0,
        )
        .await
    }

    pub(crate) async fn settle_active_turn_at_seq(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        stop_reason: &str,
        event_seq: u64,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation
                || record.driver_incarnation.as_deref() != Some(driver_incarnation)
            {
                return Ok(Vec::new());
            }
            let Some(active) = record.active_turn.take() else {
                return Ok(Vec::new());
            };
            let mut active = active;
            active.resolve_stop_waiters_as_requested();
            record.interactions.clear_at(event_seq);
            let outcome = if active.projection.stop_requested {
                SharedTurnOutcome::Cancelled
            } else if stop_reason == "end_turn" {
                SharedTurnOutcome::Completed
            } else {
                SharedTurnOutcome::Failed
            };
            let state = match outcome {
                SharedTurnOutcome::Completed => InternalPromptState::Completed,
                SharedTurnOutcome::Cancelled => InternalPromptState::Cancelled,
                SharedTurnOutcome::Failed => InternalPromptState::Failed,
            };
            record
                .prompt_ledger
                .get_mut(&active.identity)
                .expect("active turn has ledger entry")
                .state = state;
            if outcome == SharedTurnOutcome::Failed {
                self.metrics.record_queue_items_failed(1);
            }
            record.notify.notify_one();
            Ok(vec![crate::acp::types::AcpEvent::SharedTurnSettled {
                generation,
                turn_id: active.projection.turn_id,
                outcome,
            }])
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn fail_live_session(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        error_code: &'static str,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation
                || record.driver_incarnation.as_deref() != Some(driver_incarnation)
            {
                return Ok(Vec::new());
            }
            // While bootstrapping, only the typed route-bootstrap settler may
            // classify a terminal driver outcome. The runtime monitor can race
            // that outcome, but must not collapse a companion failure or an
            // allowed fallback into the generic session-unavailable state.
            if record.phase != SharedSessionPhase::Ready {
                return Ok(Vec::new());
            }
            Ok(fail_live_session_record(record, error_code, &self.metrics))
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    #[cfg(test)]
    pub(crate) async fn observe_interaction(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        kind: SharedInteractionKind,
        interaction_id: &str,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.observe_interaction_at_seq(
            connection_id,
            generation,
            driver_incarnation,
            kind,
            interaction_id,
            0,
        )
        .await
    }

    pub(crate) async fn observe_interaction_at_seq(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        kind: SharedInteractionKind,
        interaction_id: &str,
        event_seq: u64,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation
                || record.driver_incarnation.as_deref() != Some(driver_incarnation)
            {
                return Err(SharedSessionError::GenerationStale);
            }
            record
                .interactions
                .set_pending(kind, interaction_id, event_seq);
            record.notify.notify_one();
            Ok(Vec::new())
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    #[cfg(test)]
    pub(crate) async fn observe_interaction_resolved(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        kind: SharedInteractionKind,
        interaction_id: &str,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.observe_interaction_resolved_at_seq(
            connection_id,
            generation,
            driver_incarnation,
            kind,
            interaction_id,
            0,
        )
        .await
    }

    pub(crate) async fn observe_interaction_resolved_at_seq(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        kind: SharedInteractionKind,
        interaction_id: &str,
        event_seq: u64,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation
                || record.driver_incarnation.as_deref() != Some(driver_incarnation)
            {
                return Err(SharedSessionError::GenerationStale);
            }
            record
                .interactions
                .resolve_matching(kind, interaction_id, event_seq);
            record.notify.notify_one();
            Ok(Vec::new())
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn claim_interaction(
        &self,
        guard: &SharedMutationGuard,
        kind: SharedInteractionKind,
        interaction_id: &str,
    ) -> Result<SharedInteractionClaim, SharedSessionError> {
        self.ensure_accepting()?;
        let result = self
            .with_authoritative_record(&guard.connection_id, |record| {
                self.ensure_accepting()?;
                self.validate_prompt_guard(record, guard)?;
                if record.interaction_claims.contains_key(&kind) {
                    return Err(SharedSessionError::InteractionAlreadyResolved);
                }
                {
                    let interaction = record
                        .interactions
                        .get_mut(kind)
                        .as_mut()
                        .filter(|interaction| interaction.id == interaction_id)
                        .ok_or(SharedSessionError::InteractionAlreadyResolved)?;
                    if interaction.admission != InteractionAdmissionState::Pending {
                        return Err(SharedSessionError::InteractionAlreadyResolved);
                    }
                    interaction.admission = InteractionAdmissionState::Resolving;
                }
                let claim_id = uuid::Uuid::new_v4();
                record.interaction_claims.insert(
                    kind,
                    ActiveInteractionClaim {
                        interaction_id: interaction_id.to_string(),
                        claim_id,
                    },
                );
                Ok(SharedInteractionClaim {
                    connection_id: guard.connection_id.clone(),
                    generation: guard.generation,
                    kind,
                    interaction_id: interaction_id.to_string(),
                    claim_id,
                })
            })
            .await;
        let result = match result {
            Ok(result) => {
                self.require_authoritative_result(&guard.connection_id, guard.generation, result)
                    .await
            }
            Err(error) => Err(error),
        };
        if matches!(result, Err(SharedSessionError::InteractionAlreadyResolved)) {
            self.metrics.record_interaction_stale();
        }
        result
    }

    pub(crate) async fn complete_interaction(
        &self,
        claim: &SharedInteractionClaim,
    ) -> Result<(), SharedSessionError> {
        let result = self
            .with_authoritative_record(&claim.connection_id, |record| {
                if record.generation != claim.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                let completed = record
                    .interaction_claims
                    .get(&claim.kind)
                    .is_some_and(|active| {
                        active.interaction_id == claim.interaction_id
                            && active.claim_id == claim.claim_id
                    });
                if completed {
                    record.interaction_claims.remove(&claim.kind);
                    if let Some(interaction) = record.interactions.get_mut(claim.kind).as_mut() {
                        if interaction.id == claim.interaction_id {
                            interaction.admission = InteractionAdmissionState::Resolved;
                        }
                    }
                }
                Ok(completed)
            })
            .await?;
        let completed = self
            .require_authoritative_result(&claim.connection_id, claim.generation, result)
            .await?;
        if completed {
            self.metrics.record_interaction_winner();
        }
        Ok(())
    }

    pub(crate) async fn complete_interaction_as_stale(
        &self,
        claim: &SharedInteractionClaim,
    ) -> Result<(), SharedSessionError> {
        let result = self
            .with_authoritative_record(&claim.connection_id, |record| {
                if record.generation != claim.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                let completed = record
                    .interaction_claims
                    .get(&claim.kind)
                    .is_some_and(|active| {
                        active.interaction_id == claim.interaction_id
                            && active.claim_id == claim.claim_id
                    });
                if completed {
                    record.interaction_claims.remove(&claim.kind);
                    if let Some(interaction) = record.interactions.get_mut(claim.kind).as_mut() {
                        if interaction.id == claim.interaction_id {
                            interaction.admission = InteractionAdmissionState::Resolved;
                        }
                    }
                }
                Ok(completed)
            })
            .await?;
        let completed = self
            .require_authoritative_result(&claim.connection_id, claim.generation, result)
            .await?;
        if completed {
            self.metrics.record_interaction_stale();
        }
        Ok(())
    }

    pub(crate) async fn release_interaction_claim(
        &self,
        claim: &SharedInteractionClaim,
    ) -> Result<(), SharedSessionError> {
        let result = self
            .with_authoritative_record(&claim.connection_id, |record| {
                if record.generation != claim.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                let owns_claim = record
                    .interaction_claims
                    .get(&claim.kind)
                    .is_some_and(|active| {
                        active.interaction_id == claim.interaction_id
                            && active.claim_id == claim.claim_id
                    });
                if owns_claim {
                    record.interaction_claims.remove(&claim.kind);
                    if let Some(interaction) = record.interactions.get_mut(claim.kind).as_mut() {
                        if interaction.id == claim.interaction_id
                            && interaction.admission == InteractionAdmissionState::Resolving
                        {
                            interaction.admission = InteractionAdmissionState::Pending;
                        }
                    }
                }
                Ok(())
            })
            .await?;
        self.require_authoritative_result(&claim.connection_id, claim.generation, result)
            .await
    }

    pub(crate) async fn claim_stop_request(
        &self,
        request: &SharedStopRequest,
    ) -> Result<SharedStopClaimDecision, SharedSessionError> {
        self.ensure_accepting()?;
        let result = self
            .with_authoritative_record(&request.guard.connection_id, |record| {
                self.ensure_accepting()?;
                self.validate_prompt_guard(record, &request.guard)?;
                let active = record
                    .active_turn
                    .as_mut()
                    .filter(|active| active.projection.turn_id == request.turn_id)
                    .ok_or(SharedSessionError::StaleTurn)?;
                match &active.stop_admission {
                    StopAdmissionState::Open => {
                        let (result_tx, _) = watch::channel(None);
                        active.stop_admission = StopAdmissionState::Resolving { result_tx };
                        active.projection.stop_requested = true;
                        Ok(SharedStopClaimDecision::Claimed(SharedStopClaim {
                            connection_id: request.guard.connection_id.clone(),
                            generation: request.guard.generation,
                            turn_id: request.turn_id.clone(),
                        }))
                    }
                    StopAdmissionState::Resolving { result_tx } => {
                        Ok(SharedStopClaimDecision::Resolving(result_tx.subscribe()))
                    }
                    StopAdmissionState::Requested => Ok(SharedStopClaimDecision::Requested),
                }
            })
            .await;
        let result = match result {
            Ok(result) => {
                self.require_authoritative_result(
                    &request.guard.connection_id,
                    request.guard.generation,
                    result,
                )
                .await
            }
            Err(error) => Err(error),
        };
        if matches!(result, Err(SharedSessionError::StaleTurn)) {
            self.metrics.record_stale_stop();
        }
        result
    }

    pub(crate) async fn complete_stop_request(
        &self,
        claim: &SharedStopClaim,
    ) -> Result<(), SharedSessionError> {
        let result = self
            .with_authoritative_record(&claim.connection_id, |record| {
                if record.generation != claim.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                let Some(active) = record
                    .active_turn
                    .as_mut()
                    .filter(|active| active.projection.turn_id == claim.turn_id)
                else {
                    return Ok(());
                };
                active.complete_stop_request();
                Ok(())
            })
            .await?;
        self.require_authoritative_result(&claim.connection_id, claim.generation, result)
            .await
    }

    pub(crate) async fn validate_stop_claim(
        &self,
        claim: &SharedStopClaim,
    ) -> Result<(), SharedSessionError> {
        let result = self
            .with_authoritative_record(&claim.connection_id, |record| {
                if record.generation != claim.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                let active = record
                    .active_turn
                    .as_ref()
                    .filter(|active| active.projection.turn_id == claim.turn_id)
                    .ok_or(SharedSessionError::StaleTurn)?;
                if !matches!(active.stop_admission, StopAdmissionState::Resolving { .. }) {
                    return Err(SharedSessionError::StaleTurn);
                }
                Ok(())
            })
            .await;
        let result = match result {
            Ok(result) => {
                self.require_authoritative_result(&claim.connection_id, claim.generation, result)
                    .await
            }
            Err(error) => Err(error),
        };
        if matches!(result, Err(SharedSessionError::StaleTurn)) {
            self.metrics.record_stale_stop();
        }
        result
    }

    pub(crate) async fn release_stop_request(
        &self,
        claim: &SharedStopClaim,
    ) -> Result<(), SharedSessionError> {
        let result = self
            .with_authoritative_record(&claim.connection_id, |record| {
                if record.generation != claim.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                let Some(active) = record
                    .active_turn
                    .as_mut()
                    .filter(|active| active.projection.turn_id == claim.turn_id)
                else {
                    return Ok(());
                };
                active.release_stop_request();
                Ok(())
            })
            .await?;
        self.require_authoritative_result(&claim.connection_id, claim.generation, result)
            .await
    }

    pub(crate) async fn reconcile_runtime_snapshot(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
        snapshot: &SharedRuntimeWorkSnapshot,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation
                || record.driver_incarnation.as_deref() != Some(driver_incarnation)
            {
                return Err(SharedSessionError::GenerationStale);
            }
            record.interactions.reconcile_snapshot(snapshot);
            if matches!(
                snapshot.status,
                crate::acp::types::ConnectionStatus::Disconnected
                    | crate::acp::types::ConnectionStatus::Error
            ) && record.phase == SharedSessionPhase::Ready
            {
                return Ok(fail_live_session_record(
                    record,
                    "session_unavailable",
                    &self.metrics,
                ));
            }
            Ok(Vec::new())
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn runtime_subscription(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<SharedRuntimeSubscription, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            Ok(SharedRuntimeSubscription {
                notify: record.notify.clone(),
                lifecycle: record.lifecycle_tx.subscribe(),
                registration: record.registration_tx.subscribe(),
            })
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn generation_for_connection(&self, connection_id: &str) -> Option<u64> {
        self.with_authoritative_record(connection_id, |record| Ok(record.generation))
            .await
            .ok()
            .flatten()
    }

    pub(crate) async fn notify_dispatcher(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<(), SharedSessionError> {
        let notify = self
            .with_authoritative_record(connection_id, |record| {
                if record.generation != generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                Ok(record.notify.clone())
            })
            .await?
            .ok_or(SharedSessionError::SessionUnavailable)?;
        notify.notify_one();
        Ok(())
    }

    pub async fn bind_conversation_key(
        &self,
        connection_id: &str,
        generation: u64,
        conversation_id: i32,
    ) -> Result<(), SharedSessionError> {
        let destination_key = SharedSessionKey::Conversation(conversation_id);
        loop {
            let mut index = self.index.lock().await;
            let Some(source_key) = index.by_connection.get(connection_id).cloned() else {
                return Err(SharedSessionError::SessionUnavailable);
            };
            let Some(source) = index.sessions.get(&source_key).cloned() else {
                return Err(SharedSessionError::SessionUnavailable);
            };
            let Ok(source_record) = source.try_lock() else {
                drop(index);
                tokio::task::yield_now().await;
                continue;
            };
            if source_record.generation != generation
                || source_record.connection_id != connection_id
            {
                return Err(SharedSessionError::GenerationStale);
            }
            if let SharedSessionKey::Conversation(current_conversation_id) = &source_key {
                return if *current_conversation_id == conversation_id {
                    Ok(())
                } else {
                    Err(SharedSessionError::ConversationKeyConflict)
                };
            }

            if let Some(destination) = index.sessions.get(&destination_key).cloned() {
                if !Arc::ptr_eq(&source, &destination) {
                    let Ok(destination_record) = destination.try_lock() else {
                        drop(source_record);
                        drop(index);
                        tokio::task::yield_now().await;
                        continue;
                    };
                    if !Self::is_replaceable_conversation_destination(&destination_record) {
                        return Err(SharedSessionError::ConversationKeyConflict);
                    }
                    self.account_replaced_conversation_destination(&mut index, &destination_record);
                    index.remove_aliases_for_canonical(&destination_key);
                }
            }

            index.sessions.remove(&source_key);
            index
                .sessions
                .insert(destination_key.clone(), source.clone());
            index
                .by_connection
                .insert(connection_id.to_string(), destination_key.clone());
            if matches!(&source_key, SharedSessionKey::ExternalSession { .. }) {
                index.insert_alias(source_key, destination_key);
            }
            source_record.notify.notify_waiters();
            self.index_epoch
                .send_modify(|epoch| *epoch = epoch.saturating_add(1));
            return Ok(());
        }
    }

    pub(crate) async fn bind_conversation_key_guarded(
        &self,
        guard: &SharedMutationGuard,
        conversation_id: i32,
        folder_id: i32,
    ) -> Result<(), SharedSessionError> {
        let destination_key = SharedSessionKey::Conversation(conversation_id);
        loop {
            let mut index = self.index.lock().await;
            let Some(source_key) = index.by_connection.get(&guard.connection_id).cloned() else {
                if index.is_replaced_connection(&guard.connection_id, guard.generation) {
                    return Err(SharedSessionError::GenerationStale);
                }
                return Err(SharedSessionError::SessionUnavailable);
            };
            let Some(source) = index.sessions.get(&source_key).cloned() else {
                return Err(SharedSessionError::SessionUnavailable);
            };
            let Ok(mut source_record) = source.try_lock() else {
                drop(index);
                tokio::task::yield_now().await;
                continue;
            };
            self.validate_prompt_guard(&mut source_record, guard)?;
            if !matches!(
                source_record.phase,
                SharedSessionPhase::Bootstrapping | SharedSessionPhase::Ready
            ) {
                return Err(SharedSessionError::SessionUnavailable);
            }
            if let SharedSessionKey::Conversation(current) = source_key {
                return if current == conversation_id {
                    Ok(())
                } else {
                    Err(SharedSessionError::ConversationKeyConflict)
                };
            }

            let destination = index.sessions.get(&destination_key).cloned();
            let destination_record = match destination.as_ref() {
                Some(destination) if !Arc::ptr_eq(&source, destination) => {
                    let Ok(destination_record) = destination.try_lock() else {
                        drop(source_record);
                        drop(index);
                        tokio::task::yield_now().await;
                        continue;
                    };
                    if !Self::is_replaceable_conversation_destination(&destination_record) {
                        return Err(SharedSessionError::ConversationKeyConflict);
                    }
                    Some(destination_record)
                }
                _ => None,
            };
            let Some(public_state) = source_record.state.clone() else {
                return Err(SharedSessionError::SessionUnavailable);
            };
            let Ok(mut public_state) = public_state.try_write() else {
                drop(destination_record);
                drop(source_record);
                drop(index);
                tokio::task::yield_now().await;
                continue;
            };
            if public_state
                .conversation_id
                .is_some_and(|current| current != conversation_id)
            {
                return Err(SharedSessionError::ConversationKeyConflict);
            }
            if public_state
                .folder_id
                .is_some_and(|current| current != folder_id)
            {
                return Err(SharedSessionError::InvalidField { field: "folder_id" });
            }

            if let Some(destination_record) = destination_record {
                self.account_replaced_conversation_destination(&mut index, &destination_record);
                index.remove_aliases_for_canonical(&destination_key);
            }
            index.sessions.remove(&source_key);
            index
                .sessions
                .insert(destination_key.clone(), source.clone());
            index
                .by_connection
                .insert(guard.connection_id.clone(), destination_key.clone());
            if matches!(&source_key, SharedSessionKey::ExternalSession { .. }) {
                index.insert_alias(source_key, destination_key);
            }
            public_state.conversation_id = Some(conversation_id);
            public_state.folder_id = Some(folder_id);
            source_record.notify.notify_waiters();
            self.index_epoch
                .send_modify(|epoch| *epoch = epoch.saturating_add(1));
            return Ok(());
        }
    }

    /// Validate a mutation guard against the current generation and lease.
    ///
    /// The broker index is authoritative even when the manager connection map
    /// has already removed a failed connection. A replacement tombstone is
    /// therefore surfaced as a generation fence rather than a generic lease
    /// miss for mutation callers.
    pub async fn validate_guard(
        &self,
        guard: &SharedMutationGuard,
    ) -> Result<(), SharedSessionError> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(&guard.connection_id) else {
                    if index.is_replaced_connection(&guard.connection_id, guard.generation) {
                        return Err(SharedSessionError::GenerationStale);
                    }
                    return Err(SharedSessionError::LeaseMissing);
                };
                let result = match record.try_lock() {
                    Ok(mut record) => {
                        if record.generation != guard.generation {
                            return Err(SharedSessionError::GenerationStale);
                        }
                        let expired = record.prune_expired_leases(tokio::time::Instant::now());
                        self.metrics.remove_active_leases(expired.len());
                        self.metrics.record_lease_expired(expired.len());
                        if record
                            .active_leases
                            .values()
                            .any(|lease| lease.lease_id == guard.lease_id)
                        {
                            return Ok(());
                        }
                        return if record
                            .expired_leases
                            .iter()
                            .any(|lease_id| lease_id == &guard.lease_id)
                        {
                            Err(SharedSessionError::LeaseExpired)
                        } else {
                            Err(SharedSessionError::LeaseMissing)
                        };
                    }
                    Err(_) => true,
                };
                result
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Validate a WebSocket fence and return the binding used by its
    /// subscription. The `(None, None)` result is deliberately represented by
    /// `ConnectionGone`: callers may then attempt the legacy manager path,
    /// while any fenced value for an unknown id is rejected fail-closed.
    pub async fn validate_and_bind_lease(
        &self,
        connection_id: &str,
        generation: Option<u64>,
        lease_id: Option<&str>,
    ) -> Result<LeaseSocketBinding, DetachReason> {
        self.validate_and_bind_lease_inner(connection_id, generation, lease_id)
            .await
            .map(|(binding, _)| binding)
    }

    /// Validate a lease and return the broker-retained public state captured in
    /// the same map -> record critical section. This prevents a generation
    /// replacement between lease validation and state lookup from being
    /// misclassified as a legacy `connection_gone` attach.
    pub(crate) async fn validate_and_bind_lease_with_state(
        &self,
        connection_id: &str,
        generation: Option<u64>,
        lease_id: Option<&str>,
    ) -> Result<(LeaseSocketBinding, Arc<RwLock<SessionState>>), DetachReason> {
        let (binding, state) = self
            .validate_and_bind_lease_inner(connection_id, generation, lease_id)
            .await?;
        state
            .map(|state| (binding, state))
            .ok_or(DetachReason::ConnectionGone)
    }

    async fn validate_and_bind_lease_inner(
        &self,
        connection_id: &str,
        generation: Option<u64>,
        lease_id: Option<&str>,
    ) -> Result<(LeaseSocketBinding, Option<Arc<RwLock<SessionState>>>), DetachReason> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(connection_id) else {
                    if index.is_replaced_connection(connection_id, generation.unwrap_or_default())
                        || (generation.is_none()
                            && lease_id.is_none()
                            && index.has_replaced_connection(connection_id))
                    {
                        return Err(DetachReason::SessionReplaced);
                    }
                    return if generation.is_none() && lease_id.is_none() {
                        Err(DetachReason::ConnectionGone)
                    } else {
                        Err(DetachReason::GenerationStale)
                    };
                };
                let Some(generation) = generation else {
                    return Err(DetachReason::GenerationStale);
                };
                let Some(lease_id) = lease_id else {
                    return Err(DetachReason::GenerationStale);
                };
                let result = match record.try_lock() {
                    Ok(mut record) => {
                        if record.generation != generation {
                            return Err(DetachReason::GenerationStale);
                        }
                        let expired = record.prune_expired_leases(tokio::time::Instant::now());
                        self.metrics.remove_active_leases(expired.len());
                        self.metrics.record_lease_expired(expired.len());
                        let Some(lease) = record
                            .active_leases
                            .values()
                            .find(|lease| lease.lease_id == lease_id)
                        else {
                            return Err(
                                if record
                                    .expired_leases
                                    .iter()
                                    .any(|expired_id| expired_id == lease_id)
                                {
                                    DetachReason::LeaseExpired
                                } else {
                                    DetachReason::LeaseMissing
                                },
                            );
                        };
                        return Ok((
                            LeaseSocketBinding {
                                connection_id: record.connection_id.clone(),
                                generation: record.generation,
                                lease_id: lease.lease_id.clone(),
                                lease_expires_at: lease.expires_at_utc,
                            },
                            record.state.clone(),
                        ));
                    }
                    Err(_) => true,
                };
                result
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Renew each distinct binding in input order. The caller owns any
    /// subscription fan-out; this method intentionally returns one outcome for
    /// every input item so transport retries remain deterministic.
    pub async fn renew_leases(&self, bindings: &[LeaseSocketBinding]) -> Vec<LeaseRenewalOutcome> {
        let mut outcomes = Vec::with_capacity(bindings.len());
        for binding in bindings {
            outcomes.push(self.renew_lease(binding).await);
        }
        outcomes
    }

    async fn renew_lease(&self, binding: &LeaseSocketBinding) -> LeaseRenewalOutcome {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(&binding.connection_id) else {
                    return if index
                        .is_replaced_connection(&binding.connection_id, binding.generation)
                    {
                        LeaseRenewalOutcome::Detached(DetachReason::SessionReplaced)
                    } else {
                        LeaseRenewalOutcome::Detached(DetachReason::LeaseMissing)
                    };
                };
                let result = match record.try_lock() {
                    Ok(mut record) => {
                        if record.generation != binding.generation {
                            return LeaseRenewalOutcome::Detached(DetachReason::GenerationStale);
                        }
                        let now = tokio::time::Instant::now();
                        let now_utc = Utc::now();
                        let expired = record.prune_expired_leases(now);
                        self.metrics.remove_active_leases(expired.len());
                        self.metrics.record_lease_expired(expired.len());
                        let connection_id = record.connection_id.clone();
                        let generation = record.generation;
                        let Some(lease) = record
                            .active_leases
                            .values_mut()
                            .find(|lease| lease.lease_id == binding.lease_id)
                        else {
                            return if record
                                .expired_leases
                                .iter()
                                .any(|expired_id| expired_id == &binding.lease_id)
                            {
                                LeaseRenewalOutcome::Detached(DetachReason::LeaseExpired)
                            } else {
                                LeaseRenewalOutcome::Detached(DetachReason::LeaseMissing)
                            };
                        };
                        let lease_ttl = self.lease_ttl();
                        let expires_at = now + lease_ttl;
                        let expires_at_utc = now_utc
                            + chrono::Duration::from_std(lease_ttl)
                                .expect("shared session lease TTL must fit chrono::Duration");
                        lease.expires_at = expires_at;
                        lease.expires_at_utc = expires_at_utc;
                        let lease_id = lease.lease_id.clone();
                        let lease_expires_at = lease.expires_at_utc;
                        return LeaseRenewalOutcome::Renewed(LeaseSocketBinding {
                            connection_id,
                            generation,
                            lease_id,
                            lease_expires_at,
                        });
                    }
                    Err(_) => true,
                };
                result
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Release only the matching active lease on the current generation.
    /// Releasing an already-expired or already-released id is an idempotent
    /// `Ok(false)` and does not create another secret-bearing tombstone.
    pub async fn release_lease(
        &self,
        guard: &SharedMutationGuard,
    ) -> Result<bool, SharedSessionError> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(&guard.connection_id) else {
                    if index.is_replaced_connection(&guard.connection_id, guard.generation) {
                        return Err(SharedSessionError::GenerationStale);
                    }
                    return Err(SharedSessionError::SessionUnavailable);
                };
                let result = match record.try_lock() {
                    Ok(mut record) => {
                        if record.generation != guard.generation {
                            return Err(SharedSessionError::GenerationStale);
                        }
                        let expired = record.prune_expired_leases(tokio::time::Instant::now());
                        self.metrics.remove_active_leases(expired.len());
                        self.metrics.record_lease_expired(expired.len());
                        let Some(client) = record
                            .active_leases
                            .iter()
                            .find(|(_, lease)| lease.lease_id == guard.lease_id)
                            .map(|(client, _)| client.clone())
                        else {
                            return Ok(false);
                        };
                        record.active_leases.remove(&client);
                        self.metrics.remove_active_leases(1);
                        self.metrics.record_lease_released();
                        return Ok(true);
                    }
                    Err(_) => true,
                };
                result
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Expire leases across every authoritative record without touching the
    /// manager connection map. Expiry is a lease lifecycle event only and
    /// never disconnects the underlying ACP process.
    pub async fn expire_leases(&self, now: tokio::time::Instant) -> Vec<String> {
        let mut expired_ids = Vec::new();
        loop {
            let mut contended = false;
            {
                let index = self.index.lock().await;
                for record in index.sessions.values() {
                    match record.try_lock() {
                        Ok(mut record) => {
                            let expired = record.prune_expired_leases(now);
                            self.metrics.remove_active_leases(expired.len());
                            self.metrics.record_lease_expired(expired.len());
                            expired_ids.extend(expired);
                        }
                        Err(_) => contended = true,
                    }
                }
            }
            if !contended {
                break;
            }
            tokio::task::yield_now().await;
        }
        expired_ids
    }

    #[cfg(test)]
    fn with_limits_for_test(max_active_leases: usize, max_connect_ledger_entries: usize) -> Self {
        Self {
            limits: BrokerLimits {
                max_active_leases,
                max_connect_ledger_entries,
                ..BrokerLimits::default()
            },
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_prompt_limits_for_test(
        max_prompt_ledger_entries: usize,
        max_waiting_prompts: usize,
        max_waiting_bytes: usize,
    ) -> Self {
        Self {
            limits: BrokerLimits {
                max_prompt_ledger_entries,
                max_waiting_prompts,
                max_waiting_bytes,
                ..BrokerLimits::default()
            },
            ..Self::default()
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn with_prompt_ledger_limit_for_test(max_prompt_ledger_entries: usize) -> Self {
        Self {
            limits: BrokerLimits {
                max_prompt_ledger_entries,
                ..BrokerLimits::default()
            },
            ..Self::default()
        }
    }

    pub async fn reserve_or_attach(
        &self,
        request: SharedReserveRequest,
    ) -> Result<SharedReserveOutcome, SharedSessionError> {
        self.ensure_accepting()?;
        validate_client_label("device_id", &request.device_id)?;
        validate_client_label("client_instance_id", &request.client_instance_id)?;
        validate_client_label("request_id", &request.request_id)?;

        loop {
            self.ensure_accepting()?;
            let lookup = {
                let mut index = self.index.lock().await;
                self.ensure_accepting()?;
                if let Some(record) = index.record_for_key(&request.key) {
                    ReserveLookup::Existing(record.clone())
                } else {
                    let mut initial = SharedSessionRecord::reserved(&request, 1, None);
                    let attachment = match initial.attach_or_renew_lease(
                        &request,
                        self.lease_ttl(),
                        SharedDisposition::Created,
                        self.limits,
                    ) {
                        Ok((attachment, _)) => attachment,
                        Err(error) => {
                            if error.is_capacity_error() {
                                self.metrics.record_capacity_rejection();
                            }
                            return Err(error);
                        }
                    };
                    let record = Arc::new(Mutex::new(initial));
                    index
                        .by_connection
                        .insert(request.connection_id.clone(), request.key.clone());
                    index.sessions.insert(request.key.clone(), record);
                    self.index_epoch
                        .send_modify(|epoch| *epoch = epoch.saturating_add(1));
                    self.metrics.add_active_leases(1);
                    self.metrics.add_live_session();
                    ReserveLookup::Created(attachment)
                }
            };

            let record = match lookup {
                ReserveLookup::Created(attachment) => {
                    self.metrics.record_connect(true);
                    return Ok(SharedReserveOutcome {
                        attachment,
                        created: true,
                    });
                }
                ReserveLookup::Existing(record) => record,
            };

            #[cfg(test)]
            self.wait_idle_final_cas_barrier_for_test().await;

            let decision = {
                let mut current = record.lock().await;
                self.ensure_accepting()?;
                current.check_attach_identity(&request.launch_identity)?;
                match current.retry_decision(&request)? {
                    FailedRetryDecision::Attach => {
                        let expired = current.prune_expired_leases(request.now);
                        self.metrics.remove_active_leases(expired.len());
                        self.metrics.record_lease_expired(expired.len());
                        match current.attach_or_renew_lease(
                            &request,
                            self.lease_ttl(),
                            SharedDisposition::Attached,
                            self.limits,
                        ) {
                            Ok((attachment, added_lease)) => {
                                if added_lease {
                                    self.metrics.add_active_leases(1);
                                }
                                ReserveDecision::Attach(attachment)
                            }
                            Err(error) => {
                                if error.is_capacity_error() {
                                    self.metrics.record_capacity_rejection();
                                }
                                return Err(error);
                            }
                        }
                    }
                    FailedRetryDecision::Replace { failed_generation } => {
                        ReserveDecision::Replace { failed_generation }
                    }
                }
            };

            match decision {
                ReserveDecision::Attach(attachment) => {
                    self.metrics.record_connect(false);
                    return Ok(SharedReserveOutcome {
                        attachment,
                        created: false,
                    });
                }
                ReserveDecision::Replace { failed_generation } => {
                    if let Some(outcome) = self
                        .replace_failed_generation(&request, &record, failed_generation)
                        .await?
                    {
                        self.metrics.record_connect(true);
                        return Ok(outcome);
                    }
                }
            }
        }
    }

    pub async fn mark_failed(
        &self,
        connection_id: &str,
        generation: u64,
        error_code: impl Into<String>,
        cleanup_complete: bool,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        let error_code = error_code.into();
        validate_failure_code(&error_code)?;
        self.with_authoritative_record_and_state(connection_id, None, |record, state| {
            if record.connection_id != connection_id || record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            let is_bootstrap_failure = matches!(
                record.phase,
                SharedSessionPhase::Reserved | SharedSessionPhase::Bootstrapping
            );
            let now = tokio::time::Instant::now();
            let phase = SharedSessionPhase::Failed {
                error_code: error_code.clone(),
                cleanup_complete,
            };
            if let Some(state) = state {
                update_public_shared_phase(state, generation, phase.clone());
            }
            let mut events = fail_all_prompt_work(record, &error_code, &self.metrics);
            record.finish_bootstrap(now);
            if cleanup_complete {
                record.finish_cleanup(tokio::time::Instant::now());
            } else {
                record.begin_cleanup(tokio::time::Instant::now());
            }
            record.cleanup_complete = cleanup_complete;
            record.phase = phase;
            record.idle_zero_since = None;
            record.failed_zero_since = None;
            record.host_owned_work.clear();
            record.publish_registration();
            record
                .lifecycle_tx
                .send_replace(SharedLifecycleState::Failed);
            record.notify.notify_waiters();
            if is_bootstrap_failure {
                self.metrics.record_bootstrap_failed(
                    &bounded_agent_category(record.launch_identity.agent_type),
                    record.launch_identity.route_capability.metric_label(),
                    &error_code,
                    record.bootstrap_duration(now),
                );
            }
            events.push(crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                generation,
                phase: record.phase.clone(),
            });
            Ok(events)
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub async fn mark_cleanup_complete(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<
        (
            Option<(Arc<RwLock<SessionState>>, EventEmitter)>,
            Vec<crate::acp::types::AcpEvent>,
        ),
        SharedSessionError,
    > {
        self.with_authoritative_record_and_state(connection_id, None, |record, state| {
            if record.connection_id != connection_id || record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            let error_code = match &record.phase {
                SharedSessionPhase::Failed { error_code, .. } => error_code.clone(),
                _ => return Err(SharedSessionError::SessionUnavailable),
            };
            let publication_handles = match (record.state.as_ref(), record.emitter.as_ref()) {
                (Some(state), Some(emitter)) => Some((state.clone(), emitter.clone())),
                _ => None,
            };
            let phase = SharedSessionPhase::Failed {
                error_code,
                cleanup_complete: true,
            };
            if let Some(state) = state {
                update_public_shared_phase(state, generation, phase.clone());
            }
            record.finish_cleanup(tokio::time::Instant::now());
            record.cleanup_complete = true;
            record.phase = phase;
            record.failed_zero_since = None;
            record.publish_registration();
            Ok((
                publication_handles,
                vec![crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                    generation,
                    phase: record.phase.clone(),
                }],
            ))
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub async fn diagnostic_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<SharedSessionProjection> {
        self.with_authoritative_record(connection_id, |record| {
            if record.connection_id != connection_id {
                return Err(SharedSessionError::SessionUnavailable);
            }
            Ok(SharedSessionProjection {
                generation: record.generation,
                phase: record.phase.clone(),
                queue: record
                    .waiting_prompts
                    .iter()
                    .map(|queued| queued.summary.clone())
                    .collect(),
                active_turn: record
                    .active_turn
                    .as_ref()
                    .map(|active| active.projection.clone()),
                lease_expires_at: None,
                expired_lease_tombstone_count: record.expired_leases.len(),
            })
        })
        .await
        .ok()
        .flatten()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) async fn has_pending_interaction_for_test(
        &self,
        connection_id: &str,
        interaction_id: &str,
    ) -> bool {
        self.with_authoritative_record(connection_id, |record| {
            Ok(record.interactions.has_pending_id(interaction_id))
        })
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
    }

    pub(crate) async fn authoritative_snapshot(
        &self,
        connection_id: &str,
    ) -> Result<SharedAuthoritativeSnapshot, SharedSessionError> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(key) = index.by_connection.get(connection_id).cloned() else {
                    return Err(SharedSessionError::SessionUnavailable);
                };
                let Some(record) = index.sessions.get(&key).cloned() else {
                    return Err(SharedSessionError::SessionUnavailable);
                };
                let record = match record.try_lock() {
                    Ok(record) => record,
                    Err(_) => {
                        drop(index);
                        tokio::task::yield_now().await;
                        continue;
                    }
                };
                let state = record.state.clone();
                match state.as_ref() {
                    Some(state) => match state.try_read() {
                        Ok(state) => {
                            return Ok(SharedAuthoritativeSnapshot {
                                purpose: record.launch_identity.purpose,
                                canonical_conversation_id: match &key {
                                    SharedSessionKey::Conversation(id) => Some(*id),
                                    _ => None,
                                },
                                generation: record.generation,
                                phase: record.phase.clone(),
                                event_seq: state.event_seq,
                                folder_id: state.folder_id,
                                agent_type: record.launch_identity.agent_type,
                            })
                        }
                        Err(_) => true,
                    },
                    None => {
                        return Ok(SharedAuthoritativeSnapshot {
                            purpose: record.launch_identity.purpose,
                            canonical_conversation_id: match &key {
                                SharedSessionKey::Conversation(id) => Some(*id),
                                _ => None,
                            },
                            generation: record.generation,
                            phase: record.phase.clone(),
                            event_seq: 0,
                            folder_id: None,
                            agent_type: record.launch_identity.agent_type,
                        })
                    }
                }
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    pub(crate) async fn launch_identity_for_connection(
        &self,
        connection_id: &str,
    ) -> Result<SharedLaunchIdentity, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| Ok(record.launch_identity.clone()))
            .await?
            .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn wait_until_registered(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<SharedRegistrationState, SharedSessionError> {
        let mut receiver = self
            .with_authoritative_record(connection_id, |record| {
                if record.generation != generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                Ok(record.registration_tx.subscribe())
            })
            .await?
            .ok_or(SharedSessionError::SessionUnavailable)?;

        loop {
            let current = receiver.borrow().clone();
            if current.phase != SharedSessionPhase::Reserved && current.state.is_some() {
                return Ok(current);
            }
            receiver
                .changed()
                .await
                .map_err(|_| SharedSessionError::SessionUnavailable)?;
        }
    }

    pub async fn is_managed_connection(&self, connection_id: &str) -> bool {
        let index = self.index.lock().await;
        index.by_connection.contains_key(connection_id)
            || index.has_replaced_connection(connection_id)
    }

    pub(crate) async fn install_registered(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: String,
        state: Arc<RwLock<SessionState>>,
        emitter: EventEmitter,
        child_pid: Arc<std::sync::atomic::AtomicU32>,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            if record.phase != SharedSessionPhase::Reserved || record.driver_incarnation.is_some() {
                return Err(SharedSessionError::GenerationStale);
            }
            record.driver_incarnation = Some(driver_incarnation.clone());
            record.state = Some(state.clone());
            record.emitter = Some(emitter.clone());
            record.child_pid = Some(child_pid.clone());
            record.phase = SharedSessionPhase::Bootstrapping;
            record.publish_registration();
            Ok(vec![
                crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                    generation,
                    phase: record.phase.clone(),
                },
            ])
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn begin_registered_replacement(
        &self,
        connection_id: &str,
        generation: u64,
        previous_incarnation: &str,
    ) -> Result<RegisteredReplacementPermit, SharedSessionError> {
        let permit = RegisteredReplacementPermit {
            connection_id: connection_id.to_string(),
            generation,
            previous_incarnation: previous_incarnation.to_string(),
            token: uuid::Uuid::new_v4().to_string(),
        };
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation
                || record.phase != SharedSessionPhase::Bootstrapping
                || record.driver_incarnation.as_deref() != Some(previous_incarnation)
                || record.replacement_permit.is_some()
            {
                return Err(SharedSessionError::GenerationStale);
            }
            record.replacement_permit = Some(ActiveRegisteredReplacement {
                previous_incarnation: permit.previous_incarnation.clone(),
                token: permit.token.clone(),
            });
            Ok(permit.clone())
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn commit_registered_replacement(
        &self,
        permit: &RegisteredReplacementPermit,
        next_incarnation: String,
        state: Arc<RwLock<SessionState>>,
        emitter: EventEmitter,
        child_pid: Arc<std::sync::atomic::AtomicU32>,
    ) -> Result<(), SharedSessionError> {
        self.with_authoritative_record_and_state(
            &permit.connection_id,
            None,
            |record, public_state| {
                if record.generation != permit.generation
                    || record.phase != SharedSessionPhase::Bootstrapping
                    || record.driver_incarnation.as_deref()
                        != Some(permit.previous_incarnation.as_str())
                    || !record.replacement_permit.as_ref().is_some_and(|active| {
                        active.previous_incarnation == permit.previous_incarnation
                            && active.token == permit.token
                    })
                {
                    return Err(SharedSessionError::GenerationStale);
                }
                let registered_state = record
                    .state
                    .as_ref()
                    .ok_or(SharedSessionError::SessionUnavailable)?;
                let public_state = public_state.ok_or(SharedSessionError::SessionUnavailable)?;
                if !Arc::ptr_eq(registered_state, &state)
                    || public_state.connection_incarnation != next_incarnation
                {
                    return Err(SharedSessionError::GenerationStale);
                }
                record.driver_incarnation = Some(next_incarnation.clone());
                record.emitter = Some(emitter.clone());
                record.child_pid = Some(child_pid.clone());
                record.replacement_permit = None;
                record.publish_registration();
                Ok(())
            },
        )
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn mark_ready(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record_and_state(connection_id, None, |record, state| {
            if record.generation != generation
                || record.phase != SharedSessionPhase::Bootstrapping
                || record.driver_incarnation.as_deref() != Some(driver_incarnation)
                || record.replacement_permit.is_some()
            {
                return Err(SharedSessionError::GenerationStale);
            }
            let state = state.ok_or(SharedSessionError::SessionUnavailable)?;
            if matches!(
                state.status,
                crate::acp::types::ConnectionStatus::Disconnected
                    | crate::acp::types::ConnectionStatus::Error
            ) {
                return Err(SharedSessionError::SessionUnavailable);
            }
            update_public_shared_phase(state, generation, SharedSessionPhase::Ready);
            let now = tokio::time::Instant::now();
            record.finish_bootstrap(now);
            record.phase = SharedSessionPhase::Ready;
            record.idle_zero_since = None;
            record.failed_zero_since = None;
            record.publish_registration();
            self.metrics
                .record_bootstrap_ready(record.bootstrap_duration(now));
            Ok(vec![
                crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                    generation,
                    phase: record.phase.clone(),
                },
            ])
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn is_current_bootstrapping_driver(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
    ) -> bool {
        self.with_authoritative_record(connection_id, |record| {
            Ok(record.generation == generation
                && record.phase == SharedSessionPhase::Bootstrapping
                && record.driver_incarnation.as_deref() == Some(driver_incarnation)
                && record.replacement_permit.is_none())
        })
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
    }

    pub(crate) async fn driver_incarnation_for_generation(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<Option<String>, SharedSessionError> {
        self.with_authoritative_record(connection_id, |record| {
            if record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            Ok(record.driver_incarnation.clone())
        })
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn driver_child_pid_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<Arc<std::sync::atomic::AtomicU32>> {
        self.with_authoritative_record(connection_id, |record| Ok(record.child_pid.clone()))
            .await
            .ok()
            .flatten()
            .flatten()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fail_registered(
        &self,
        connection_id: &str,
        generation: u64,
        expected_driver_incarnation: Option<&str>,
        error: SharedSessionError,
        cleanup_complete: bool,
        state: Arc<RwLock<SessionState>>,
        emitter: EventEmitter,
    ) -> Result<
        (
            Arc<RwLock<SessionState>>,
            EventEmitter,
            Vec<crate::acp::types::AcpEvent>,
        ),
        SharedSessionError,
    > {
        let error_code = error.code().to_string();
        let fallback_state = state.clone();
        self.with_authoritative_record_and_state(
            connection_id,
            Some(&fallback_state),
            |record, public_state| {
                if record.generation != generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                match expected_driver_incarnation {
                    Some(expected)
                        if record.driver_incarnation.as_deref() != Some(expected)
                            || record.phase != SharedSessionPhase::Bootstrapping =>
                    {
                        return Err(SharedSessionError::GenerationStale);
                    }
                    None if record.driver_incarnation.is_some()
                        || record.phase != SharedSessionPhase::Reserved =>
                    {
                        return Err(SharedSessionError::GenerationStale);
                    }
                    _ => {}
                }
                let state = record
                    .state
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| state.clone());
                let emitter = record
                    .emitter
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| emitter.clone());
                let phase = SharedSessionPhase::Failed {
                    error_code: error_code.clone(),
                    cleanup_complete,
                };
                update_public_shared_phase(
                    public_state.ok_or(SharedSessionError::SessionUnavailable)?,
                    generation,
                    phase.clone(),
                );
                record.state = Some(state.clone());
                record.emitter = Some(emitter.clone());
                let now = tokio::time::Instant::now();
                record.finish_bootstrap(now);
                if cleanup_complete {
                    record.finish_cleanup(tokio::time::Instant::now());
                } else {
                    record.begin_cleanup(tokio::time::Instant::now());
                }
                record.cleanup_complete = cleanup_complete;
                record.phase = phase;
                record.idle_zero_since = None;
                record.failed_zero_since = None;
                record.host_owned_work.clear();
                record.replacement_permit = None;
                record.publish_registration();
                record
                    .lifecycle_tx
                    .send_replace(SharedLifecycleState::Failed);
                record.notify.notify_waiters();
                self.metrics.record_bootstrap_failed(
                    &bounded_agent_category(record.launch_identity.agent_type),
                    record.launch_identity.route_capability.metric_label(),
                    &error_code,
                    record.bootstrap_duration(now),
                );
                let mut events = fail_all_prompt_work(record, &error_code, &self.metrics);
                events.push(crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                    generation,
                    phase: record.phase.clone(),
                });
                Ok((state, emitter, events))
            },
        )
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fail_registered_replacement(
        &self,
        permit: &RegisteredReplacementPermit,
        error: SharedSessionError,
        cleanup_complete: bool,
        state: Arc<RwLock<SessionState>>,
        emitter: EventEmitter,
    ) -> Result<
        (
            Arc<RwLock<SessionState>>,
            EventEmitter,
            Vec<crate::acp::types::AcpEvent>,
        ),
        SharedSessionError,
    > {
        let error_code = error.code().to_string();
        let fallback_state = state.clone();
        self.with_authoritative_record_and_state(
            &permit.connection_id,
            Some(&fallback_state),
            |record, public_state| {
                if record.generation != permit.generation
                    || record.phase != SharedSessionPhase::Bootstrapping
                    || record.driver_incarnation.as_deref()
                        != Some(permit.previous_incarnation.as_str())
                    || !record.replacement_permit.as_ref().is_some_and(|active| {
                        active.previous_incarnation == permit.previous_incarnation
                            && active.token == permit.token
                    })
                {
                    return Err(SharedSessionError::GenerationStale);
                }
                let state = record
                    .state
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| state.clone());
                let emitter = record
                    .emitter
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| emitter.clone());
                let phase = SharedSessionPhase::Failed {
                    error_code: error_code.clone(),
                    cleanup_complete,
                };
                update_public_shared_phase(
                    public_state.ok_or(SharedSessionError::SessionUnavailable)?,
                    permit.generation,
                    phase.clone(),
                );
                record.state = Some(state.clone());
                record.emitter = Some(emitter.clone());
                let now = tokio::time::Instant::now();
                record.finish_bootstrap(now);
                if cleanup_complete {
                    record.finish_cleanup(tokio::time::Instant::now());
                } else {
                    record.begin_cleanup(tokio::time::Instant::now());
                }
                record.cleanup_complete = cleanup_complete;
                record.phase = phase;
                record.idle_zero_since = None;
                record.failed_zero_since = None;
                record.host_owned_work.clear();
                record.replacement_permit = None;
                record.publish_registration();
                record
                    .lifecycle_tx
                    .send_replace(SharedLifecycleState::Failed);
                record.notify.notify_waiters();
                self.metrics.record_bootstrap_failed(
                    &bounded_agent_category(record.launch_identity.agent_type),
                    record.launch_identity.route_capability.metric_label(),
                    &error_code,
                    record.bootstrap_duration(now),
                );
                let mut events = fail_all_prompt_work(record, &error_code, &self.metrics);
                events.push(crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                    generation: permit.generation,
                    phase: record.phase.clone(),
                });
                Ok((state, emitter, events))
            },
        )
        .await?
        .ok_or(SharedSessionError::SessionUnavailable)
    }

    pub(crate) async fn public_state_and_emitter(
        &self,
        connection_id: &str,
    ) -> Option<(Arc<RwLock<SessionState>>, EventEmitter)> {
        self.with_authoritative_record(connection_id, |record| {
            match (record.state.clone(), record.emitter.clone()) {
                (Some(state), Some(emitter)) => Ok(Some((state, emitter))),
                _ => Ok(None),
            }
        })
        .await
        .ok()
        .flatten()
        .flatten()
    }

    pub(crate) async fn wait_for_phase(
        &self,
        connection_id: &str,
        generation: u64,
        expected: SharedSessionPhase,
    ) -> Result<(), SharedSessionError> {
        let mut receiver = self
            .with_authoritative_record(connection_id, |record| {
                if record.generation != generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                Ok(record.registration_tx.subscribe())
            })
            .await?
            .ok_or(SharedSessionError::SessionUnavailable)?;
        loop {
            let current = receiver.borrow().phase.clone();
            if current == expected {
                return Ok(());
            }
            match (&current, &expected) {
                (
                    SharedSessionPhase::Failed {
                        error_code: current_code,
                        cleanup_complete: false,
                    },
                    SharedSessionPhase::Failed {
                        error_code: expected_code,
                        cleanup_complete: true,
                    },
                ) if current_code == expected_code => {}
                (SharedSessionPhase::Failed { .. }, _) => {
                    return Err(SharedSessionError::SessionUnavailable);
                }
                (SharedSessionPhase::Closing, _) => {
                    return Err(SharedSessionError::Closing);
                }
                _ => {}
            }
            receiver
                .changed()
                .await
                .map_err(|_| SharedSessionError::SessionUnavailable)?;
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn wait_for_key_phase_for_test(
        &self,
        key: &SharedSessionKey,
        expected: SharedSessionPhase,
    ) -> Result<(), SharedSessionError> {
        let mut epoch = self.index_epoch.subscribe();
        loop {
            let record = {
                let index = self.index.lock().await;
                index.record_for_key(key).cloned()
            };
            if let Some(record) = record {
                let mut registration = record.lock().await.registration_tx.subscribe();
                loop {
                    if registration.borrow().phase == expected {
                        return Ok(());
                    }
                    registration
                        .changed()
                        .await
                        .map_err(|_| SharedSessionError::SessionUnavailable)?;
                }
            }
            epoch
                .changed()
                .await
                .map_err(|_| SharedSessionError::SessionUnavailable)?;
        }
    }

    async fn with_authoritative_record<T>(
        &self,
        connection_id: &str,
        mut operation: impl FnMut(&mut SharedSessionRecord) -> Result<T, SharedSessionError>,
    ) -> Result<Option<T>, SharedSessionError> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(connection_id) else {
                    return Ok(None);
                };
                let contended = match record.try_lock() {
                    Ok(mut record) => return operation(&mut record).map(Some),
                    Err(_) => true,
                };
                contended
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    async fn require_authoritative_result<T>(
        &self,
        connection_id: &str,
        _generation: u64,
        result: Option<T>,
    ) -> Result<T, SharedSessionError> {
        if let Some(result) = result {
            return Ok(result);
        }
        if self
            .index
            .lock()
            .await
            .has_replaced_connection(connection_id)
        {
            Err(SharedSessionError::GenerationStale)
        } else {
            Err(SharedSessionError::SessionUnavailable)
        }
    }

    fn validate_prompt_guard(
        &self,
        record: &mut SharedSessionRecord,
        guard: &SharedMutationGuard,
    ) -> Result<(), SharedSessionError> {
        if record.connection_id != guard.connection_id || record.generation != guard.generation {
            return Err(SharedSessionError::GenerationStale);
        }
        let expired = record.prune_expired_leases(tokio::time::Instant::now());
        self.metrics.remove_active_leases(expired.len());
        self.metrics.record_lease_expired(expired.len());
        if record
            .active_leases
            .values()
            .any(|lease| lease.lease_id == guard.lease_id)
        {
            return Ok(());
        }
        if record
            .expired_leases
            .iter()
            .any(|expired_id| expired_id == &guard.lease_id)
        {
            Err(SharedSessionError::LeaseExpired)
        } else {
            Err(SharedSessionError::LeaseMissing)
        }
    }

    #[cfg(test)]
    pub(crate) async fn prompt_state_for_test(
        &self,
        connection_id: &str,
        queue_item_id: &str,
    ) -> Option<InternalPromptState> {
        self.with_authoritative_record(connection_id, |record| {
            Ok(record
                .prompt_ledger
                .values()
                .find(|entry| entry.queue_item_id == queue_item_id)
                .map(|entry| entry.state))
        })
        .await
        .ok()
        .flatten()
        .flatten()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn key_for_connection_for_test(
        &self,
        connection_id: &str,
    ) -> Option<SharedSessionKey> {
        self.index
            .lock()
            .await
            .by_connection
            .get(connection_id)
            .cloned()
    }

    async fn with_authoritative_record_and_state<T>(
        &self,
        connection_id: &str,
        fallback_state: Option<&Arc<RwLock<SessionState>>>,
        mut operation: impl FnMut(
            &mut SharedSessionRecord,
            Option<&mut SessionState>,
        ) -> Result<T, SharedSessionError>,
    ) -> Result<Option<T>, SharedSessionError> {
        loop {
            // State contention must release both broker locks before yielding;
            // every retry revalidates the authoritative record and fences.
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(connection_id) else {
                    return Ok(None);
                };
                let contended = match record.try_lock() {
                    Ok(mut record) => {
                        let state = record.state.as_ref().or(fallback_state).cloned();
                        match state {
                            Some(state) => match state.try_write() {
                                Ok(mut state) => {
                                    return operation(&mut record, Some(&mut state)).map(Some)
                                }
                                Err(_) => true,
                            },
                            None => return operation(&mut record, None).map(Some),
                        }
                    }
                    Err(_) => true,
                };
                contended
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    async fn replace_failed_generation(
        &self,
        request: &SharedReserveRequest,
        expected_record: &Arc<Mutex<SharedSessionRecord>>,
        failed_generation: u64,
    ) -> Result<Option<SharedReserveOutcome>, SharedSessionError> {
        let mut index = self.index.lock().await;
        let canonical_key = index.canonical_key(&request.key);
        let is_authoritative = index
            .sessions
            .get(&canonical_key)
            .is_some_and(|current| Arc::ptr_eq(current, expected_record));
        if !is_authoritative {
            return Ok(None);
        }

        let current = match expected_record.try_lock() {
            Ok(current) => current,
            Err(_) => {
                drop(index);
                tokio::task::yield_now().await;
                return Ok(None);
            }
        };
        if current.generation != failed_generation {
            return Err(SharedSessionError::GenerationStale);
        }
        if !matches!(current.phase, SharedSessionPhase::Failed { .. }) {
            return Err(SharedSessionError::GenerationStale);
        }
        if !current.cleanup_complete {
            return Err(SharedSessionError::CleanupInProgress);
        }

        let old_connection_id = current.connection_id.clone();
        let old_active_leases = current.active_leases.len();
        let old_waiting_prompts = current.waiting_prompts.len();
        let old_waiting_bytes = current.waiting_bytes;
        let next_generation = failed_generation
            .checked_add(1)
            .ok_or(SharedSessionError::GenerationStale)?;
        let mut replacement =
            SharedSessionRecord::reserved(request, next_generation, Some(failed_generation));
        let (attachment, added_lease) = match replacement.attach_or_renew_lease(
            request,
            self.lease_ttl(),
            SharedDisposition::Created,
            self.limits,
        ) {
            Ok(result) => result,
            Err(error) => {
                if error.is_capacity_error() {
                    self.metrics.record_capacity_rejection();
                }
                return Err(error);
            }
        };
        debug_assert!(added_lease);
        let replacement = Arc::new(Mutex::new(replacement));

        current
            .lifecycle_tx
            .send_replace(SharedLifecycleState::Replaced);
        current.notify.notify_waiters();
        index.record_replaced_connection(old_connection_id.clone(), failed_generation);
        index.by_connection.remove(&old_connection_id);
        index
            .by_connection
            .insert(request.connection_id.clone(), canonical_key.clone());
        index.sessions.insert(canonical_key, replacement);
        self.index_epoch
            .send_modify(|epoch| *epoch = epoch.saturating_add(1));
        self.metrics.remove_active_leases(old_active_leases);
        self.metrics
            .remove_waiting(old_waiting_prompts, old_waiting_bytes);
        self.metrics.add_active_leases(1);

        Ok(Some(SharedReserveOutcome {
            attachment,
            created: true,
        }))
    }

    fn is_replaceable_conversation_destination(record: &SharedSessionRecord) -> bool {
        matches!(
            record.phase,
            SharedSessionPhase::Failed {
                cleanup_complete: true,
                ..
            }
        ) && record.cleanup_complete
            && record.active_leases.is_empty()
    }

    fn account_replaced_conversation_destination(
        &self,
        index: &mut SharedSessionIndex,
        destination: &SharedSessionRecord,
    ) {
        debug_assert!(Self::is_replaceable_conversation_destination(destination));
        destination
            .lifecycle_tx
            .send_replace(SharedLifecycleState::Replaced);
        destination.notify.notify_waiters();
        index.by_connection.remove(&destination.connection_id);
        index.record_replaced_connection(destination.connection_id.clone(), destination.generation);
        self.metrics
            .remove_waiting(destination.waiting_prompts.len(), destination.waiting_bytes);
        self.metrics.remove_live_session();
    }
}

impl Drop for SharedHostWorkPermit {
    fn drop(&mut self) {
        let Some((connection_id, generation, permit_id)) = self.identity.take() else {
            return;
        };
        let Some(index) = self.broker_index.upgrade() else {
            tracing::debug!(
                "[ACP] shared host-work drop after broker shutdown connection={} generation={}",
                connection_id,
                generation
            );
            return;
        };
        let shutdown_fallback = HostWorkShutdownFallback {
            connection_id: connection_id.clone(),
            generation,
            completed: false,
        };
        self.runtime.spawn(async move {
            release_host_work(index, (connection_id, generation, permit_id)).await;
            shutdown_fallback.complete();
        });
    }
}

struct HostWorkShutdownFallback {
    connection_id: String,
    generation: u64,
    completed: bool,
}

impl HostWorkShutdownFallback {
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for HostWorkShutdownFallback {
    fn drop(&mut self) {
        if !self.completed {
            tracing::debug!(
                "[ACP] shared host-work release dropped during runtime shutdown connection={} generation={}",
                self.connection_id,
                self.generation
            );
        }
    }
}

async fn release_host_work(
    index: Arc<Mutex<SharedSessionIndex>>,
    (connection_id, generation, permit_id): (String, u64, uuid::Uuid),
) -> bool {
    loop {
        let index_guard = index.lock().await;
        let Some(record) = index_guard.record_for_connection(&connection_id).cloned() else {
            return false;
        };
        let result = match record.try_lock() {
            Ok(mut record) => {
                if record.generation != generation {
                    return false;
                }
                let removed = record.host_owned_work.remove(&permit_id);
                let notify = removed.then(|| record.notify.clone());
                drop(record);
                drop(index_guard);
                if let Some(notify) = notify {
                    notify.notify_one();
                }
                return removed;
            }
            Err(_) => true,
        };
        drop(index_guard);
        if result {
            tokio::task::yield_now().await;
        }
    }
}

fn update_public_shared_phase(
    state: &mut SessionState,
    generation: u64,
    phase: SharedSessionPhase,
) {
    state.status = phase.connection_status();
    match state.shared_session.as_mut() {
        Some(projection) => {
            projection.generation = generation;
            projection.phase = phase;
        }
        None => {
            state.shared_session = Some(SharedSessionProjection {
                generation,
                phase,
                queue: Vec::new(),
                active_turn: None,
                lease_expires_at: None,
                expired_lease_tombstone_count: 0,
            });
        }
    }
}

#[derive(serde::Serialize)]
struct CanonicalPromptPayload<'a> {
    blocks: &'a [crate::acp::types::PromptInputBlock],
    folder_id: Option<i32>,
    conversation_id: Option<i32>,
    client_message_id: &'a str,
    capture: Option<CanonicalPromptCapture<'a>>,
}

#[derive(serde::Serialize)]
struct CanonicalPromptCapture<'a> {
    visible_text: &'a Option<String>,
    locale: &'a Option<crate::models::system::AppLocale>,
}

fn canonical_prompt_bytes(request: &SharedPromptRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&CanonicalPromptPayload {
        blocks: &request.blocks,
        folder_id: request.folder_id,
        conversation_id: request.conversation_id,
        client_message_id: &request.client_message_id,
        capture: request
            .capture
            .as_ref()
            .map(|capture| CanonicalPromptCapture {
                visible_text: &capture.visible_text,
                locale: &capture.locale,
            }),
    })
}

fn validate_prompt_request(request: &SharedPromptRequest) -> Result<(), SharedSessionError> {
    validate_client_label("client_instance_id", &request.client_instance_id)?;
    validate_client_label("client_request_id", &request.client_request_id)?;
    if request.blocks.is_empty() {
        return Err(SharedSessionError::InvalidField { field: "blocks" });
    }
    if request.conversation_id.is_some() && request.folder_id.is_none() {
        return Err(SharedSessionError::InvalidField { field: "folder_id" });
    }
    if request.client_message_id.is_empty() {
        return Err(SharedSessionError::InvalidField {
            field: "client_message_id",
        });
    }
    Ok(())
}

fn bounded_agent_category(agent_type: crate::models::AgentType) -> String {
    if agent_type.is_custom() {
        "custom".to_string()
    } else {
        agent_type.as_wire().into_owned()
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn queue_depth_event(record: &SharedSessionRecord) -> crate::acp::types::AcpEvent {
    crate::acp::types::AcpEvent::PromptQueueDepthChanged {
        generation: record.generation,
        waiting_count: u32::try_from(record.waiting_prompts.len()).unwrap_or(u32::MAX),
        waiting_bytes: u64::try_from(record.waiting_bytes).unwrap_or(u64::MAX),
    }
}

fn fail_live_session_record(
    record: &mut SharedSessionRecord,
    error_code: &str,
    metrics: &SharedSessionMetrics,
) -> Vec<crate::acp::types::AcpEvent> {
    let generation = record.generation;
    let mut events = Vec::new();
    let active_failed = usize::from(record.active_turn.is_some());
    let waiting_failed = record.waiting_prompts.len();
    let waiting_bytes = record.waiting_bytes;
    if let Some(mut active) = record.active_turn.take() {
        active.resolve_stop_waiters_as_requested();
        record
            .prompt_ledger
            .get_mut(&active.identity)
            .expect("active turn has ledger entry")
            .invalidate_admission(InternalPromptState::Failed);
        events.push(crate::acp::types::AcpEvent::SharedTurnSettled {
            generation,
            turn_id: active.projection.turn_id,
            outcome: SharedTurnOutcome::Failed,
        });
    }
    record.interactions.clear();
    while let Some(queued) = record.waiting_prompts.pop_front() {
        record
            .prompt_ledger
            .get_mut(&queued.identity)
            .expect("queued item has ledger entry")
            .invalidate_admission(InternalPromptState::Failed);
        events.push(crate::acp::types::AcpEvent::PromptQueueItemFailed {
            generation,
            queue_item_id: queued.summary.queue_item_id,
            error_code: error_code.to_string(),
        });
    }
    record.waiting_bytes = 0;
    metrics.remove_waiting(waiting_failed, waiting_bytes);
    metrics.record_queue_items_failed(active_failed.saturating_add(waiting_failed));
    events.push(queue_depth_event(record));
    record.finish_bootstrap(tokio::time::Instant::now());
    record.begin_cleanup(tokio::time::Instant::now());
    record.phase = SharedSessionPhase::Failed {
        error_code: error_code.to_string(),
        cleanup_complete: false,
    };
    record.cleanup_complete = false;
    record.idle_zero_since = None;
    record.failed_zero_since = None;
    record.host_owned_work.clear();
    record.publish_registration();
    record
        .lifecycle_tx
        .send_replace(SharedLifecycleState::Failed);
    record.notify.notify_waiters();
    events.push(crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
        generation,
        phase: record.phase.clone(),
    });
    events
}

fn fail_all_prompt_work(
    record: &mut SharedSessionRecord,
    error_code: &str,
    metrics: &SharedSessionMetrics,
) -> Vec<crate::acp::types::AcpEvent> {
    let mut events = Vec::new();
    let active_failed = usize::from(record.active_turn.is_some());
    let waiting_failed = record.waiting_prompts.len();
    let waiting_bytes = record.waiting_bytes;
    if let Some(mut active) = record.active_turn.take() {
        active.resolve_stop_waiters_as_requested();
        record
            .prompt_ledger
            .get_mut(&active.identity)
            .expect("active turn has ledger entry")
            .invalidate_admission(InternalPromptState::Failed);
        events.push(crate::acp::types::AcpEvent::SharedTurnSettled {
            generation: record.generation,
            turn_id: active.projection.turn_id,
            outcome: SharedTurnOutcome::Failed,
        });
    }
    record.interactions.clear();
    let had_waiting = !record.waiting_prompts.is_empty();
    while let Some(queued) = record.waiting_prompts.pop_front() {
        record
            .prompt_ledger
            .get_mut(&queued.identity)
            .expect("queued item has ledger entry")
            .invalidate_admission(InternalPromptState::Failed);
        events.push(crate::acp::types::AcpEvent::PromptQueueItemFailed {
            generation: record.generation,
            queue_item_id: queued.summary.queue_item_id,
            error_code: error_code.to_string(),
        });
    }
    if had_waiting {
        record.waiting_bytes = 0;
        events.push(queue_depth_event(record));
    }
    metrics.remove_waiting(waiting_failed, waiting_bytes);
    metrics.record_queue_items_failed(active_failed.saturating_add(waiting_failed));
    events
}

#[derive(Clone, Copy)]
struct BrokerLimits {
    max_active_leases: usize,
    max_connect_ledger_entries: usize,
    max_prompt_ledger_entries: usize,
    max_waiting_prompts: usize,
    max_waiting_bytes: usize,
}

impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_active_leases: MAX_ACTIVE_LEASES,
            max_connect_ledger_entries: MAX_CONNECT_LEDGER_ENTRIES,
            max_prompt_ledger_entries: MAX_PROMPT_LEDGER_ENTRIES,
            max_waiting_prompts: MAX_WAITING_PROMPTS,
            max_waiting_bytes: MAX_WAITING_BYTES,
        }
    }
}

#[derive(Default)]
struct SharedSessionIndex {
    sessions: HashMap<SharedSessionKey, Arc<Mutex<SharedSessionRecord>>>,
    aliases: HashMap<SharedSessionKey, SharedSessionKey>,
    by_connection: HashMap<String, SharedSessionKey>,
    replaced_connections: VecDeque<ReplacedConnectionTombstone>,
}

impl SharedSessionIndex {
    fn insert_alias(&mut self, alias: SharedSessionKey, canonical: SharedSessionKey) {
        debug_assert!(alias != canonical);
        debug_assert!(!self.aliases.contains_key(&canonical));
        debug_assert!(!self.sessions.contains_key(&alias));
        debug_assert!(self.sessions.contains_key(&canonical));
        self.aliases.insert(alias, canonical);
    }

    fn canonical_key(&self, key: &SharedSessionKey) -> SharedSessionKey {
        match self.aliases.get(key) {
            Some(canonical) => {
                debug_assert!(self.sessions.contains_key(canonical));
                canonical.clone()
            }
            None => key.clone(),
        }
    }

    fn record_for_key(&self, key: &SharedSessionKey) -> Option<&Arc<Mutex<SharedSessionRecord>>> {
        let canonical = self.aliases.get(key).unwrap_or(key);
        debug_assert!(self.aliases.get(key).is_none() || self.sessions.contains_key(canonical));
        self.sessions.get(canonical)
    }

    fn record_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<&Arc<Mutex<SharedSessionRecord>>> {
        let key = self.by_connection.get(connection_id)?;
        self.sessions.get(key)
    }

    fn remove_aliases_for_canonical(&mut self, key: &SharedSessionKey) {
        self.aliases
            .retain(|alias, canonical| alias != key && canonical != key);
    }

    fn remove_canonical_session(
        &mut self,
        key: &SharedSessionKey,
    ) -> Option<Arc<Mutex<SharedSessionRecord>>> {
        self.remove_aliases_for_canonical(key);
        self.sessions.remove(key)
    }

    fn is_replaced_connection(&self, connection_id: &str, generation: u64) -> bool {
        self.replaced_connections.iter().any(|tombstone| {
            tombstone.connection_id == connection_id && tombstone.generation == generation
        })
    }

    fn has_replaced_connection(&self, connection_id: &str) -> bool {
        self.replaced_connections
            .iter()
            .any(|tombstone| tombstone.connection_id == connection_id)
    }

    fn record_replaced_connection(&mut self, connection_id: String, generation: u64) {
        self.replaced_connections
            .push_back(ReplacedConnectionTombstone {
                connection_id,
                generation,
            });
        if self.replaced_connections.len() > MAX_REPLACED_CONNECTION_TOMBSTONES {
            self.replaced_connections.pop_front();
        }
    }
}

struct ReplacedConnectionTombstone {
    connection_id: String,
    generation: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PromptIdentity {
    generation: u64,
    client_instance_id: String,
    client_request_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InternalPromptState {
    Queued,
    Dispatching,
    Completed,
    Failed,
    Cancelled,
}

struct PromptLedgerEntry {
    payload_hash: [u8; 32],
    queue_item_id: String,
    enqueue_seq: u64,
    state: InternalPromptState,
    frozen_result: Option<PromptEnqueueResult>,
    admission_events: Vec<crate::acp::types::AcpEvent>,
    admission_publication: Arc<tokio::sync::OnceCell<()>>,
    admission_invalidated: Arc<AtomicBool>,
    admission_published: bool,
}

impl PromptLedgerEntry {
    fn invalidate_admission(&mut self, state: InternalPromptState) {
        self.admission_invalidated.store(true, Ordering::Release);
        self.state = state;
    }
}

struct QueuedPromptRecord {
    identity: PromptIdentity,
    summary: SharedQueuedPromptSummary,
    blocks: Vec<crate::acp::types::PromptInputBlock>,
    folder_id: Option<i32>,
    conversation_id: Option<i32>,
    client_message_id: String,
    capture: Option<crate::auto_title::PromptCaptureContext>,
    waiting_bytes: usize,
}

struct BrokerActiveTurn {
    identity: PromptIdentity,
    projection: SharedActiveTurnProjection,
    stop_admission: StopAdmissionState,
}

enum StopAdmissionState {
    Open,
    Resolving {
        result_tx: watch::Sender<Option<StopAdmissionResolution>>,
    },
    Requested,
}

impl BrokerActiveTurn {
    fn complete_stop_request(&mut self) {
        let previous = std::mem::replace(&mut self.stop_admission, StopAdmissionState::Requested);
        match previous {
            StopAdmissionState::Resolving { result_tx } => {
                result_tx.send_replace(Some(StopAdmissionResolution::Requested));
            }
            StopAdmissionState::Open | StopAdmissionState::Requested => {}
        }
        self.projection.stop_requested = true;
    }

    fn release_stop_request(&mut self) {
        let previous = std::mem::replace(&mut self.stop_admission, StopAdmissionState::Open);
        match previous {
            StopAdmissionState::Resolving { result_tx } => {
                self.projection.stop_requested = false;
                result_tx.send_replace(Some(StopAdmissionResolution::DefinitelyNotAdmitted));
            }
            StopAdmissionState::Open => {
                self.projection.stop_requested = false;
            }
            StopAdmissionState::Requested => {
                self.stop_admission = StopAdmissionState::Requested;
                self.projection.stop_requested = true;
            }
        }
    }

    fn resolve_stop_waiters_as_requested(&mut self) {
        if matches!(self.stop_admission, StopAdmissionState::Resolving { .. }) {
            self.complete_stop_request();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InteractionAdmissionState {
    Pending,
    Resolving,
    Resolved,
}

struct SharedInteraction {
    id: String,
    admission: InteractionAdmissionState,
}

#[derive(Default)]
struct SharedInteractions {
    permission: Option<SharedInteraction>,
    question: Option<SharedInteraction>,
    plan_approval: Option<SharedInteraction>,
    last_observed_event_seq: u64,
}

impl SharedInteractions {
    fn get_mut(&mut self, kind: SharedInteractionKind) -> &mut Option<SharedInteraction> {
        match kind {
            SharedInteractionKind::Permission => &mut self.permission,
            SharedInteractionKind::Question => &mut self.question,
            SharedInteractionKind::PlanApproval => &mut self.plan_approval,
        }
    }

    fn has_any(&self) -> bool {
        self.permission.is_some() || self.question.is_some() || self.plan_approval.is_some()
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn has_pending_id(&self, interaction_id: &str) -> bool {
        [
            self.permission.as_ref(),
            self.question.as_ref(),
            self.plan_approval.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|interaction| {
            interaction.id == interaction_id
                && interaction.admission == InteractionAdmissionState::Pending
        })
    }

    fn set_pending(&mut self, kind: SharedInteractionKind, interaction_id: &str, event_seq: u64) {
        if event_seq < self.last_observed_event_seq {
            return;
        }
        self.last_observed_event_seq = self.last_observed_event_seq.max(event_seq);
        let slot = self.get_mut(kind);
        if slot
            .as_ref()
            .is_some_and(|interaction| interaction.id == interaction_id)
        {
            return;
        }
        *slot = Some(SharedInteraction {
            id: interaction_id.to_string(),
            admission: InteractionAdmissionState::Pending,
        });
    }

    fn resolve_matching(
        &mut self,
        kind: SharedInteractionKind,
        interaction_id: &str,
        event_seq: u64,
    ) {
        if event_seq < self.last_observed_event_seq {
            return;
        }
        self.last_observed_event_seq = self.last_observed_event_seq.max(event_seq);
        if let Some(interaction) = self.get_mut(kind).as_mut() {
            if interaction.id == interaction_id {
                interaction.admission = InteractionAdmissionState::Resolved;
            }
        }
    }

    fn reconcile(&mut self, kind: SharedInteractionKind, interaction_id: Option<&str>) {
        let slot = self.get_mut(kind);
        match interaction_id {
            Some(interaction_id)
                if slot
                    .as_ref()
                    .is_some_and(|interaction| interaction.id == interaction_id) => {}
            Some(interaction_id) => {
                *slot = Some(SharedInteraction {
                    id: interaction_id.to_string(),
                    admission: InteractionAdmissionState::Pending,
                });
            }
            None if slot.as_ref().is_some_and(|interaction| {
                interaction.admission == InteractionAdmissionState::Resolving
            }) => {}
            None => *slot = None,
        }
    }

    fn reconcile_snapshot(&mut self, snapshot: &SharedRuntimeWorkSnapshot) {
        if snapshot.event_seq < self.last_observed_event_seq {
            return;
        }
        self.reconcile(
            SharedInteractionKind::Permission,
            snapshot.pending_permission_id.as_deref(),
        );
        self.reconcile(
            SharedInteractionKind::Question,
            snapshot.pending_question_id.as_deref(),
        );
        self.reconcile(
            SharedInteractionKind::PlanApproval,
            snapshot.pending_plan_approval_id.as_deref(),
        );
        self.last_observed_event_seq = snapshot.event_seq;
    }

    fn clear(&mut self) {
        self.permission = None;
        self.question = None;
        self.plan_approval = None;
    }

    fn clear_at(&mut self, event_seq: u64) {
        if event_seq < self.last_observed_event_seq {
            return;
        }
        self.clear();
        self.last_observed_event_seq = event_seq;
    }
}

struct SharedSessionRecord {
    generation: u64,
    connection_id: String,
    launch_identity: SharedLaunchIdentity,
    phase: SharedSessionPhase,
    cleanup_complete: bool,
    state: Option<Arc<RwLock<SessionState>>>,
    emitter: Option<EventEmitter>,
    driver_incarnation: Option<String>,
    child_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
    replacement_permit: Option<ActiveRegisteredReplacement>,
    registration_tx: watch::Sender<SharedRegistrationState>,
    lifecycle_tx: watch::Sender<SharedLifecycleState>,
    active_leases: HashMap<ClientIdentity, ActiveLease>,
    connect_ledger: HashMap<ConnectIdentity, SharedSessionAttachment>,
    // One bounded pointer per client into `connect_ledger`. It distinguishes
    // retrying the current request after release/expiry (which may renew) from
    // replaying an older request after a newer attach incarnation superseded
    // it (which must return its frozen result without revoking the newer lease).
    latest_connect_identities: HashMap<ClientIdentity, ConnectIdentity>,
    prompt_ledger: HashMap<PromptIdentity, PromptLedgerEntry>,
    waiting_prompts: VecDeque<QueuedPromptRecord>,
    waiting_bytes: usize,
    next_enqueue_seq: u64,
    active_turn: Option<BrokerActiveTurn>,
    interactions: SharedInteractions,
    interaction_claims: HashMap<SharedInteractionKind, ActiveInteractionClaim>,
    host_owned_work: HashSet<uuid::Uuid>,
    idle_zero_since: Option<tokio::time::Instant>,
    failed_zero_since: Option<tokio::time::Instant>,
    bootstrap_started_at: tokio::time::Instant,
    bootstrap_duration: Option<Duration>,
    cleanup_started_at: Option<tokio::time::Instant>,
    cleanup_duration: Option<Duration>,
    notify: Arc<Notify>,
    expired_leases: VecDeque<String>,
    replaced_failed_generation: Option<u64>,
    _created_at: tokio::time::Instant,
    _created_at_utc: DateTime<Utc>,
    connect_count: u64,
}

impl SharedSessionRecord {
    fn reserved(
        request: &SharedReserveRequest,
        generation: u64,
        replaced_failed_generation: Option<u64>,
    ) -> Self {
        let (registration_tx, _) = watch::channel(SharedRegistrationState::reserved());
        let (lifecycle_tx, _) = watch::channel(SharedLifecycleState::Active);
        Self {
            generation,
            connection_id: request.connection_id.clone(),
            launch_identity: request.launch_identity.clone(),
            phase: SharedSessionPhase::Reserved,
            cleanup_complete: false,
            state: None,
            emitter: None,
            driver_incarnation: None,
            child_pid: None,
            replacement_permit: None,
            registration_tx,
            lifecycle_tx,
            active_leases: HashMap::new(),
            connect_ledger: HashMap::new(),
            latest_connect_identities: HashMap::new(),
            prompt_ledger: HashMap::new(),
            waiting_prompts: VecDeque::new(),
            waiting_bytes: 0,
            next_enqueue_seq: 1,
            active_turn: None,
            interactions: SharedInteractions::default(),
            interaction_claims: HashMap::new(),
            host_owned_work: HashSet::new(),
            idle_zero_since: None,
            failed_zero_since: None,
            bootstrap_started_at: request.now,
            bootstrap_duration: None,
            cleanup_started_at: None,
            cleanup_duration: None,
            notify: Arc::new(Notify::new()),
            expired_leases: VecDeque::new(),
            replaced_failed_generation,
            _created_at: request.now,
            _created_at_utc: request.now_utc,
            connect_count: 0,
        }
    }

    fn has_broker_occupants(&self) -> bool {
        !self.active_leases.is_empty()
            || self.active_turn.is_some()
            || !self.waiting_prompts.is_empty()
            || !self.host_owned_work.is_empty()
            || self.interactions.has_any()
    }

    fn publish_registration(&self) {
        self.registration_tx.send_replace(SharedRegistrationState {
            phase: self.phase.clone(),
            state: self.state.clone(),
            emitter: self.emitter.clone(),
            driver_incarnation: self.driver_incarnation.clone(),
        });
    }

    fn finish_bootstrap(&mut self, now: tokio::time::Instant) {
        self.bootstrap_duration
            .get_or_insert_with(|| now.saturating_duration_since(self.bootstrap_started_at));
    }

    fn begin_cleanup(&mut self, now: tokio::time::Instant) {
        self.finish_bootstrap(now);
        self.cleanup_started_at.get_or_insert(now);
    }

    fn finish_cleanup(&mut self, now: tokio::time::Instant) {
        self.begin_cleanup(now);
        let started_at = self.cleanup_started_at.expect("cleanup start is set");
        self.cleanup_duration
            .get_or_insert_with(|| now.saturating_duration_since(started_at));
    }

    fn bootstrap_duration(&self, now: tokio::time::Instant) -> Duration {
        self.bootstrap_duration
            .unwrap_or_else(|| now.saturating_duration_since(self.bootstrap_started_at))
    }

    fn cleanup_duration(&self, now: tokio::time::Instant) -> Duration {
        self.cleanup_duration.unwrap_or_else(|| {
            self.cleanup_started_at
                .map(|started_at| now.saturating_duration_since(started_at))
                .unwrap_or_default()
        })
    }

    fn check_attach_identity(
        &mut self,
        requested: &SharedLaunchIdentity,
    ) -> Result<(), SharedSessionError> {
        let conflict_kind = if self.launch_identity.agent_type != requested.agent_type {
            Some(SharedConfigConflictKind::AgentType)
        } else if self.launch_identity.working_dir_fingerprint != requested.working_dir_fingerprint
        {
            Some(SharedConfigConflictKind::WorkingDirectory)
        } else if Self::external_session_ids_conflict(
            &self.launch_identity.external_session_id,
            &requested.external_session_id,
        ) {
            Some(SharedConfigConflictKind::ExternalSession)
        } else if self.launch_identity.attach_mode != requested.attach_mode {
            Some(SharedConfigConflictKind::AttachMode)
        } else if self.launch_identity.route_fingerprint != requested.route_fingerprint {
            Some(SharedConfigConflictKind::DelegationRoute)
        } else if self.launch_identity.terminal_shell_fingerprint
            != requested.terminal_shell_fingerprint
        {
            Some(SharedConfigConflictKind::TerminalShell)
        } else if self.launch_identity.purpose != requested.purpose {
            Some(SharedConfigConflictKind::Purpose)
        } else {
            None
        };

        if let Some(conflict_kind) = conflict_kind {
            return Err(SharedSessionError::ConfigConflict {
                connection_id: self.connection_id.clone(),
                conflict_kind,
            });
        }
        // Conversation roots freeze launch identity at reserve, before
        // SessionStarted persists the agent session id. Later attachers learn
        // that id from the conversation row and must join the live process
        // instead of 409. Promote None → Some so a later different id conflicts.
        if self.launch_identity.external_session_id.is_none() {
            self.launch_identity.external_session_id = requested.external_session_id.clone();
        }
        Ok(())
    }

    fn external_session_ids_conflict(stored: &Option<String>, requested: &Option<String>) -> bool {
        match (stored.as_deref(), requested.as_deref()) {
            (Some(stored), Some(requested)) => stored != requested,
            _ => false,
        }
    }

    fn retry_decision(
        &self,
        request: &SharedReserveRequest,
    ) -> Result<FailedRetryDecision, SharedSessionError> {
        match &self.phase {
            SharedSessionPhase::Closing => Err(SharedSessionError::Closing),
            SharedSessionPhase::Failed { .. } => {
                let Some(retry_generation) = request.retry_failed_generation else {
                    return Ok(FailedRetryDecision::Attach);
                };
                if retry_generation != self.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                if !self.cleanup_complete {
                    return Err(SharedSessionError::CleanupInProgress);
                }
                Ok(FailedRetryDecision::Replace {
                    failed_generation: retry_generation,
                })
            }
            SharedSessionPhase::Reserved
            | SharedSessionPhase::Bootstrapping
            | SharedSessionPhase::Ready => match request.retry_failed_generation {
                None => Ok(FailedRetryDecision::Attach),
                Some(failed_generation)
                    if self.replaced_failed_generation == Some(failed_generation)
                        && self.generation == failed_generation.saturating_add(1) =>
                {
                    Ok(FailedRetryDecision::Attach)
                }
                Some(_) => Err(SharedSessionError::GenerationStale),
            },
        }
    }

    fn prune_expired_leases(&mut self, now: tokio::time::Instant) -> Vec<String> {
        let expired_clients: Vec<_> = self
            .active_leases
            .iter()
            .filter(|(_, lease)| lease.expires_at <= now)
            .map(|(client, _)| client.clone())
            .collect();
        let mut expired_ids = Vec::with_capacity(expired_clients.len());
        for client in &expired_clients {
            if let Some(lease) = self.active_leases.remove(client) {
                expired_ids.push(lease.lease_id.clone());
                self.expired_leases.push_back(lease.lease_id);
                if self.expired_leases.len() > MAX_EXPIRED_LEASE_TOMBSTONES {
                    self.expired_leases.pop_front();
                }
            }
        }
        expired_ids
    }

    fn attach_or_renew_lease(
        &mut self,
        request: &SharedReserveRequest,
        lease_ttl: Duration,
        disposition: SharedDisposition,
        limits: BrokerLimits,
    ) -> Result<(SharedSessionAttachment, bool), SharedSessionError> {
        let client_identity = ClientIdentity::from_request(request);
        let connect_identity = ConnectIdentity::from_request(request);

        if let Some(previous) = self.connect_ledger.get(&connect_identity) {
            let is_active = self
                .active_leases
                .get(&client_identity)
                .is_some_and(|lease| lease.lease_id == previous.lease_id);
            if is_active {
                self.idle_zero_since = None;
                self.failed_zero_since = None;
                self.connect_count = self.connect_count.saturating_add(1);
                return Ok((previous.clone(), false));
            }
            let was_superseded = self
                .latest_connect_identities
                .get(&client_identity)
                .is_some_and(|latest| latest != &connect_identity);
            if was_superseded {
                // The ledger is the idempotency result. Replaying it is not a
                // new attach and therefore cannot rotate the current lease.
                self.connect_count = self.connect_count.saturating_add(1);
                return Ok((previous.clone(), false));
            }
        }

        let is_new_client = !self.active_leases.contains_key(&client_identity);
        if is_new_client && self.active_leases.len() >= limits.max_active_leases {
            return Err(SharedSessionError::ClientLeaseCapacityExceeded);
        }
        if !self.connect_ledger.contains_key(&connect_identity)
            && self.connect_ledger.len() >= limits.max_connect_ledger_entries
        {
            return Err(SharedSessionError::ConnectLedgerCapacityExceeded);
        }

        let monotonic_expiry = request.now + lease_ttl;
        let wall_expiry = request.now_utc
            + chrono::Duration::from_std(lease_ttl)
                .expect("shared session lease TTL must fit chrono::Duration");
        let lease_id = uuid::Uuid::new_v4().to_string();
        let lease = ActiveLease {
            lease_id: lease_id.clone(),
            expires_at: monotonic_expiry,
            expires_at_utc: wall_expiry,
        };
        self.active_leases.insert(client_identity.clone(), lease);

        let attachment = SharedSessionAttachment {
            connection_id: self.connection_id.clone(),
            generation: self.generation,
            lease_id,
            lease_expires_at: wall_expiry,
            disposition,
            phase: self.phase.clone(),
        };
        self.latest_connect_identities
            .insert(client_identity, connect_identity.clone());
        self.connect_ledger
            .insert(connect_identity, attachment.clone());
        self.idle_zero_since = None;
        self.failed_zero_since = None;
        self.connect_count = self.connect_count.saturating_add(1);
        Ok((attachment, is_new_client))
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct SharedRegistrationState {
    pub phase: SharedSessionPhase,
    pub state: Option<Arc<RwLock<SessionState>>>,
    pub emitter: Option<EventEmitter>,
    pub driver_incarnation: Option<String>,
}

impl SharedRegistrationState {
    fn reserved() -> Self {
        Self {
            phase: SharedSessionPhase::Reserved,
            state: None,
            emitter: None,
            driver_incarnation: None,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedLifecycleState {
    Active,
    Failed,
    Closing,
    Removed,
    Replaced,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientIdentity {
    client_instance_id: String,
}

impl ClientIdentity {
    fn from_request(request: &SharedReserveRequest) -> Self {
        Self {
            client_instance_id: request.client_instance_id.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ConnectIdentity {
    client_instance_id: String,
    device_id: String,
    request_id: String,
}

impl ConnectIdentity {
    fn from_request(request: &SharedReserveRequest) -> Self {
        Self {
            client_instance_id: request.client_instance_id.clone(),
            device_id: request.device_id.clone(),
            request_id: request.request_id.clone(),
        }
    }
}

struct ActiveLease {
    lease_id: String,
    expires_at: tokio::time::Instant,
    expires_at_utc: DateTime<Utc>,
}

enum ReserveLookup {
    Created(SharedSessionAttachment),
    Existing(Arc<Mutex<SharedSessionRecord>>),
}

enum FailedRetryDecision {
    Attach,
    Replace { failed_generation: u64 },
}

enum ReserveDecision {
    Attach(SharedSessionAttachment),
    Replace { failed_generation: u64 },
}

#[cfg(test)]
include!("shared_session/tests.rs");
