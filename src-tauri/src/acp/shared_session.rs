mod dto;
mod error;
mod metrics;

pub use dto::*;
pub use error::{validate_client_label, SharedSessionError};
pub use metrics::{SharedSessionMetrics, SharedSessionMetricsSnapshot};

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use tokio::sync::{watch, Mutex, RwLock};

use crate::{acp::session_state::SessionState, web::event_bridge::EventEmitter};

use error::validate_failure_code;

#[derive(Clone)]
pub struct SharedSessionBroker {
    index: Arc<Mutex<SharedSessionIndex>>,
    index_epoch: Arc<watch::Sender<u64>>,
    metrics: Arc<SharedSessionMetrics>,
    lease_ttl: Duration,
    limits: BrokerLimits,
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
            lease_ttl: DEFAULT_CLIENT_LEASE_TTL,
            limits: BrokerLimits::default(),
        }
    }
}

impl SharedSessionBroker {
    pub fn metrics(&self) -> &SharedSessionMetrics {
        &self.metrics
    }

    #[cfg(test)]
    fn with_limits_for_test(max_active_leases: usize, max_connect_ledger_entries: usize) -> Self {
        Self {
            limits: BrokerLimits {
                max_active_leases,
                max_connect_ledger_entries,
            },
            ..Self::default()
        }
    }

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
                    let mut initial = SharedSessionRecord::reserved(&request, 1, None);
                    let attachment = match initial.attach_or_renew_lease(
                        &request,
                        self.lease_ttl,
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

            let decision = {
                let mut current = record.lock().await;
                current.check_attach_identity(&request.launch_identity)?;
                match current.retry_decision(&request)? {
                    FailedRetryDecision::Attach => {
                        let expired = current.prune_expired_leases(request.now);
                        self.metrics.remove_active_leases(expired);
                        match current.attach_or_renew_lease(
                            &request,
                            self.lease_ttl,
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
            let phase = SharedSessionPhase::Failed {
                error_code: error_code.clone(),
                cleanup_complete,
            };
            if let Some(state) = state {
                update_public_shared_phase(state, generation, phase.clone());
            }
            record.cleanup_complete = cleanup_complete;
            record.phase = phase;
            record.publish_registration();
            record
                .lifecycle_tx
                .send_replace(SharedLifecycleState::Failed);
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

    pub async fn mark_cleanup_complete(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<Vec<crate::acp::types::AcpEvent>, SharedSessionError> {
        self.with_authoritative_record_and_state(connection_id, None, |record, state| {
            if record.connection_id != connection_id || record.generation != generation {
                return Err(SharedSessionError::GenerationStale);
            }
            let error_code = match &record.phase {
                SharedSessionPhase::Failed { error_code, .. } => error_code.clone(),
                _ => return Err(SharedSessionError::SessionUnavailable),
            };
            let phase = SharedSessionPhase::Failed {
                error_code,
                cleanup_complete: true,
            };
            if let Some(state) = state {
                update_public_shared_phase(state, generation, phase.clone());
            }
            record.cleanup_complete = true;
            record.phase = phase;
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
                queue: Vec::new(),
                active_turn: None,
                lease_expires_at: None,
            })
        })
        .await
        .ok()
        .flatten()
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
            update_public_shared_phase(state, generation, SharedSessionPhase::Ready);
            record.phase = SharedSessionPhase::Ready;
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
                record.cleanup_complete = cleanup_complete;
                record.phase = phase;
                record.replacement_permit = None;
                record.publish_registration();
                record
                    .lifecycle_tx
                    .send_replace(SharedLifecycleState::Failed);
                Ok((
                    state,
                    emitter,
                    vec![crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                        generation,
                        phase: record.phase.clone(),
                    }],
                ))
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
                record.cleanup_complete = cleanup_complete;
                record.phase = phase;
                record.replacement_permit = None;
                record.publish_registration();
                record
                    .lifecycle_tx
                    .send_replace(SharedLifecycleState::Failed);
                Ok((
                    state,
                    emitter,
                    vec![crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                        generation: permit.generation,
                        phase: record.phase.clone(),
                    }],
                ))
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
                index.sessions.get(key).cloned()
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
        let is_authoritative = index
            .sessions
            .get(&request.key)
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
        let next_generation = failed_generation
            .checked_add(1)
            .ok_or(SharedSessionError::GenerationStale)?;
        let mut replacement =
            SharedSessionRecord::reserved(request, next_generation, Some(failed_generation));
        let (attachment, added_lease) = match replacement.attach_or_renew_lease(
            request,
            self.lease_ttl,
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
        index.by_connection.remove(&old_connection_id);
        index
            .by_connection
            .insert(request.connection_id.clone(), request.key.clone());
        index.sessions.insert(request.key.clone(), replacement);
        self.index_epoch
            .send_modify(|epoch| *epoch = epoch.saturating_add(1));
        self.metrics.remove_active_leases(old_active_leases);
        self.metrics.add_active_leases(1);

        Ok(Some(SharedReserveOutcome {
            attachment,
            created: true,
        }))
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
            });
        }
    }
}

