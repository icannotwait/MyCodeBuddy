mod service;
mod store;
mod types;

pub use service::*;
pub use store::*;
pub use types::*;

#[cfg(test)]
#[allow(clippy::module_inception)] // Keeps recovery authorization integration tests beside the module exports.
mod recovery_authorization {
    use std::sync::{Arc, Mutex};
    use std::time::Duration as StdDuration;

    use chrono::{DateTime, Duration, TimeZone, Utc};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
    use serde_json::json;
    use tokio::sync::Barrier;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::acp::manager::ConnectionManager;
    use crate::acp::question::{QuestionAnswer, QuestionAnswerItem};
    use crate::db::entities::recovery_authorization::{self, RecoveryAuthorizationStatus};
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::AgentType;
    use crate::web::event_bridge::EventEmitter;

    #[derive(Clone)]
    struct TestClock(Arc<Mutex<DateTime<Utc>>>);

    impl TestClock {
        fn at(now: DateTime<Utc>) -> Self {
            Self(Arc::new(Mutex::new(now)))
        }

        fn set(&self, now: DateTime<Utc>) {
            *self.0.lock().expect("clock lock") = now;
        }
    }

    impl RecoveryAuthorizationClock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().expect("clock lock")
        }
    }

    fn start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, 1, 2, 3)
            .single()
            .expect("valid time")
    }

    async fn fixture(
        now: DateTime<Utc>,
    ) -> (
        crate::db::AppDatabase,
        i32,
        TestClock,
        RecoveryAuthorizationService,
    ) {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/recovery-authorization").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent conversation");
        let clock = TestClock::at(now);
        let service =
            RecoveryAuthorizationService::with_clock(db.conn.clone(), Arc::new(clock.clone()));
        (db, parent.id, clock, service)
    }

    fn challenge(parent_conversation_id: i32) -> RecoveryChallenge {
        RecoveryChallenge {
            parent_conversation_id,
            subject_kind: RecoverySubjectKind::DelegationTask,
            subject_id: "subject-a".into(),
            delegation_identity: Some(DelegationAuthorizationIdentity {
                source_task_id: "source-a".into(),
                child_conversation_id: Some(41),
                lineage_root_task_id: "root-a".into(),
                work_unit_key: Some("unit-a".into()),
            }),
            source_state_fingerprint: "fingerprint-a".into(),
            allowed_action: RecoveryAllowedAction::Replace,
            action_payload: json!({
                "agent": "codex",
                "nested": {"z": 2, "a": 1},
                "reset_reason_hash": "reason-a"
            }),
            cause_code: "source_unresumable".into(),
            risk_class: "destructive_replacement".into(),
            display_reason: Some("source_session_missing".into()),
        }
    }

    fn expectation<'a>(
        challenge: &'a RecoveryChallenge,
        correlation: &'a str,
    ) -> AuthorizationConsumeExpectation<'a> {
        AuthorizationConsumeExpectation {
            parent_conversation_id: challenge.parent_conversation_id,
            subject_kind: challenge.subject_kind,
            subject_id: &challenge.subject_id,
            source_state_fingerprint: &challenge.source_state_fingerprint,
            allowed_action: challenge.allowed_action,
            action_payload: &challenge.action_payload,
            consumer_kind: RecoveryConsumerKind::DelegationTaskRun,
            consumer_id: "run-a",
            consumer_correlation_id: correlation,
        }
    }

    async fn pending_id(
        service: &RecoveryAuthorizationService,
        challenge: RecoveryChallenge,
    ) -> String {
        match service.prepare(challenge).await.expect("prepare") {
            PreparedAuthorization::Pending { row, .. } => row.authorization_id,
            other => panic!("expected pending, got {other:?}"),
        }
    }

    async fn approve(
        service: &RecoveryAuthorizationService,
        authorization_id: &str,
    ) -> RecoveryAuthorizationResult {
        service
            .resolve_question(authorization_id, approve_outcome())
            .await
            .expect("approve")
    }

    fn approve_outcome() -> crate::acp::question::QuestionOutcome {
        crate::acp::question::QuestionOutcome {
            answers: vec![crate::acp::question::QuestionAnsweredItem {
                question: "recovery_authorization".into(),
                header: "Recovery".into(),
                multi_select: false,
                selected: vec![RECOVERY_APPROVE_LABEL.into()],
            }],
            declined: false,
        }
    }

    async fn row(
        service: &RecoveryAuthorizationService,
        authorization_id: &str,
    ) -> recovery_authorization::Model {
        service
            .store()
            .find_by_id(authorization_id)
            .await
            .expect("read row")
            .expect("row exists")
    }

    async fn start_request(
        service: &RecoveryAuthorizationService,
        manager: &ConnectionManager,
        parent_connection_id: &str,
        challenge: RecoveryChallenge,
    ) -> (
        tokio::task::JoinHandle<Result<RecoveryAuthorizationResult, RecoveryAuthorizationError>>,
        recovery_authorization::Model,
        String,
    ) {
        let request_service = service.clone();
        let request_manager = manager.clone_ref();
        let request = tokio::spawn(async move {
            request_service
                .request(&request_manager, challenge, CancellationToken::new())
                .await
        });
        let parent_conversation_id = manager
            .get_state(parent_connection_id)
            .await
            .unwrap()
            .read()
            .await
            .conversation_id
            .unwrap();
        let pending = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let rows = service
                    .store()
                    .list_for_parent(parent_conversation_id)
                    .await
                    .unwrap();
                if let Some(row) = rows.into_iter().find(|row| {
                    row.status == RecoveryAuthorizationStatus::Pending && row.question_id.is_some()
                }) {
                    break row;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("recovery card must bind within timeout");
        let answer_question_id = manager
            .get_state(parent_connection_id)
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .as_ref()
            .unwrap()
            .questions[0]
            .id
            .clone();
        (request, pending, answer_question_id)
    }

    #[tokio::test]
    async fn concurrent_requests_reuse_one_pending_or_approved_challenge() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let service = Arc::new(service);
        let gate = Arc::new(Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let gate = gate.clone();
            let service = service.clone();
            let challenge = challenge(parent);
            tasks.push(tokio::spawn(async move {
                gate.wait().await;
                service.prepare(challenge).await.expect("prepare")
            }));
        }
        gate.wait().await;
        let first = tasks.remove(0).await.expect("join");
        let second = tasks.remove(0).await.expect("join");
        let first_row = match first {
            PreparedAuthorization::Pending { row, .. } => row,
            other => panic!("pending expected: {other:?}"),
        };
        let second_row = match second {
            PreparedAuthorization::Pending { row, .. } => row,
            other => panic!("pending expected: {other:?}"),
        };
        assert_eq!(first_row.authorization_id, second_row.authorization_id);
        assert_eq!(service.store().count_active(parent).await.unwrap(), 1);

        let approved = approve(&service, &first_row.authorization_id).await;
        let a = service.prepare(challenge(parent)).await.unwrap();
        let b = service.prepare(challenge(parent)).await.unwrap();
        for prepared in [a, b] {
            let PreparedAuthorization::ExistingApproved(result) = prepared else {
                panic!("approved result expected")
            };
            assert_eq!(result.authorization_id, approved.authorization_id);
            assert_eq!(result.expires_at, approved.expires_at);
        }
    }

    #[tokio::test]
    async fn duplicate_pending_call_waits_for_the_same_durable_resolution_without_a_second_card() {
        let (db, parent, clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection("parent-a", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        manager
            .get_state("parent-a")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));

        let first_service = service.clone();
        let first_manager = manager.clone_ref();
        let first_challenge = challenge(parent);
        let first = tokio::spawn(async move {
            first_service
                .request(&first_manager, first_challenge, CancellationToken::new())
                .await
        });
        let pending = loop {
            let rows = service.store().list_for_parent(parent).await.unwrap();
            if !rows.is_empty() && rows[0].question_id.is_some() {
                break rows;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(pending.len(), 1);
        let authorization_id = pending[0].authorization_id.clone();
        let question_id = pending[0].question_id.clone().expect("creator bound card");
        let answer_question_id = manager
            .get_state("parent-a")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .as_ref()
            .unwrap()
            .questions[0]
            .id
            .clone();

        let second_service = service.clone();
        let second_manager = manager.clone_ref();
        let second_challenge = challenge(parent);
        let second = tokio::spawn(async move {
            second_service
                .request(&second_manager, second_challenge, CancellationToken::new())
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(
            manager.pending_question_count_for_parent("parent-a").await,
            1
        );
        manager
            .answer_question(
                "parent-a",
                &question_id,
                QuestionAnswer {
                    answers: vec![QuestionAnswerItem {
                        question_id: answer_question_id,
                        labels: vec![RECOVERY_APPROVE_LABEL.into()],
                    }],
                    declined: false,
                },
            )
            .await
            .unwrap();
        let one = first.await.unwrap().unwrap();
        let two = second.await.unwrap().unwrap();
        assert_eq!(one, two);
        assert_eq!(one.authorization_id, authorization_id);
        assert_eq!(
            service.store().list_for_parent(parent).await.unwrap().len(),
            1
        );

        drop(service);
        let restored = RecoveryAuthorizationService::with_clock(db.conn.clone(), Arc::new(clock));
        let durable = restored
            .wait_for_resolution(&authorization_id, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(durable, one);
    }

    #[tokio::test]
    async fn approval_expires_exactly_ten_minutes_after_approved_at() {
        let (_db, parent, clock, service) = fixture(start()).await;
        let authorization_id = pending_id(&service, challenge(parent)).await;
        let approved = approve(&service, &authorization_id).await;
        let approved_at = approved.approved_at.expect("approved_at");
        assert_eq!(approved.expires_at, Some(approved_at + APPROVAL_TTL));

        clock.set(approved_at + APPROVAL_TTL - Duration::nanoseconds(1));
        assert_eq!(
            service.get(&authorization_id).await.unwrap().status,
            RecoveryAuthorizationStatus::Approved
        );
        clock.set(approved_at + APPROVAL_TTL);
        assert_eq!(
            service.get(&authorization_id).await.unwrap().status,
            RecoveryAuthorizationStatus::Expired
        );
        clock.set(approved_at + APPROVAL_TTL + Duration::days(1));
        let expired = service.get(&authorization_id).await.unwrap();
        assert_eq!(expired.status, RecoveryAuthorizationStatus::Expired);
        assert!(expired.consumed_at.is_none());
        assert!(expired.consumed_by_kind.is_none());
        assert!(expired.consumed_by_id.is_none());
        assert!(expired.consumer_correlation_id.is_none());
    }

    #[tokio::test]
    async fn decline_dismiss_and_parent_disconnect_end_declined_or_abandoned() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "lifecycle-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("lifecycle-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));

        let (decline, decline_row, answer_question_id) =
            start_request(&service, &manager, "lifecycle-parent", challenge(parent)).await;
        manager
            .answer_question(
                "lifecycle-parent",
                decline_row.question_id.as_deref().unwrap(),
                QuestionAnswer {
                    answers: vec![QuestionAnswerItem {
                        question_id: answer_question_id,
                        labels: vec![RECOVERY_DECLINE_LABEL.into()],
                    }],
                    declined: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            decline.await.unwrap().unwrap().status,
            RecoveryAuthorizationStatus::Declined
        );

        let mut dismiss_challenge = challenge(parent);
        dismiss_challenge.source_state_fingerprint = "lifecycle-dismiss".into();
        let (dismiss, dismiss_row, _) =
            start_request(&service, &manager, "lifecycle-parent", dismiss_challenge).await;
        manager
            .answer_question(
                "lifecycle-parent",
                dismiss_row.question_id.as_deref().unwrap(),
                QuestionAnswer {
                    answers: Vec::new(),
                    declined: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            dismiss.await.unwrap().unwrap().status,
            RecoveryAuthorizationStatus::Declined
        );

        let mut teardown_challenge = challenge(parent);
        teardown_challenge.source_state_fingerprint = "lifecycle-teardown".into();
        let (teardown, teardown_row, _) =
            start_request(&service, &manager, "lifecycle-parent", teardown_challenge).await;
        let waiter_a =
            service.wait_for_resolution(&teardown_row.authorization_id, CancellationToken::new());
        let waiter_b =
            service.wait_for_resolution(&teardown_row.authorization_id, CancellationToken::new());
        manager.cancel_questions_by_parent("lifecycle-parent").await;
        let (request_result, a, b) = tokio::join!(teardown, waiter_a, waiter_b);
        let request_result = request_result.unwrap().unwrap();
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(
            request_result.status,
            RecoveryAuthorizationStatus::Abandoned
        );
        assert_eq!(a, b);
        assert_eq!(a, request_result);

        for authorization_id in [
            decline_row.authorization_id,
            dismiss_row.authorization_id,
            teardown_row.authorization_id,
        ] {
            let terminal = row(&service, &authorization_id).await;
            assert!(terminal.approved_at.is_none());
            assert!(terminal.expires_at.is_none());
            assert!(terminal.consumed_at.is_none());
            assert!(terminal.consumed_by_kind.is_none());
            assert!(terminal.consumed_by_id.is_none());
            assert!(terminal.consumer_correlation_id.is_none());
        }
    }

    #[tokio::test]
    async fn occupied_question_channel_returns_blocked_and_leaves_no_orphan_pending_authorization()
    {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection("parent-b", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        manager
            .get_state("parent-b")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        let existing = manager
            .register_question(
                "parent-b",
                vec![crate::acp::question::generic_test_question()],
            )
            .await
            .expect("occupy channel");

        let error = service
            .request(&manager, challenge(parent), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code(), "recovery_authorization_blocked");
        assert_eq!(
            manager.pending_question_count_for_parent("parent-b").await,
            1
        );
        assert_eq!(
            manager
                .pending_question_parent_connection_id(&existing.question_id)
                .await
                .as_deref(),
            Some("parent-b")
        );
        assert_eq!(service.store().count_active(parent).await.unwrap(), 0);
        let rows = service.store().list_for_parent(parent).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, RecoveryAuthorizationStatus::Abandoned);
    }

    #[tokio::test]
    async fn registration_refusal_cleanup_retries_after_request_is_dropped() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "registration-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("registration-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        let existing = manager
            .register_question(
                "registration-parent",
                vec![crate::acp::question::generic_test_question()],
            )
            .await
            .expect("occupy question channel");
        let pause = service.pause_next_write(InjectedRecoveryWriteFailure::Abandon);
        service.fail_next_write(InjectedRecoveryWriteFailure::Abandon);

        let request_service = service.clone();
        let request_manager = manager.clone_ref();
        let request = tokio::spawn(async move {
            request_service
                .request(
                    &request_manager,
                    challenge(parent),
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(StdDuration::from_secs(1), pause.reached.notified())
            .await
            .expect("registration rollback must reach injected abandonment");
        let pending = service.store().list_for_parent(parent).await.unwrap()[0].clone();
        request.abort();
        let _ = request.await;
        pause.release.notify_one();

        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                if row(&service, &pending.authorization_id).await.status
                    == RecoveryAuthorizationStatus::Abandoned
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned registration rollback must retry to durable abandonment");
        assert_eq!(
            manager
                .pending_question_count_for_parent("registration-parent")
                .await,
            1
        );
        assert_eq!(
            manager
                .pending_question_parent_connection_id(&existing.question_id)
                .await
                .as_deref(),
            Some("registration-parent")
        );
    }

    #[tokio::test]
    async fn bind_rollback_retries_after_request_is_dropped_before_card_release() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "bind-retry-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("bind-retry-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        service.fail_next_write(InjectedRecoveryWriteFailure::Bind);
        service.fail_next_write(InjectedRecoveryWriteFailure::Abandon);
        let pause = service.pause_next_write(InjectedRecoveryWriteFailure::Abandon);

        let request_service = service.clone();
        let request_manager = manager.clone_ref();
        let request = tokio::spawn(async move {
            request_service
                .request(
                    &request_manager,
                    challenge(parent),
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(StdDuration::from_secs(1), pause.reached.notified())
            .await
            .expect("bind rollback must reach injected abandonment");
        let pending = service.store().list_for_parent(parent).await.unwrap()[0].clone();
        assert_eq!(
            manager
                .pending_question_count_for_parent("bind-retry-parent")
                .await,
            1
        );
        request.abort();
        let _ = request.await;
        pause.release.notify_one();

        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                if row(&service, &pending.authorization_id).await.status
                    == RecoveryAuthorizationStatus::Abandoned
                    && manager
                        .pending_question_count_for_parent("bind-retry-parent")
                        .await
                        == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned bind rollback must retry before clearing the card");
    }

    #[tokio::test]
    async fn request_token_cancellation_cleanup_survives_request_drop() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "cancel-retry-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("cancel-retry-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        let cancelled = CancellationToken::new();
        let request_service = service.clone();
        let request_manager = manager.clone_ref();
        let request_cancelled = cancelled.clone();
        let request = tokio::spawn(async move {
            request_service
                .request(&request_manager, challenge(parent), request_cancelled)
                .await
        });
        let pending = loop {
            let rows = service.store().list_for_parent(parent).await.unwrap();
            if rows
                .first()
                .and_then(|row| row.question_id.as_ref())
                .is_some()
            {
                break rows[0].clone();
            }
            tokio::task::yield_now().await;
        };
        let pause = service.pause_next_write(InjectedRecoveryWriteFailure::Abandon);
        service.fail_next_write(InjectedRecoveryWriteFailure::Abandon);
        cancelled.cancel();
        tokio::time::timeout(StdDuration::from_secs(1), pause.reached.notified())
            .await
            .expect("request cancellation must reach injected abandonment");
        assert_eq!(
            manager
                .pending_question_count_for_parent("cancel-retry-parent")
                .await,
            1
        );
        request.abort();
        let _ = request.await;
        pause.release.notify_one();

        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                if row(&service, &pending.authorization_id).await.status
                    == RecoveryAuthorizationStatus::Abandoned
                    && manager
                        .pending_question_count_for_parent("cancel-retry-parent")
                        .await
                        == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned cancellation cleanup must retry before clearing the card");
    }

    #[tokio::test]
    async fn parent_teardown_not_found_completes_card_cleanup() {
        let (db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "not-found-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("not-found-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        let (request, _pending, _answer_question_id) =
            start_request(&service, &manager, "not-found-parent", challenge(parent)).await;
        crate::db::entities::conversation::Entity::delete_by_id(parent)
            .exec(&db.conn)
            .await
            .expect("delete parent conversation");
        manager.cancel_questions_by_parent("not-found-parent").await;

        tokio::time::timeout(StdDuration::from_secs(1), async {
            loop {
                if manager
                    .pending_question_count_for_parent("not-found-parent")
                    .await
                    == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("NotFound teardown is completed cleanup, not retryable");
        let _ = request.await;
    }

    #[tokio::test]
    async fn bind_failure_closes_card_and_abandons_created_row() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "bind-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("bind-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        service.fail_next_write(InjectedRecoveryWriteFailure::Bind);

        let error = service
            .request(&manager, challenge(parent), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, RecoveryAuthorizationError::Database(_)));
        assert_eq!(
            manager
                .pending_question_count_for_parent("bind-parent")
                .await,
            0
        );
        assert_eq!(service.store().count_active(parent).await.unwrap(), 0);
        let rows = service.store().list_for_parent(parent).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, RecoveryAuthorizationStatus::Abandoned);
    }

    #[tokio::test]
    async fn settlement_failures_keep_the_recovery_card_until_durable_retry() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "settle-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("settle-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));

        let request_service = service.clone();
        let request_manager = manager.clone_ref();
        let request = tokio::spawn(async move {
            request_service
                .request(
                    &request_manager,
                    challenge(parent),
                    CancellationToken::new(),
                )
                .await
        });
        let pending = loop {
            let rows = service.store().list_for_parent(parent).await.unwrap();
            if rows
                .first()
                .and_then(|row| row.question_id.as_ref())
                .is_some()
            {
                break rows[0].clone();
            }
            tokio::task::yield_now().await;
        };
        let question_id = pending.question_id.clone().unwrap();
        let answer_question_id = manager
            .get_state("settle-parent")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .as_ref()
            .unwrap()
            .questions[0]
            .id
            .clone();
        let answer = QuestionAnswer {
            answers: vec![QuestionAnswerItem {
                question_id: answer_question_id,
                labels: vec![RECOVERY_APPROVE_LABEL.into()],
            }],
            declined: false,
        };

        service.fail_next_write(InjectedRecoveryWriteFailure::Resolve);
        assert!(manager
            .answer_question("settle-parent", &question_id, answer.clone())
            .await
            .is_err());
        assert_eq!(
            manager
                .pending_question_count_for_parent("settle-parent")
                .await,
            1
        );
        assert_eq!(
            row(&service, &pending.authorization_id).await.status,
            RecoveryAuthorizationStatus::Pending
        );

        manager
            .answer_question("settle-parent", &question_id, answer)
            .await
            .unwrap();
        assert_eq!(
            request.await.unwrap().unwrap().status,
            RecoveryAuthorizationStatus::Approved
        );

        let dismiss_service = service.clone();
        let dismiss_manager = manager.clone_ref();
        let mut dismiss_challenge = challenge(parent);
        dismiss_challenge.source_state_fingerprint = "dismiss-fingerprint".into();
        let dismiss = tokio::spawn(async move {
            dismiss_service
                .request(
                    &dismiss_manager,
                    dismiss_challenge,
                    CancellationToken::new(),
                )
                .await
        });
        let dismiss_row = loop {
            let rows = service.store().list_for_parent(parent).await.unwrap();
            if let Some(row) = rows.iter().find(|row| {
                row.status == RecoveryAuthorizationStatus::Pending && row.question_id.is_some()
            }) {
                break row.clone();
            }
            tokio::task::yield_now().await;
        };
        let dismiss_question_id = dismiss_row.question_id.clone().unwrap();
        service.fail_next_write(InjectedRecoveryWriteFailure::Resolve);
        assert!(manager
            .answer_question(
                "settle-parent",
                &dismiss_question_id,
                QuestionAnswer {
                    answers: Vec::new(),
                    declined: true,
                },
            )
            .await
            .is_err());
        assert_eq!(
            manager
                .pending_question_count_for_parent("settle-parent")
                .await,
            1
        );
        assert_eq!(
            row(&service, &dismiss_row.authorization_id).await.status,
            RecoveryAuthorizationStatus::Pending
        );
        manager
            .answer_question(
                "settle-parent",
                &dismiss_question_id,
                QuestionAnswer {
                    answers: Vec::new(),
                    declined: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            dismiss.await.unwrap().unwrap().status,
            RecoveryAuthorizationStatus::Declined
        );

        let teardown_service = service.clone();
        let teardown_manager = manager.clone_ref();
        let mut teardown_challenge = challenge(parent);
        teardown_challenge.source_state_fingerprint = "teardown-fingerprint".into();
        let teardown = tokio::spawn(async move {
            teardown_service
                .request(
                    &teardown_manager,
                    teardown_challenge,
                    CancellationToken::new(),
                )
                .await
        });
        let teardown_row = loop {
            let rows = service.store().list_for_parent(parent).await.unwrap();
            if let Some(row) = rows.iter().find(|row| {
                row.status == RecoveryAuthorizationStatus::Pending && row.question_id.is_some()
            }) {
                break row.clone();
            }
            tokio::task::yield_now().await;
        };
        service.fail_next_write(InjectedRecoveryWriteFailure::Abandon);
        manager.cancel_questions_by_parent("settle-parent").await;
        let teardown_result = tokio::time::timeout(std::time::Duration::from_secs(5), teardown)
            .await
            .expect("teardown retry must resolve")
            .unwrap()
            .unwrap();
        assert_eq!(
            teardown_result.status,
            RecoveryAuthorizationStatus::Abandoned
        );
        assert_eq!(
            row(&service, &teardown_row.authorization_id).await.status,
            RecoveryAuthorizationStatus::Abandoned
        );
    }

    #[tokio::test]
    async fn parent_teardown_waits_for_failed_inflight_answer_before_abandoning() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "race-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("race-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        let (request, pending, answer_question_id) =
            start_request(&service, &manager, "race-parent", challenge(parent)).await;
        let question_id = pending.question_id.clone().unwrap();
        let pause = service.pause_next_write(InjectedRecoveryWriteFailure::Resolve);
        service.fail_next_write(InjectedRecoveryWriteFailure::Resolve);

        let answer_manager = manager.clone_ref();
        let answer = tokio::spawn(async move {
            answer_manager
                .answer_question(
                    "race-parent",
                    &question_id,
                    QuestionAnswer {
                        answers: vec![QuestionAnswerItem {
                            question_id: answer_question_id,
                            labels: vec![RECOVERY_APPROVE_LABEL.into()],
                        }],
                        declined: false,
                    },
                )
                .await
        });
        pause.reached.notified().await;
        let teardown_manager = manager.clone_ref();
        let teardown = tokio::spawn(async move {
            teardown_manager
                .cancel_questions_by_parent("race-parent")
                .await;
        });
        tokio::task::yield_now().await;
        pause.release.notify_one();

        assert!(answer.await.unwrap().is_err());
        teardown.await.unwrap();
        let request_result = tokio::time::timeout(std::time::Duration::from_secs(5), request)
            .await
            .expect("racing teardown must resolve")
            .unwrap()
            .unwrap();
        assert_eq!(
            row(&service, &pending.authorization_id).await.status,
            RecoveryAuthorizationStatus::Abandoned
        );
        assert_eq!(
            request_result.status,
            RecoveryAuthorizationStatus::Abandoned
        );
    }

    #[tokio::test]
    async fn aborting_answer_caller_does_not_strand_settlement_claim() {
        let (_db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection(
                "abort-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("abort-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        let (request, pending, answer_question_id) =
            start_request(&service, &manager, "abort-parent", challenge(parent)).await;
        let question_id = pending.question_id.clone().unwrap();
        let pause = service.pause_next_write(InjectedRecoveryWriteFailure::Resolve);
        let answer_manager = manager.clone_ref();
        let answer = tokio::spawn(async move {
            answer_manager
                .answer_question(
                    "abort-parent",
                    &question_id,
                    QuestionAnswer {
                        answers: vec![QuestionAnswerItem {
                            question_id: answer_question_id,
                            labels: vec![RECOVERY_APPROVE_LABEL.into()],
                        }],
                        declined: false,
                    },
                )
                .await
        });
        pause.reached.notified().await;
        answer.abort();
        let _ = answer.await;
        pause.release.notify_one();

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), request)
            .await
            .expect("owned settlement must outlive aborted caller")
            .unwrap()
            .unwrap();
        assert_eq!(result.status, RecoveryAuthorizationStatus::Approved);
        assert_eq!(
            manager
                .pending_question_count_for_parent("abort-parent")
                .await,
            0
        );
        assert_eq!(
            row(&service, &pending.authorization_id).await.status,
            RecoveryAuthorizationStatus::Approved
        );
    }

    #[tokio::test]
    async fn approved_receipt_survives_connection_rebind_for_same_parent_conversation() {
        let (db, parent, _clock, service) = fixture(start()).await;
        let manager = ConnectionManager::new();
        manager.install_recovery_authorization_service(Arc::new(service.clone()));
        manager
            .insert_test_connection(
                "old-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("old-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);
        let (request, pending, answer_question_id) =
            start_request(&service, &manager, "old-parent", challenge(parent)).await;
        manager
            .answer_question(
                "old-parent",
                pending.question_id.as_deref().unwrap(),
                QuestionAnswer {
                    answers: vec![QuestionAnswerItem {
                        question_id: answer_question_id,
                        labels: vec![RECOVERY_APPROVE_LABEL.into()],
                    }],
                    declined: false,
                },
            )
            .await
            .unwrap();
        let approved = request.await.unwrap().unwrap();
        let id = approved.authorization_id;
        manager.disconnect("old-parent").await.unwrap();
        assert!(manager.get_state("old-parent").await.is_none());
        manager
            .insert_test_connection(
                "new-parent",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        manager
            .get_state("new-parent")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent);

        let source = challenge(parent);
        let expected = expectation(&source, "rebind-correlation");
        let txn = db.conn.begin().await.unwrap();
        let approved = validate_for_consumption_txn(&txn, &id, &expected, start())
            .await
            .unwrap();
        consume_txn(&txn, approved, &expected, start())
            .await
            .unwrap();
        txn.rollback().await.unwrap();

        let mut wrong = expectation(&source, "wrong-parent");
        wrong.parent_conversation_id = parent + 1;
        let txn = db.conn.begin().await.unwrap();
        let err = validate_for_consumption_txn(&txn, &id, &wrong, start())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "recovery_authorization_parent_mismatch");
        txn.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn cross_parent_subject_fingerprint_action_payload_and_reason_mismatches_fail() {
        let (db, parent, _clock, service) = fixture(start()).await;
        let base = challenge(parent);
        let id = pending_id(&service, base.clone()).await;
        approve(&service, &id).await;

        let mut cases: Vec<(AuthorizationConsumeExpectation<'_>, &str)> = Vec::new();
        let other_payload =
            json!({"agent":"other","nested":{"a":1,"z":2},"reset_reason_hash":"reason-a"});
        let other_reason =
            json!({"agent":"codex","nested":{"a":1,"z":2},"reset_reason_hash":"other"});
        let other_subject = "other-subject";
        let other_fingerprint = "other-fingerprint";
        let mut e = expectation(&base, "mismatch");
        e.parent_conversation_id += 1;
        cases.push((e, "recovery_authorization_parent_mismatch"));
        let mut e = expectation(&base, "mismatch");
        e.subject_kind = RecoverySubjectKind::Workflow;
        cases.push((e, "recovery_authorization_subject_kind_mismatch"));
        let mut e = expectation(&base, "mismatch");
        e.subject_id = other_subject;
        cases.push((e, "recovery_authorization_subject_id_mismatch"));
        let mut e = expectation(&base, "mismatch");
        e.source_state_fingerprint = other_fingerprint;
        cases.push((e, "recovery_authorization_fingerprint_mismatch"));
        let mut e = expectation(&base, "mismatch");
        e.allowed_action = RecoveryAllowedAction::FreshDispatch;
        cases.push((e, "recovery_authorization_action_mismatch"));
        let mut e = expectation(&base, "mismatch");
        e.action_payload = &other_payload;
        cases.push((e, "recovery_authorization_payload_mismatch"));
        let mut e = expectation(&base, "mismatch");
        e.action_payload = &other_reason;
        cases.push((e, "recovery_authorization_payload_mismatch"));

        for (expected, code) in cases {
            let txn = db.conn.begin().await.unwrap();
            let err = validate_for_consumption_txn(&txn, &id, &expected, start())
                .await
                .unwrap_err();
            assert_eq!(err.code(), code);
            txn.rollback().await.unwrap();
            let unchanged = row(&service, &id).await;
            assert_eq!(unchanged.status, RecoveryAuthorizationStatus::Approved);
            assert!(unchanged.consumed_at.is_none());
            assert!(unchanged.consumer_correlation_id.is_none());
        }
    }

    #[tokio::test]
    async fn concurrent_consumers_have_exactly_one_winner_and_rollback_restores_approved() {
        let temp = tempfile::tempdir().unwrap();
        let db = crate::db::init_database(temp.path(), "recovery-consume-race")
            .await
            .unwrap();
        let folder = seed_folder(&db, "/tmp/recovery-consume").await;
        let parent = conversation_service::create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .unwrap()
            .id;
        let service = RecoveryAuthorizationService::with_clock(
            db.conn.clone(),
            Arc::new(TestClock::at(start())),
        );
        let source = challenge(parent);
        let id = pending_id(&service, source.clone()).await;
        approve(&service, &id).await;

        let barrier = Arc::new(Barrier::new(3));
        let mut consumers = Vec::new();
        for correlation in ["winner-a", "winner-b"] {
            let conn = db.conn.clone();
            let barrier = barrier.clone();
            let id = id.clone();
            let source = source.clone();
            consumers.push(tokio::spawn(async move {
                let expected = expectation(&source, correlation);
                let txn = conn.begin().await.unwrap();
                let approved = validate_for_consumption_txn(&txn, &id, &expected, start())
                    .await
                    .unwrap();
                barrier.wait().await;
                match consume_txn(&txn, approved, &expected, start()).await {
                    Ok(()) => {
                        txn.commit().await.unwrap();
                        true
                    }
                    Err(RecoveryAuthorizationError::ConsumedConflict) => {
                        txn.rollback().await.unwrap();
                        false
                    }
                    Err(RecoveryAuthorizationError::Database(_)) => {
                        txn.rollback().await.unwrap();
                        loop {
                            let retry = conn.begin().await.unwrap();
                            match validate_for_consumption_txn(&retry, &id, &expected, start())
                                .await
                            {
                                Err(RecoveryAuthorizationError::ConsumedConflict) => {
                                    retry.rollback().await.unwrap();
                                    break false;
                                }
                                Ok(_) | Err(RecoveryAuthorizationError::Database(_)) => {
                                    retry.rollback().await.unwrap();
                                    tokio::task::yield_now().await;
                                }
                                Err(error) => panic!("unexpected retry error: {error}"),
                            }
                        }
                    }
                    Err(error) => panic!("unexpected consume error: {error}"),
                }
            }));
        }
        barrier.wait().await;
        let mut winners = 0;
        for consumer in consumers {
            winners += usize::from(consumer.await.unwrap());
        }
        assert_eq!(winners, 1, "exactly one consume CAS must commit");
        let consumed = row(&service, &id).await;
        assert_eq!(consumed.status, RecoveryAuthorizationStatus::Consumed);
        assert!(matches!(
            consumed.consumer_correlation_id.as_deref(),
            Some("winner-a" | "winner-b")
        ));

        let mut second_challenge = source.clone();
        second_challenge.subject_id = "rollback-subject".into();
        second_challenge.source_state_fingerprint = "rollback-fingerprint".into();
        let rollback_id = pending_id(&service, second_challenge.clone()).await;
        approve(&service, &rollback_id).await;
        let expected = expectation(&second_challenge, "rolled-back");
        let txn = db.conn.begin().await.unwrap();
        let approved = validate_for_consumption_txn(&txn, &rollback_id, &expected, start())
            .await
            .unwrap();
        consume_txn(&txn, approved, &expected, start())
            .await
            .unwrap();
        txn.rollback().await.unwrap();
        assert_eq!(
            row(&service, &rollback_id).await.status,
            RecoveryAuthorizationStatus::Approved
        );
        let txn = db.conn.begin().await.unwrap();
        let approved = validate_for_consumption_txn(&txn, &rollback_id, &expected, start())
            .await
            .unwrap();
        consume_txn(&txn, approved, &expected, start())
            .await
            .unwrap();
        txn.commit().await.unwrap();
    }

    #[tokio::test]
    async fn exact_consumed_correlation_replays_but_different_correlation_conflicts() {
        let (db, parent, _clock, service) = fixture(start()).await;
        let source = challenge(parent);
        let id = pending_id(&service, source.clone()).await;
        approve(&service, &id).await;
        let exact = expectation(&source, "correlation-a");
        let txn = db.conn.begin().await.unwrap();
        let approved = validate_for_consumption_txn(&txn, &id, &exact, start())
            .await
            .unwrap();
        consume_txn(&txn, approved, &exact, start()).await.unwrap();
        txn.commit().await.unwrap();
        let original = row(&service, &id).await;

        let txn = db.conn.begin().await.unwrap();
        let replay = validate_for_consumption_txn(&txn, &id, &exact, start())
            .await
            .unwrap();
        consume_txn(&txn, replay, &exact, start()).await.unwrap();
        txn.commit().await.unwrap();

        for mutation in 0..9 {
            let mut changed_source = source.clone();
            let correlation = if mutation == 8 {
                "correlation-b"
            } else {
                "correlation-a"
            };
            match mutation {
                1 => changed_source.subject_kind = RecoverySubjectKind::Workflow,
                2 => changed_source.subject_id = "other-subject".into(),
                3 => changed_source.source_state_fingerprint = "other-fingerprint".into(),
                4 => changed_source.allowed_action = RecoveryAllowedAction::Continue,
                5 => changed_source.action_payload["agent"] = json!("other"),
                _ => {}
            }
            let mut different = expectation(&changed_source, correlation);
            match mutation {
                0 => different.parent_conversation_id += 1,
                6 => different.consumer_kind = RecoveryConsumerKind::WorkflowManifestRevision,
                7 => different.consumer_id = "other-run",
                _ => {}
            }
            let txn = db.conn.begin().await.unwrap();
            let err = validate_for_consumption_txn(&txn, &id, &different, start())
                .await
                .unwrap_err();
            assert_eq!(err.code(), "recovery_authorization_consumed_conflict");
            txn.rollback().await.unwrap();
            let preserved = row(&service, &id).await;
            assert_eq!(preserved.consumed_at, original.consumed_at);
            assert_eq!(preserved.consumed_by_kind, original.consumed_by_kind);
            assert_eq!(preserved.consumed_by_id, original.consumed_by_id);
            assert_eq!(
                preserved.consumer_correlation_id,
                original.consumer_correlation_id
            );
        }
    }

    #[tokio::test]
    async fn terminal_retention_never_prunes_pending_or_approved_authorizations() {
        let (_db, parent, clock, service) = fixture(start()).await;
        let old = start() - Duration::days(31);
        let boundary = start() - Duration::days(TERMINAL_AUTHORIZATION_RETENTION_DAYS);
        for (index, status, requested_at) in [
            (0, RecoveryAuthorizationStatus::Pending, old),
            (1, RecoveryAuthorizationStatus::Approved, old),
            (2, RecoveryAuthorizationStatus::Declined, old),
            (3, RecoveryAuthorizationStatus::Consumed, old),
            (4, RecoveryAuthorizationStatus::Expired, old),
            (5, RecoveryAuthorizationStatus::Abandoned, old),
            (6, RecoveryAuthorizationStatus::Declined, boundary),
        ] {
            let approved_at =
                matches!(&status, RecoveryAuthorizationStatus::Approved).then_some(old);
            let expires_at = matches!(
                &status,
                RecoveryAuthorizationStatus::Approved | RecoveryAuthorizationStatus::Expired
            )
            .then_some(requested_at);
            let consumed_at =
                matches!(&status, RecoveryAuthorizationStatus::Consumed).then_some(requested_at);
            recovery_authorization::ActiveModel {
                authorization_id: Set(format!("retention-row-{index}")),
                parent_conversation_id: Set(parent),
                subject_kind: Set(RecoverySubjectKind::Workflow.as_str().into()),
                subject_id: Set(format!("retention-subject-{index}")),
                source_task_id: Set(None),
                child_conversation_id: Set(None),
                lineage_root_task_id: Set(None),
                work_unit_key: Set(None),
                source_state_fingerprint: Set(format!("retention-fingerprint-{index}")),
                allowed_action: Set(RecoveryAllowedAction::RecoverWorkflow.as_str().into()),
                action_payload_json: Set("{}".into()),
                cause_code: Set("retention".into()),
                risk_class: Set("test".into()),
                display_reason: Set(None),
                status: Set(status),
                question_id: Set(None),
                requested_at: Set(requested_at),
                approved_at: Set(approved_at),
                expires_at: Set(expires_at),
                consumed_at: Set(consumed_at),
                consumed_by_kind: Set(None),
                consumed_by_id: Set(None),
                consumer_correlation_id: Set(None),
            }
            .insert(service.store().connection())
            .await
            .unwrap();
        }
        clock.set(start());
        assert_eq!(service.prune_now().await.unwrap(), 4);
        let survivors = recovery_authorization::Entity::find()
            .filter(recovery_authorization::Column::ParentConversationId.eq(parent))
            .all(service.store().connection())
            .await
            .unwrap();
        let ids: Vec<_> = survivors
            .into_iter()
            .map(|row| row.authorization_id)
            .collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"retention-row-0".into()));
        assert!(ids.contains(&"retention-row-1".into()));
        assert!(ids.contains(&"retention-row-6".into()));
    }
}
