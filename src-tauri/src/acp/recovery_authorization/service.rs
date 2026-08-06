use std::collections::HashMap;
#[cfg(test)]
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::acp::manager::ConnectionManager;
use crate::acp::question::{
    QuestionOption, QuestionOutcome, QuestionSpec, RecoveryQuestionPresentation,
};
use crate::db::entities::recovery_authorization::{self, RecoveryAuthorizationStatus};

use super::{
    canonical_json, derive_recovery_action_metadata, PreparedAuthorization,
    RecoveryAuthorizationError, RecoveryAuthorizationResult, RecoveryAuthorizationStore,
    RecoveryChallenge, APPROVAL_TTL, RECOVERY_APPROVE_LABEL, RECOVERY_DECLINE_LABEL,
};

const PRUNE_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

pub(crate) fn recovery_cleanup_retry_delay(failed_attempts: u32) -> StdDuration {
    let initial = if cfg!(test) {
        StdDuration::from_millis(1)
    } else {
        StdDuration::from_millis(100)
    };
    let maximum = if cfg!(test) {
        StdDuration::from_millis(8)
    } else {
        StdDuration::from_secs(5)
    };
    initial
        .saturating_mul(1_u32 << failed_attempts.min(6))
        .min(maximum)
}

pub trait RecoveryAuthorizationClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

struct SystemRecoveryAuthorizationClock;

impl RecoveryAuthorizationClock for SystemRecoveryAuthorizationClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone)]
pub struct RecoveryAuthorizationService {
    store: RecoveryAuthorizationStore,
    clock: Arc<dyn RecoveryAuthorizationClock>,
    notifications: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    #[cfg(test)]
    injected_failures: Arc<Mutex<VecDeque<InjectedRecoveryWriteFailure>>>,
    #[cfg(test)]
    injected_pause: Arc<Mutex<Option<(InjectedRecoveryWriteFailure, InjectedRecoveryWritePause)>>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectedRecoveryWriteFailure {
    Bind,
    Resolve,
    Abandon,
}

#[cfg(test)]
#[derive(Clone)]
pub struct InjectedRecoveryWritePause {
    pub reached: Arc<Notify>,
    pub release: Arc<Notify>,
}

impl RecoveryAuthorizationService {
    pub fn new(conn: sea_orm::DatabaseConnection) -> Self {
        Self::with_clock(conn, Arc::new(SystemRecoveryAuthorizationClock))
    }