#[derive(Clone, Copy)]
struct BrokerLimits {
    max_active_leases: usize,
    max_connect_ledger_entries: usize,
}

impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_active_leases: MAX_ACTIVE_LEASES,
            max_connect_ledger_entries: MAX_CONNECT_LEDGER_ENTRIES,
        }
    }
}

#[derive(Default)]
struct SharedSessionIndex {
    sessions: HashMap<SharedSessionKey, Arc<Mutex<SharedSessionRecord>>>,
    by_connection: HashMap<String, SharedSessionKey>,
}

impl SharedSessionIndex {
    fn record_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<&Arc<Mutex<SharedSessionRecord>>> {
        let key = self.by_connection.get(connection_id)?;
        self.sessions.get(key)
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
            expired_leases: VecDeque::new(),
            replaced_failed_generation,
            _created_at: request.now,
            _created_at_utc: request.now_utc,
            connect_count: 0,
        }
    }

    fn publish_registration(&self) {
        self.registration_tx.send_replace(SharedRegistrationState {
            phase: self.phase.clone(),
            state: self.state.clone(),
            emitter: self.emitter.clone(),
            driver_incarnation: self.driver_incarnation.clone(),
        });
    }

    fn check_attach_identity(
        &self,
        requested: &SharedLaunchIdentity,
    ) -> Result<(), SharedSessionError> {
        let conflict_kind = if self.launch_identity.agent_type != requested.agent_type {
            Some(SharedConfigConflictKind::AgentType)
        } else if self.launch_identity.working_dir_fingerprint != requested.working_dir_fingerprint
        {
            Some(SharedConfigConflictKind::WorkingDirectory)
        } else if self.launch_identity.external_session_id != requested.external_session_id {
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
        Ok(())
    }

    fn retry_decision(
        &self,
        request: &SharedReserveRequest,
    ) -> Result<FailedRetryDecision, SharedSessionError> {
        match &self.phase {
            SharedSessionPhase::Closing => Err(SharedSessionError::Closing),
            SharedSessionPhase::Failed { .. } => {
                let retry_generation = request
                    .retry_failed_generation
                    .ok_or(SharedSessionError::SessionUnavailable)?;
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

    fn prune_expired_leases(&mut self, now: tokio::time::Instant) -> usize {
        let expired_clients: Vec<_> = self
            .active_leases
            .iter()
            .filter(|(_, lease)| lease.expires_at <= now)
            .map(|(client, _)| client.clone())
            .collect();
        for client in &expired_clients {
            if let Some(lease) = self.active_leases.remove(client) {
                self.expired_leases.push_back(lease.lease_id);
                if self.expired_leases.len() > MAX_EXPIRED_LEASE_TOMBSTONES {
                    self.expired_leases.pop_front();
                }
            }
        }
        expired_clients.len()
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
        let lease = self
            .active_leases
            .entry(client_identity)
            .or_insert_with(|| ActiveLease {
                lease_id: uuid::Uuid::new_v4().to_string(),
                expires_at: monotonic_expiry,
                expires_at_utc: wall_expiry,
            });
        lease.expires_at = monotonic_expiry;
        lease.expires_at_utc = wall_expiry;

        let attachment = SharedSessionAttachment {
            connection_id: self.connection_id.clone(),
            generation: self.generation,
            lease_id: lease.lease_id.clone(),
            lease_expires_at: lease.expires_at_utc,
            disposition,
            phase: self.phase.clone(),
        };
        self.connect_ledger
            .insert(connect_identity, attachment.clone());
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
enum SharedLifecycleState {
    Active,
    Failed,
    Closing,
    Removed,
    Replaced,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientIdentity {
    client_instance_id: String,
    device_id: String,
}

impl ClientIdentity {
    fn from_request(request: &SharedReserveRequest) -> Self {
        Self {
            client_instance_id: request.client_instance_id.clone(),
            device_id: request.device_id.clone(),
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
