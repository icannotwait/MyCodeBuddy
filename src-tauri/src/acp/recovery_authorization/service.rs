use std::collections::HashMap;
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
    canonical_json, PreparedAuthorization, RecoveryAuthorizationError, RecoveryAuthorizationResult,
    RecoveryAuthorizationStore, RecoveryChallenge, APPROVAL_TTL, RECOVERY_APPROVE_LABEL,
    RECOVERY_DECLINE_LABEL,
};

const PRUNE_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

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
    injected_failure: Arc<Mutex<Option<InjectedRecoveryWriteFailure>>>,
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
            injected_failure: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            injected_pause: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn fail_next_write(&self, failure: InjectedRecoveryWriteFailure) {
        *self
            .injected_failure
            .lock()
            .expect("recovery failure injection lock poisoned") = Some(failure);
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
            .injected_failure
            .lock()
            .expect("recovery failure injection lock poisoned");
        if injected.as_ref() == Some(&failure) {
            *injected = None;
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

    pub fn start_maintenance(self: &Arc<Self>) {
        let service = Arc::downgrade(self);
        tokio::spawn(async move {
            run_maintenance(service).await;
        });
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
        let presentation = RecoveryQuestionPresentation {
            subject: challenge.subject_kind.as_str().to_string(),
            action: challenge.allowed_action.as_str().to_string(),
            target: challenge.subject_id.clone(),
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
                        self.store
                            .abandon_pending(&row.authorization_id, None)
                            .await?;
                        self.notify(&row.authorization_id);
                        return Err(RecoveryAuthorizationError::Blocked);
                    }
                };
                if let Err(error) = self
                    .bind_question(&row.authorization_id, &registration.question_id)
                    .await
                {
                    if self
                        .abandon_question(&row.authorization_id, &registration.question_id)
                        .await
                        .is_ok()
                    {
                        manager
                            .finish_question_settlement(&registration.question_id, None)
                            .await;
                    }
                    return Err(error);
                }
                tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => {
                        self.abandon_question(&row.authorization_id, &registration.question_id)
                            .await?;
                        manager
                            .cancel_question("", &registration.question_id)
                            .await;
                        Err(RecoveryAuthorizationError::Cancelled)
                    }
                    outcome = registration.answer_rx => {
                        match outcome {
                            Ok(outcome) => self.resolve_question(&row.authorization_id, outcome).await,
                            Err(_) => {
                                self.abandon_question(&row.authorization_id, &registration.question_id)
                                    .await?;
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
        #[cfg(test)]
        self.maybe_fail_write(InjectedRecoveryWriteFailure::Abandon)
            .await?;
        self.store
            .abandon_pending(authorization_id, Some(question_id))
            .await?;
        self.notify(authorization_id);
        Ok(())
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