    pub fn with_clock(
        conn: sea_orm::DatabaseConnection,
        clock: Arc<dyn RecoveryAuthorizationClock>,
    ) -> Self {
        Self {
            store: RecoveryAuthorizationStore::new(conn),
            clock,
            notifications: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            injected_failures: Arc::new(Mutex::new(VecDeque::new())),
            #[cfg(test)]
            injected_pause: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn fail_next_write(&self, failure: InjectedRecoveryWriteFailure) {
        self.injected_failures
            .lock()
            .expect("recovery failure injection lock poisoned")
            .push_back(failure);
    }

    #[cfg(test)]
    pub fn pause_next_write(
        &self,
        failure: InjectedRecoveryWriteFailure,
    ) -> InjectedRecoveryWritePause {
        let pause = InjectedRecoveryWritePause {
            reached: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        *self
            .injected_pause
            .lock()
            .expect("recovery pause injection lock poisoned") = Some((failure, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn maybe_fail_write(
        &self,
        failure: InjectedRecoveryWriteFailure,
    ) -> Result<(), RecoveryAuthorizationError> {
        let pause = {
            let mut injected = self
                .injected_pause
                .lock()
                .expect("recovery pause injection lock poisoned");
            if injected
                .as_ref()
                .is_some_and(|(write, _)| *write == failure)
            {
                injected.take().map(|(_, pause)| pause)
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
        let mut injected = self
            .injected_failures
            .lock()
            .expect("recovery failure injection lock poisoned");
        if injected.front() == Some(&failure) {
            injected.pop_front();
            Err(RecoveryAuthorizationError::Database(format!(
                "injected {failure:?} failure"
            )))
        } else {
            Ok(())
        }
    }

    pub fn store(&self) -> &RecoveryAuthorizationStore {
        &self.store
    }

    /// Start the background prune loop.
    ///
    /// Desktop Tauri `setup` builds the delegation stack on the main thread
    /// **outside** a Tokio context, so plain `tokio::spawn` panics with
    /// "there is no reactor running". Match the rest of the boot stack
    /// (`spawn_tool_watchdog`, `spawn_delegation_supervisor`): use Tauri's
    /// global async runtime under `tauri-runtime`, and `tokio::spawn` only
    /// on the server binary path where a runtime is already entered.
    pub fn start_maintenance(self: &Arc<Self>) {
        let service = Arc::downgrade(self);
        let run = async move {
            run_maintenance(service).await;
        };
        #[cfg(feature = "tauri-runtime")]
        tauri::async_runtime::spawn(run);
        #[cfg(not(feature = "tauri-runtime"))]
        tokio::spawn(run);
    }

    pub async fn prepare(
        &self,
        challenge: RecoveryChallenge,
    ) -> Result<PreparedAuthorization, RecoveryAuthorizationError> {
        loop {
            if let Some(row) = self.store.find_active(&challenge).await? {
                if !row_matches_challenge(&row, &challenge)? {
                    return Err(RecoveryAuthorizationError::ChallengeConflict);
                }
                if row.status == RecoveryAuthorizationStatus::Approved
                    && row
                        .expires_at
                        .is_some_and(|expires_at| self.clock.now() >= expires_at)
                {
                    self.store
                        .expire_if_due(&row.authorization_id, self.clock.now())
                        .await?;
                    self.notify(&row.authorization_id);
                    continue;
                }
                return Ok(match row.status {
                    RecoveryAuthorizationStatus::Pending => PreparedAuthorization::Pending {
                        row,
                        newly_created: false,
                    },
                    RecoveryAuthorizationStatus::Approved => {
                        PreparedAuthorization::ExistingApproved((&row).into())
                    }
                    _ => unreachable!("active query returned terminal status"),
                });
            }

            match self
                .store
                .insert_pending(&challenge, self.clock.now())
                .await
            {
                Ok(row) => {
                    self.notification(&row.authorization_id);
                    return Ok(PreparedAuthorization::Pending {
                        row,
                        newly_created: true,
                    });
                }
                Err(insert_error) => {
                    if self.store.find_active(&challenge).await?.is_some() {
                        continue;
                    }
                    return Err(insert_error);
                }
            }
        }
    }

    pub async fn request(
        &self,
        manager: &ConnectionManager,
        challenge: RecoveryChallenge,
        cancelled: CancellationToken,
    ) -> Result<RecoveryAuthorizationResult, RecoveryAuthorizationError> {
        let parent_conversation_id = challenge.parent_conversation_id;
        let target =
            derive_recovery_action_metadata(challenge.allowed_action, &challenge.action_payload)
                .ok_or(RecoveryAuthorizationError::PayloadMismatch)?
                .target_code
                .to_string();
        let presentation = RecoveryQuestionPresentation {
            subject: challenge.subject_kind.as_str().to_string(),
            action: challenge.allowed_action.as_str().to_string(),
            target,
            cause: challenge.cause_code.clone(),
            risk: challenge.risk_class.clone(),
            display_reason: challenge.display_reason.clone(),
        };
        match self.prepare(challenge).await? {
            PreparedAuthorization::ExistingApproved(result) => Ok(result),
            PreparedAuthorization::Pending {
                row,
                newly_created: false,
            } => {
                self.wait_for_resolution(&row.authorization_id, cancelled)
                    .await
            }
            PreparedAuthorization::Pending {
                row,
                newly_created: true,
            } => {
                let questions = vec![QuestionSpec {
                    id: uuid::Uuid::new_v4().to_string(),
                    question: "recovery_authorization".to_string(),
                    header: "Recovery".to_string(),
                    multi_select: false,
                    options: vec![
                        QuestionOption {
                            label: RECOVERY_APPROVE_LABEL.to_string(),
                            description: String::new(),
                        },
                        QuestionOption {
                            label: RECOVERY_DECLINE_LABEL.to_string(),
                            description: String::new(),
                        },
                    ],
                    is_secret: false,
                    recovery: Some(presentation),
                }];
                let registration = match manager
                    .register_recovery_question(
                        parent_conversation_id,
                        row.authorization_id.clone(),
                        questions,
                    )
                    .await
                {
                    Ok(registration) => registration,
                    Err(_) => {
                        let cleanup = self.spawn_unbound_abandonment(row.authorization_id);
                        Self::await_owned_cleanup(cleanup).await?;
                        return Err(RecoveryAuthorizationError::Blocked);
                    }
                };
                if let Err(error) = self
                    .bind_question(&row.authorization_id, &registration.question_id)
                    .await
                {
                    let cleanup = self.spawn_card_abandonment(
                        manager,
                        row.authorization_id,
                        registration.question_id,
                    );
                    return match Self::await_owned_cleanup(cleanup).await {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(cleanup_error),
                    };
                }
                tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => {
                        let cleanup = self.spawn_card_abandonment(
                            manager,
                            row.authorization_id,
                            registration.question_id,
                        );
                        Self::await_owned_cleanup(cleanup).await?;
                        Err(RecoveryAuthorizationError::Cancelled)
                    }
                    outcome = registration.answer_rx => {
                        match outcome {
                            Ok(outcome) => self.resolve_question(&row.authorization_id, outcome).await,
                            Err(_) => {
                                let cleanup = self.spawn_card_abandonment(
                                    manager,
                                    row.authorization_id.clone(),
                                    registration.question_id,
                                );
                                Self::await_owned_cleanup(cleanup).await?;
                                self.wait_for_resolution(&row.authorization_id, CancellationToken::new()).await
                            }
                        }
                    }
                }
            }
            PreparedAuthorization::NotRequired { .. } | PreparedAuthorization::HardStop { .. } => {
                Err(RecoveryAuthorizationError::ChallengeConflict)
            }
        }
    }

    pub async fn bind_question(
        &self,
        authorization_id: &str,
        question_id: &str,
    ) -> Result<(), RecoveryAuthorizationError> {
        #[cfg(test)]
        self.maybe_fail_write(InjectedRecoveryWriteFailure::Bind)
            .await?;
        self.store
            .bind_question(authorization_id, question_id)
            .await?;
        Ok(())
    }

    pub async fn resolve_question(
        &self,
        authorization_id: &str,
        outcome: QuestionOutcome,
    ) -> Result<RecoveryAuthorizationResult, RecoveryAuthorizationError> {
        #[cfg(test)]
        self.maybe_fail_write(InjectedRecoveryWriteFailure::Resolve)
            .await?;
        let approve = !outcome.declined
            && outcome.answers.len() == 1
            && outcome.answers[0].selected.as_slice() == [RECOVERY_APPROVE_LABEL];
        let row = if approve {
            let approved_at = self.clock.now();
            self.store
                .approve_pending(authorization_id, approved_at, approved_at + APPROVAL_TTL)
                .await?
        } else {
            self.store.decline_pending(authorization_id).await?
        };
        self.notify(authorization_id);
        Ok((&row).into())
    }

    pub async fn abandon_question(
        &self,
        authorization_id: &str,
        question_id: &str,
    ) -> Result<(), RecoveryAuthorizationError> {
        self.abandon_pending_once(authorization_id, Some(question_id))
            .await?;
        self.notify(authorization_id);
        Ok(())
    }

    pub(crate) async fn abandon_until_terminal(
        &self,
        authorization_id: &str,
        question_id: Option<&str>,
    ) -> Result<(), RecoveryAuthorizationError> {
        let mut failed_attempts = 0_u32;
        loop {
            match self
                .abandon_pending_once(authorization_id, question_id)
                .await
            {
                Ok(row) if row.status != RecoveryAuthorizationStatus::Pending => {
                    self.notify(authorization_id);
                    return Ok(());
                }
                Ok(_) => {
                    let error = RecoveryAuthorizationError::QuestionBindingConflict;
                    tracing::error!(
                        code = error.code(),
                        "[recovery_authorization] abandonment stopped on nonterminal row"
                    );
                    return Err(error);
                }
                Err(RecoveryAuthorizationError::NotFound) => {
                    self.notify(authorization_id);
                    return Ok(());
                }
                Err(error @ RecoveryAuthorizationError::Database(_)) => {
                    let delay = recovery_cleanup_retry_delay(failed_attempts);
                    failed_attempts = failed_attempts.saturating_add(1);
                    tracing::warn!(
                        code = error.code(),
                        retry_delay_ms = delay.as_millis(),
                        "[recovery_authorization] abandonment database retry"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    tracing::error!(
                        code = error.code(),
                        "[recovery_authorization] abandonment stopped on nontransient error"
                    );
                    return Err(error);
                }
            }
        }
    }

    async fn abandon_pending_once(
        &self,
        authorization_id: &str,
        question_id: Option<&str>,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        #[cfg(test)]
        self.maybe_fail_write(InjectedRecoveryWriteFailure::Abandon)
            .await?;
        self.store
            .abandon_pending(authorization_id, question_id)
            .await
    }

    fn spawn_unbound_abandonment(
        &self,
        authorization_id: String,
    ) -> tokio::task::JoinHandle<Result<(), RecoveryAuthorizationError>> {
        let service = self.clone();
        tokio::spawn(async move {
            service
                .abandon_until_terminal(&authorization_id, None)
                .await
        })
    }

    fn spawn_card_abandonment(
        &self,
        manager: &ConnectionManager,
        authorization_id: String,
        question_id: String,
    ) -> tokio::task::JoinHandle<Result<(), RecoveryAuthorizationError>> {
        let manager = manager.clone_ref();
        tokio::spawn(async move {
            manager
                .abandon_recovery_question_until_terminal(&authorization_id, &question_id)
                .await
        })
    }

    async fn await_owned_cleanup(
        cleanup: tokio::task::JoinHandle<Result<(), RecoveryAuthorizationError>>,
    ) -> Result<(), RecoveryAuthorizationError> {
        cleanup.await.map_err(|error| {
            RecoveryAuthorizationError::Database(format!(
                "owned recovery cleanup task failed: {error}"
            ))
        })?
    }

    pub async fn get(
        &self,
        authorization_id: &str,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        let row = self
            .store
            .find_by_id(authorization_id)
            .await?
            .ok_or(RecoveryAuthorizationError::NotFound)?;
        if row.status == RecoveryAuthorizationStatus::Approved
            && row
                .expires_at
                .is_some_and(|expires_at| self.clock.now() >= expires_at)
        {
            let expired = self
                .store
                .expire_if_due(authorization_id, self.clock.now())
                .await?;
            self.notify(authorization_id);
            Ok(expired)
        } else {
            Ok(row)
        }
    }

    pub async fn wait_for_resolution(
        &self,
        authorization_id: &str,
        cancelled: CancellationToken,
    ) -> Result<RecoveryAuthorizationResult, RecoveryAuthorizationError> {
        let notification = self.notification(authorization_id);
        loop {
            let notified = notification.notified();
            let row = self.get(authorization_id).await?;
            if row.status != RecoveryAuthorizationStatus::Pending {
                return Ok((&row).into());
            }
            tokio::select! {
                _ = notified => {}
                _ = cancelled.cancelled() => return Err(RecoveryAuthorizationError::Cancelled),
            }
        }
    }

    pub async fn prune_now(&self) -> Result<u64, RecoveryAuthorizationError> {
        self.store.prune_terminal(self.clock.now()).await
    }

    fn notification(&self, authorization_id: &str) -> Arc<Notify> {
        self.notifications
            .lock()
            .expect("recovery notification lock poisoned")
            .entry(authorization_id.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    fn notify(&self, authorization_id: &str) {
        if let Some(notification) = self
            .notifications
            .lock()
            .expect("recovery notification lock poisoned")
            .remove(authorization_id)
        {
            notification.notify_waiters();
        }
    }
}

async fn run_maintenance(service: Weak<RecoveryAuthorizationService>) {
    let mut interval = tokio::time::interval(PRUNE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let Some(service) = service.upgrade() else {
            return;
        };
        if let Err(error) = service.prune_now().await {
            tracing::warn!(error = %error, "[recovery_authorization] terminal prune failed");
        }
        drop(service);
    }
}

fn row_matches_challenge(
    row: &recovery_authorization::Model,
    challenge: &RecoveryChallenge,
) -> Result<bool, RecoveryAuthorizationError> {
    let identity = challenge.delegation_identity.as_ref();
    Ok(
        row.parent_conversation_id == challenge.parent_conversation_id
            && row.subject_kind == challenge.subject_kind.as_str()
            && row.subject_id == challenge.subject_id
            && row.source_task_id.as_deref() == identity.map(|value| value.source_task_id.as_str())
            && row.child_conversation_id == identity.and_then(|value| value.child_conversation_id)
            && row.lineage_root_task_id.as_deref()
                == identity.map(|value| value.lineage_root_task_id.as_str())
            && row.work_unit_key.as_deref()
                == identity.and_then(|value| value.work_unit_key.as_deref())
            && row.source_state_fingerprint == challenge.source_state_fingerprint
            && row.allowed_action == challenge.allowed_action.as_str()
            && row.action_payload_json == canonical_json(&challenge.action_payload)?
            && row.cause_code == challenge.cause_code
            && row.risk_class == challenge.risk_class
            && row.display_reason == challenge.display_reason,
    )
}
