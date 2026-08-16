mod tests {
    use super::*;

    use crate::acp::{
        error::AcpError,
        manager::{ConnectionManager, SharedControlAdapter, SharedControlAdmissionError},
        plan_approval::{PlanApprovalAnswer, PlanApprovalDecision},
        question::QuestionAnswer,
    };
    use sea_orm::Database;
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn request(
        key: SharedSessionKey,
        connection_id: &str,
        client: &str,
        request_id: &str,
    ) -> SharedReserveRequest {
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
                broker
                    .reserve_or_attach(request(
                        SharedSessionKey::Conversation(42),
                        &format!("candidate-{n}"),
                        &format!("client-{n}"),
                        &format!("request-{n}"),
                    ))
                    .await
                    .unwrap()
            }));
        }
        let outcomes = futures::future::join_all(joins).await;
        let ids: std::collections::HashSet<_> = outcomes
            .into_iter()
            .map(|result| result.unwrap().attachment.connection_id)
            .collect();
        assert_eq!(ids.len(), 1);
        let metrics = broker.metrics().snapshot();
        assert_eq!(metrics.created_total, 1);
        assert_eq!(metrics.attached_total, 99);
        assert_eq!(metrics.live_sessions, 1);
        assert_eq!(metrics.active_leases, 100);
    }

    #[tokio::test]
    async fn immutable_launch_conflict_does_not_mutate_live_record() {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(7),
                "conn-a",
                "client-a",
                "req-a",
            ))
            .await
            .unwrap();
        let mut conflicting = request(
            SharedSessionKey::Conversation(7),
            "conn-b",
            "client-b",
            "req-b",
        );
        conflicting.launch_identity.working_dir_fingerprint = "different".into();
        assert!(matches!(
            broker.reserve_or_attach(conflicting).await,
            Err(SharedSessionError::ConfigConflict {
                conflict_kind: SharedConfigConflictKind::WorkingDirectory,
                ..
            })
        ));
        assert_eq!(
            broker
                .diagnostic_for_connection(&first.attachment.connection_id)
                .await
                .unwrap()
                .generation,
            1
        );
    }

    #[tokio::test]
    async fn failed_retry_requires_cleanup_and_increments_generation() {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(8),
                "conn-a",
                "client-a",
                "req-a",
            ))
            .await
            .unwrap();
        broker
            .mark_failed(
                &first.attachment.connection_id,
                1,
                "companion_initialization_failed",
                false,
            )
            .await
            .unwrap();
        let mut retry = request(
            SharedSessionKey::Conversation(8),
            "conn-b",
            "client-a",
            "req-b",
        );
        retry.retry_failed_generation = Some(1);
        assert!(matches!(
            broker.reserve_or_attach(retry.clone()).await,
            Err(SharedSessionError::CleanupInProgress)
        ));
        let (cleanup_handles, cleanup_events) = broker
            .mark_cleanup_complete(&first.attachment.connection_id, 1)
            .await
            .unwrap();
        assert!(cleanup_handles.is_none());
        assert_eq!(cleanup_events.len(), 1);
        let replacement = broker.reserve_or_attach(retry).await.unwrap();
        assert_eq!(replacement.attachment.generation, 2);
        assert_eq!(replacement.attachment.connection_id, "conn-b");
        let metrics = broker.metrics().snapshot();
        assert_eq!(metrics.created_total, 2);
        assert_eq!(metrics.attached_total, 0);
        assert_eq!(metrics.live_sessions, 1);
        assert_eq!(metrics.active_leases, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn bootstrap_metrics_record_ready_and_secret_safe_failure_once() {
        let ready_broker = SharedSessionBroker::default();
        let ready = ready_broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(80),
                "bootstrap-ready",
                "ready-client",
                "ready-request",
            ))
            .await
            .unwrap()
            .attachment;
        tokio::time::advance(Duration::from_millis(25)).await;
        let ready_state = Arc::new(RwLock::new(SessionState::new(
            ready.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(80),
        )));
        ready_broker
            .install_registered(
                &ready.connection_id,
                ready.generation,
                "ready-driver".into(),
                ready_state,
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        ready_broker
            .mark_ready(&ready.connection_id, ready.generation, "ready-driver")
            .await
            .unwrap();
        let ready_metrics = ready_broker.metrics().snapshot();
        assert_eq!(ready_metrics.bootstrap_ready_total, 1);
        assert_eq!(ready_metrics.bootstrap_failed_total, BTreeMap::new());
        assert_eq!(ready_metrics.bootstrap_duration_ms_total, 25);
        assert_eq!(ready_metrics.bootstrap_duration_samples, 1);

        let failed_broker = SharedSessionBroker::default();
        let mut failed_request = request(
            SharedSessionKey::Conversation(81),
            "bootstrap-failed",
            "failed-client",
            "failed-request",
        );
        failed_request.launch_identity.agent_type =
            crate::models::agent::AgentType::custom("private-custom-agent").unwrap();
        failed_request.launch_identity.route_capability =
            SharedRouteCapability::RequiredCompanion;
        failed_request.launch_identity.working_dir_fingerprint = "private-working-dir".into();
        failed_request.launch_identity.route_fingerprint = "private-route-fingerprint".into();
        failed_request.launch_identity.terminal_shell_fingerprint = "private-shell".into();
        let failed = failed_broker
            .reserve_or_attach(failed_request)
            .await
            .unwrap()
            .attachment;
        tokio::time::advance(Duration::from_millis(37)).await;
        failed_broker
            .mark_failed(
                &failed.connection_id,
                failed.generation,
                "companion_initialization_failed",
                true,
            )
            .await
            .unwrap();
        // A repeated terminal settlement must not create another outcome or
        // duration sample.
        failed_broker
            .mark_failed(
                &failed.connection_id,
                failed.generation,
                "companion_initialization_failed",
                true,
            )
            .await
            .unwrap();

        let failed_metrics = failed_broker.metrics().snapshot();
        assert_eq!(failed_metrics.bootstrap_ready_total, 0);
        assert_eq!(
            failed_metrics.bootstrap_failed_total,
            BTreeMap::from([(
                "custom|required_companion|companion_initialization_failed".to_string(),
                1,
            )])
        );
        assert_eq!(failed_metrics.bootstrap_duration_ms_total, 37);
        assert_eq!(failed_metrics.bootstrap_duration_samples, 1);
        let encoded = serde_json::to_string(&failed_metrics).unwrap();
        for secret in [
            "private-custom-agent",
            "private-working-dir",
            "private-route-fingerprint",
            "private-shell",
        ] {
            assert!(!encoded.contains(secret));
        }
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
        }))
        .await;
        let ids: std::collections::HashSet<_> = outcomes
            .iter()
            .map(|outcome| outcome.attachment.connection_id.as_str())
            .collect();
        assert_eq!(ids.len(), 1);
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.attachment.generation == 2));
    }

    #[tokio::test]
    async fn diagnostics_never_expose_lease_or_client_identity() {
        let broker = SharedSessionBroker::default();
        let result = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(9),
                "conn",
                "private-client",
                "req",
            ))
            .await
            .unwrap();
        let value = serde_json::to_value(
            broker
                .diagnostic_for_connection(&result.attachment.connection_id)
                .await
                .unwrap(),
        )
        .unwrap();
        let encoded = value.to_string();
        assert!(!encoded.contains(&result.attachment.lease_id));
        assert!(!encoded.contains("private-client"));
    }

    #[tokio::test]
    async fn detached_generation_cannot_accept_failure_mutation() {
        let broker = failed_cleanup_complete_fixture(19).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let old_guard = old_record.lock().await;
        let mut stale_mutation = Box::pin(broker.mark_failed(
            "failed-connection",
            1,
            "companion_initialization_failed",
            false,
        ));
        assert!(matches!(
            futures::poll!(stale_mutation.as_mut()),
            std::task::Poll::Pending
        ));

        install_replacement_pointer(&broker, 19).await;
        drop(old_guard);

        assert!(matches!(
            stale_mutation.await,
            Err(SharedSessionError::SessionUnavailable) | Err(SharedSessionError::GenerationStale)
        ));
    }

    #[tokio::test]
    async fn detached_generation_cannot_complete_cleanup() {
        let broker = failed_cleanup_complete_fixture(20).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let old_guard = old_record.lock().await;
        let mut stale_cleanup = Box::pin(broker.mark_cleanup_complete("failed-connection", 1));
        assert!(matches!(
            futures::poll!(stale_cleanup.as_mut()),
            std::task::Poll::Pending
        ));

        install_replacement_pointer(&broker, 20).await;
        drop(old_guard);

        assert!(matches!(
            stale_cleanup.await,
            Err(SharedSessionError::SessionUnavailable) | Err(SharedSessionError::GenerationStale)
        ));
    }

    #[tokio::test]
    async fn diagnostics_do_not_return_a_detached_generation() {
        let broker = failed_cleanup_complete_fixture(21).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let old_guard = old_record.lock().await;
        let mut stale_diagnostic = Box::pin(broker.diagnostic_for_connection("failed-connection"));
        assert!(matches!(
            futures::poll!(stale_diagnostic.as_mut()),
            std::task::Poll::Pending
        ));

        install_replacement_pointer(&broker, 21).await;
        drop(old_guard);

        assert!(stale_diagnostic.await.is_none());
    }

    #[tokio::test]
    async fn failed_phase_rejects_unrecognized_diagnostic_text() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(22),
                "conn-a",
                "client-a",
                "request-a",
            ))
            .await
            .unwrap();
        for private in [
            "/Users/private/project/token.txt",
            "super-secret-token-value",
        ] {
            assert!(matches!(
                broker
                    .mark_failed(&reservation.attachment.connection_id, 1, private, false)
                    .await,
                Err(SharedSessionError::InvalidField {
                    field: "error_code"
                })
            ));
            let diagnostic = broker
                .diagnostic_for_connection(&reservation.attachment.connection_id)
                .await
                .unwrap();
            let serialized = serde_json::to_string(&diagnostic).unwrap();
            let debug = format!("{diagnostic:?}");
            assert!(!serialized.contains(private));
            assert!(!debug.contains(private));
        }
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
                Err(SharedSessionError::InvalidField {
                    field: "client_instance_id"
                })
            ));
        }
    }

    #[tokio::test]
    async fn capacity_limits_reject_only_new_identities() {
        assert_eq!(MAX_ACTIVE_LEASES, 256);
        assert_eq!(MAX_CONNECT_LEDGER_ENTRIES, 4_096);
        assert_eq!(MAX_WAITING_PROMPTS, 64);
        assert_eq!(MAX_WAITING_BYTES, 32 * 1024 * 1024);
        assert_eq!(MAX_PROMPT_LEDGER_ENTRIES, 65_536);
        assert_eq!(MAX_EXPIRED_LEASE_TOMBSTONES, 1_024);
        assert_eq!(MAX_REPLACED_CONNECTION_TOMBSTONES, 4_096);

        let fixture = broker_at_identity_limits().await;
        assert!(fixture.retry_existing_connect().await.is_ok());
        assert!(matches!(
            fixture.connect_new_identity().await,
            Err(SharedSessionError::ConnectLedgerCapacityExceeded)
        ));
        assert!(matches!(
            fixture.attach_new_client().await,
            Err(SharedSessionError::ClientLeaseCapacityExceeded)
        ));
        let metrics = fixture.broker.metrics().snapshot();
        assert_eq!(metrics.live_sessions, 1);
        assert_eq!(metrics.active_leases, 3);
        assert_eq!(metrics.capacity_rejected_total, 2);
    }

    #[tokio::test]
    async fn identical_connect_retry_returns_original_attachment() {
        let broker = SharedSessionBroker::default();
        let original_request = request(
            SharedSessionKey::Conversation(11),
            "conn-a",
            "client-a",
            "req-a",
        );
        let first = broker
            .reserve_or_attach(original_request.clone())
            .await
            .unwrap();
        let retry = broker.reserve_or_attach(original_request).await.unwrap();

        assert_eq!(
            retry.attachment.connection_id,
            first.attachment.connection_id
        );
        assert_eq!(retry.attachment.lease_id, first.attachment.lease_id);
        assert_eq!(
            retry.attachment.lease_expires_at,
            first.attachment.lease_expires_at
        );
        assert_eq!(retry.attachment.disposition, SharedDisposition::Created);
        assert!(!retry.created);
    }

    #[tokio::test]
    async fn same_client_instance_renews_across_device_ids() {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(12),
                "conn-a",
                "client-a",
                "req-a",
            ))
            .await
            .unwrap();
        let mut second_request = request(
            SharedSessionKey::Conversation(12),
            "conn-b",
            "client-a",
            "req-b",
        );
        second_request.device_id = "device-b".into();

        let second = broker.reserve_or_attach(second_request).await.unwrap();

        assert_eq!(second.attachment.lease_id, first.attachment.lease_id);
        assert_eq!(second.attachment.disposition, SharedDisposition::Attached);
        assert!(!second.created);
        assert_eq!(broker.metrics().snapshot().active_leases, 1);
    }

    #[tokio::test]
    async fn validated_attach_retains_public_state_across_replacement_boundary() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(23),
                "old-connection",
                "client-a",
                "request-a",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(23),
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-old".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        let (binding, retained_state) = broker
            .validate_and_bind_lease_with_state(
                &reservation.attachment.connection_id,
                Some(reservation.attachment.generation),
                Some(&reservation.attachment.lease_id),
            )
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&retained_state, &state));

        broker
            .mark_failed(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "companion_initialization_failed",
                false,
            )
            .await
            .unwrap();
        broker
            .mark_cleanup_complete(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
            )
            .await
            .unwrap();
        let mut replacement_request = request(
            SharedSessionKey::Conversation(23),
            "new-connection",
            "client-b",
            "request-b",
        );
        replacement_request.retry_failed_generation = Some(reservation.attachment.generation);
        broker.reserve_or_attach(replacement_request).await.unwrap();

        assert!(matches!(
            broker
                .validate_and_bind_lease(
                    &binding.connection_id,
                    Some(binding.generation),
                    Some(&binding.lease_id),
                )
                .await,
            Err(crate::web::ws_attach::DetachReason::SessionReplaced)
        ));
        assert!(Arc::ptr_eq(&retained_state, &state));
    }

    #[derive(Clone)]
    struct TestLease {
        attachment: SharedSessionAttachment,
        connection_id: String,
        generation: u64,
        lease_id: String,
    }

    impl TestLease {
        fn from_attachment(attachment: SharedSessionAttachment) -> Self {
            Self {
                connection_id: attachment.connection_id.clone(),
                generation: attachment.generation,
                lease_id: attachment.lease_id.clone(),
                attachment,
            }
        }

        fn guard(&self) -> SharedMutationGuard {
            SharedMutationGuard {
                connection_id: self.connection_id.clone(),
                generation: self.generation,
                lease_id: self.lease_id.clone(),
            }
        }
    }

    impl From<&TestLease> for LeaseSocketBinding {
        fn from(lease: &TestLease) -> Self {
            Self {
                connection_id: lease.connection_id.clone(),
                generation: lease.generation,
                lease_id: lease.lease_id.clone(),
                lease_expires_at: lease.attachment.lease_expires_at,
            }
        }
    }

    fn broker_with_ttl(ttl: Duration) -> SharedSessionBroker {
        let broker = SharedSessionBroker::default();
        broker.configure_client_lease_ttl(ttl);
        broker
    }

    async fn reserve_client(
        broker: &SharedSessionBroker,
        conversation_id: i32,
        client: &str,
    ) -> TestLease {
        let outcome = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(conversation_id),
                &format!("connection-{client}"),
                client,
                &format!("request-{client}"),
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            outcome.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(conversation_id),
        )));
        broker
            .install_registered(
                &outcome.attachment.connection_id,
                outcome.attachment.generation,
                format!("driver-{client}"),
                state,
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        TestLease::from_attachment(outcome.attachment)
    }

    async fn attach_client(
        broker: &SharedSessionBroker,
        conversation_id: i32,
        client: &str,
    ) -> TestLease {
        let outcome = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(conversation_id),
                &format!("connection-{client}"),
                client,
                &format!("request-{client}"),
            ))
            .await
            .unwrap();
        TestLease::from_attachment(outcome.attachment)
    }

    async fn fill_and_expire_lease_tombstones(
        broker: &SharedSessionBroker,
        count: usize,
    ) -> TestLease {
        let mut newest = None;
        for n in 0..count {
            let lease = attach_client(broker, 1, &format!("fill-{n}")).await;
            tokio::time::advance(Duration::from_secs(2)).await;
            broker.expire_leases(tokio::time::Instant::now()).await;
            newest = Some(lease);
        }
        newest.expect("at least one expired lease")
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_renews_only_bound_leases_and_expiry_never_disconnects() {
        let broker = broker_with_ttl(Duration::from_secs(90));
        let a = reserve_client(&broker, 1, "a").await;
        let b = attach_client(&broker, 1, "b").await;
        tokio::time::advance(Duration::from_secs(60)).await;
        broker.renew_leases(&[LeaseSocketBinding::from(&a)]).await;
        tokio::time::advance(Duration::from_secs(31)).await;
        let expired = broker.expire_leases(tokio::time::Instant::now()).await;
        assert_eq!(expired, vec![b.lease_id.clone()]);
        assert!(broker.validate_guard(&a.guard()).await.is_ok());
        assert!(matches!(
            broker.validate_guard(&b.guard()).await,
            Err(SharedSessionError::LeaseExpired)
        ));
        assert_eq!(
            broker
                .diagnostic_for_connection(&a.connection_id)
                .await
                .unwrap()
                .phase,
            SharedSessionPhase::Bootstrapping
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_lease_tombstones_are_bounded_and_secret_safe() {
        let broker = broker_with_ttl(Duration::from_secs(1));
        let oldest = reserve_client(&broker, 1, "oldest").await;
        let newest =
            fill_and_expire_lease_tombstones(&broker, MAX_EXPIRED_LEASE_TOMBSTONES + 1).await;
        assert!(matches!(
            broker.validate_guard(&oldest.guard()).await,
            Err(SharedSessionError::LeaseMissing)
        ));
        assert!(matches!(
            broker.validate_guard(&newest.guard()).await,
            Err(SharedSessionError::LeaseExpired)
        ));
        let diagnostic = broker
            .diagnostic_for_connection(&newest.connection_id)
            .await
            .unwrap();
        assert_eq!(
            diagnostic.expired_lease_tombstone_count,
            MAX_EXPIRED_LEASE_TOMBSTONES
        );
    }

    #[tokio::test]
    async fn registration_watch_publishes_exact_public_state_arc() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(13),
                "registered-connection",
                "registered-client",
                "registered-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-incarnation".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        let registered = broker
            .wait_until_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
            )
            .await
            .unwrap();
        assert_eq!(registered.phase, SharedSessionPhase::Bootstrapping);
        assert!(Arc::ptr_eq(registered.state.as_ref().unwrap(), &state));
        assert_eq!(
            registered.driver_incarnation.as_deref(),
            Some("driver-incarnation")
        );
    }

    #[tokio::test]
    async fn ready_watch_observes_coherent_public_state() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(131),
                "ready-connection",
                "ready-client",
                "ready-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        state.write().await.shared_session = Some(SharedSessionProjection {
            generation: reservation.attachment.generation,
            phase: SharedSessionPhase::Bootstrapping,
            queue: Vec::new(),
            active_turn: None,
            lease_expires_at: Some(reservation.attachment.lease_expires_at),
            expired_lease_tombstone_count: 0,
        });
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-incarnation".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        let waiter = {
            let broker = broker.clone();
            let connection_id = reservation.attachment.connection_id.clone();
            let generation = reservation.attachment.generation;
            tokio::spawn(async move {
                broker
                    .wait_for_phase(&connection_id, generation, SharedSessionPhase::Ready)
                    .await
            })
        };
        broker
            .mark_ready(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-incarnation",
            )
            .await
            .unwrap();
        waiter.await.unwrap().unwrap();

        let state = state.read().await;
        assert_eq!(state.status, crate::acp::types::ConnectionStatus::Connected);
        assert_eq!(
            state
                .shared_session
                .as_ref()
                .map(|projection| &projection.phase),
            Some(&SharedSessionPhase::Ready)
        );
    }

    #[tokio::test]
    async fn terminal_public_status_cannot_settle_bootstrap_as_ready() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(133),
                "terminal-bootstrap-connection",
                "terminal-bootstrap-client",
                "terminal-bootstrap-request",
            ))
            .await
            .unwrap();
        let mut public_state = SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        );
        public_state.status = crate::acp::types::ConnectionStatus::Error;
        public_state.shared_session = Some(SharedSessionProjection {
            generation: reservation.attachment.generation,
            phase: SharedSessionPhase::Bootstrapping,
            queue: Vec::new(),
            active_turn: None,
            lease_expires_at: Some(reservation.attachment.lease_expires_at),
            expired_lease_tombstone_count: 0,
        });
        let state = Arc::new(tokio::sync::RwLock::new(public_state));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "terminal-bootstrap-driver".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        assert!(matches!(
            broker
                .mark_ready(
                    &reservation.attachment.connection_id,
                    reservation.attachment.generation,
                    "terminal-bootstrap-driver",
                )
                .await,
            Err(SharedSessionError::SessionUnavailable)
        ));
        assert_eq!(
            broker
                .diagnostic_for_connection(&reservation.attachment.connection_id)
                .await
                .unwrap()
                .phase,
            SharedSessionPhase::Bootstrapping
        );
        assert_eq!(
            state.read().await.status,
            crate::acp::types::ConnectionStatus::Error
        );
    }

    #[tokio::test]
    async fn contended_public_state_does_not_hold_the_broker_record_lock() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(132),
                "contended-state-connection",
                "contended-state-client",
                "contended-state-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-incarnation".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        let state_guard = state.write().await;
        let settlement = {
            let broker = broker.clone();
            let connection_id = reservation.attachment.connection_id.clone();
            let generation = reservation.attachment.generation;
            tokio::spawn(async move {
                broker
                    .mark_ready(&connection_id, generation, "driver-incarnation")
                    .await
            })
        };
        tokio::task::yield_now().await;

        let diagnostic = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            broker.diagnostic_for_connection(&reservation.attachment.connection_id),
        )
        .await;
        drop(state_guard);
        settlement.await.unwrap().unwrap();

        assert!(
            diagnostic.is_ok(),
            "state-lock contention must release the broker record lock"
        );
    }

    #[tokio::test]
    async fn shutdown_does_not_hold_the_broker_record_lock_while_state_is_contended() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(133),
                "shutdown-lock-connection",
                "shutdown-lock-client",
                "shutdown-lock-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "shutdown-lock-driver".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        let state_guard = state.write().await;
        let mut shutdown = Box::pin(broker.begin_shutdown());
        assert!(matches!(
            futures::poll!(shutdown.as_mut()),
            std::task::Poll::Pending
        ));
        let released = tokio::time::timeout(
            Duration::from_millis(100),
            broker.release_lease(&SharedMutationGuard {
                connection_id: reservation.attachment.connection_id.clone(),
                generation: reservation.attachment.generation,
                lease_id: reservation.attachment.lease_id.clone(),
            }),
        )
        .await
        .expect("shutdown state contention must not block broker mutations")
        .unwrap();
        assert!(released);

        drop(state_guard);
        shutdown.await;
    }

    #[tokio::test]
    async fn diagnostics_do_not_hold_the_broker_record_lock_while_state_is_contended() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(134),
                "diagnostics-lock-connection",
                "diagnostics-lock-client",
                "diagnostics-lock-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "diagnostics-lock-driver".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        let state_guard = state.write().await;
        let mut diagnostics = Box::pin(broker.diagnostics());
        assert!(matches!(
            futures::poll!(diagnostics.as_mut()),
            std::task::Poll::Pending
        ));
        let released = tokio::time::timeout(
            Duration::from_millis(100),
            broker.release_lease(&SharedMutationGuard {
                connection_id: reservation.attachment.connection_id.clone(),
                generation: reservation.attachment.generation,
                lease_id: reservation.attachment.lease_id.clone(),
            }),
        )
        .await
        .expect("diagnostic state contention must not block broker mutations")
        .unwrap();
        assert!(released);

        drop(state_guard);
        assert_eq!(diagnostics.await.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn diagnostic_phase_durations_are_independent_and_freeze_on_completion() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(135),
                "duration-connection",
                "duration-client",
                "duration-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        tokio::time::advance(Duration::from_secs(5)).await;
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "duration-driver".into(),
                state,
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(7)).await;
        broker
            .mark_ready(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "duration-driver",
            )
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(30)).await;

        let ready = broker.diagnostics().await.pop().unwrap();
        assert_eq!(ready.bootstrap_duration_ms, 12_000);
        assert_eq!(ready.cleanup_duration_ms, 0);
        assert_eq!(ready.cleanup_state, "not_started");

        broker
            .mark_failed(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "session_unavailable",
                false,
            )
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(3)).await;
        let cleaning = broker.diagnostics().await.pop().unwrap();
        assert_eq!(cleaning.bootstrap_duration_ms, 12_000);
        assert_eq!(cleaning.cleanup_duration_ms, 3_000);
        assert_eq!(cleaning.cleanup_state, "in_progress");

        broker
            .mark_cleanup_complete(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
            )
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(10)).await;
        let cleaned = broker.diagnostics().await.pop().unwrap();
        assert_eq!(cleaned.bootstrap_duration_ms, 12_000);
        assert_eq!(cleaned.cleanup_duration_ms, 3_000);
        assert_eq!(cleaned.cleanup_state, "complete");
    }

    #[tokio::test]
    async fn old_driver_incarnation_cannot_settle_replacement() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(14),
                "fallback-connection",
                "fallback-client",
                "fallback-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            None,
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-old".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        let mut replacement = SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            None,
        );
        replacement.connection_incarnation = "driver-new".into();
        let permit = broker
            .begin_registered_replacement(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-old",
            )
            .await
            .unwrap();
        assert!(matches!(
            broker
                .mark_ready(
                    &reservation.attachment.connection_id,
                    reservation.attachment.generation,
                    "driver-old",
                )
                .await,
            Err(SharedSessionError::GenerationStale)
        ));
        state
            .write()
            .await
            .prepare_registered_replacement(replacement);
        broker
            .commit_registered_replacement(
                &permit,
                "driver-new".into(),
                state,
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        assert!(matches!(
            broker
                .mark_ready(
                    &reservation.attachment.connection_id,
                    reservation.attachment.generation,
                    "driver-old",
                )
                .await,
            Err(SharedSessionError::GenerationStale)
        ));
        assert!(
            broker
                .is_current_bootstrapping_driver(
                    &reservation.attachment.connection_id,
                    reservation.attachment.generation,
                    "driver-new",
                )
                .await
        );
    }

    #[tokio::test]
    async fn failed_generation_publishes_replaced_before_record_swap() {
        let broker = failed_cleanup_complete_fixture(15).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let mut lifecycle = {
            let record = old_record.lock().await;
            record.lifecycle_tx.subscribe()
        };
        let mut retry = request(
            SharedSessionKey::Conversation(15),
            "replacement-connection",
            "replacement-client",
            "replacement-request",
        );
        retry.retry_failed_generation = Some(1);

        let replacement = broker.reserve_or_attach(retry).await.unwrap();
        assert_eq!(replacement.attachment.generation, 2);
        lifecycle.changed().await.unwrap();
        assert_eq!(*lifecycle.borrow(), SharedLifecycleState::Replaced);
    }

    #[tokio::test]
    async fn cleanup_completion_publication_survives_immediate_generation_replacement() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(151),
                "failed-connection",
                "failed-client",
                "failed-request",
            ))
            .await
            .unwrap();
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(151),
        )));
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-incarnation".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        let (failed_state, failed_emitter, failed_events) = broker
            .fail_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                Some("driver-incarnation"),
                SharedSessionError::CompanionInitializationFailed,
                false,
                state.clone(),
                EventEmitter::Noop,
            )
            .await
            .unwrap();
        for event in failed_events {
            crate::web::event_bridge::emit_with_state(&failed_state, &failed_emitter, event).await;
        }

        let mut old_stream = state.read().await.event_stream().subscribe();
        let sequence_before_cleanup = state.read().await.event_seq;
        let (cleanup_handles, cleanup_events) = broker
            .mark_cleanup_complete(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
            )
            .await
            .unwrap();
        let (cleanup_state, cleanup_emitter) = cleanup_handles.expect("registered handles");
        assert!(Arc::ptr_eq(&cleanup_state, &state));

        let mut retry = request(
            SharedSessionKey::Conversation(151),
            "replacement-connection",
            "replacement-client",
            "replacement-request",
        );
        retry.retry_failed_generation = Some(reservation.attachment.generation);
        let replacement = broker.reserve_or_attach(retry).await.unwrap();
        assert_eq!(replacement.attachment.generation, 2);
        assert!(broker
            .public_state_and_emitter(&reservation.attachment.connection_id)
            .await
            .is_none());

        for event in cleanup_events {
            crate::web::event_bridge::emit_with_state(&cleanup_state, &cleanup_emitter, event)
                .await;
        }

        let envelope = old_stream.recv().await.unwrap();
        assert_eq!(envelope.seq, sequence_before_cleanup + 1);
        assert!(matches!(
            &envelope.payload,
            crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                generation: 1,
                phase: SharedSessionPhase::Failed {
                    cleanup_complete: true,
                    ..
                },
            }
        ));
        let old_state = state.read().await;
        assert_eq!(old_state.event_seq, sequence_before_cleanup + 1);
        assert!(matches!(
            old_state
                .shared_session
                .as_ref()
                .map(|projection| &projection.phase),
            Some(SharedSessionPhase::Failed {
                cleanup_complete: true,
                ..
            })
        ));
    }

    #[test]
    fn debug_output_redacts_visible_text_and_lease_ids() {
        let summary = SharedQueuedPromptSummary {
            queue_item_id: "queue-a".into(),
            enqueue_seq: 1,
            client_message_id: "message-a".into(),
            visible_text: Some("private prompt".into()),
            visible_text_truncated: false,
            attachment_count: 0,
            submitted_at: chrono::Utc::now(),
            state: SharedQueuedPromptState::Queued,
        };
        let projection = SharedSessionProjection {
            generation: 1,
            phase: SharedSessionPhase::Ready,
            queue: vec![summary],
            active_turn: None,
            lease_expires_at: None,
            expired_lease_tombstone_count: 0,
        };
        let guard = SharedMutationGuard {
            connection_id: "conn-a".into(),
            generation: 1,
            lease_id: "private-lease".into(),
        };

        let encoded = format!("{projection:?} {guard:?}");
        assert!(!encoded.contains("private prompt"));
        assert!(!encoded.contains("private-lease"));
        assert!(encoded.contains("lease_id: \"***\""));
    }

    #[test]
    fn prompt_summaries_expose_only_bounded_text_and_attachment_count() {
        let private_capture = crate::auto_title::PromptCaptureContext::new(
            Some("private capture context".into()),
            None,
        );
        let long_text = format!("safe:{}", "界".repeat(MAX_QUEUE_VISIBLE_TEXT_CHARS));
        let summary = SharedQueuedPromptSummary::from_prompt(
            "queue-a".into(),
            1,
            "message-a".into(),
            &[
                crate::acp::types::PromptInputBlock::Text {
                    text: long_text.clone(),
                },
                crate::acp::types::PromptInputBlock::Image {
                    data: "private-base64".into(),
                    mime_type: "private-mime".into(),
                    uri: Some("private-image-uri".into()),
                },
                crate::acp::types::PromptInputBlock::Resource {
                    uri: "private-resource-uri".into(),
                    mime_type: Some("private-resource-mime".into()),
                    text: Some("private-resource-text".into()),
                    blob: Some("private-resource-blob".into()),
                },
                crate::acp::types::PromptInputBlock::ResourceLink {
                    uri: "private-link-uri".into(),
                    name: "private-link-name".into(),
                    mime_type: Some("private-link-mime".into()),
                    description: Some("private-link-description".into()),
                },
            ],
            Some(&private_capture),
            chrono::Utc::now(),
            SharedQueuedPromptState::Queued,
        );

        let visible = summary.visible_text.as_deref().unwrap();
        assert_eq!(visible.chars().count(), MAX_QUEUE_VISIBLE_TEXT_CHARS);
        assert!(long_text.starts_with(visible));
        assert!(summary.visible_text_truncated);
        assert_eq!(summary.attachment_count, 3);
        let serialized = serde_json::to_string(&summary).unwrap();
        for private in [
            "private capture context",
            "private-base64",
            "private-mime",
            "private-image-uri",
            "private-resource-uri",
            "private-resource-text",
            "private-resource-blob",
            "private-link-uri",
            "private-link-name",
            "private-link-description",
        ] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn shared_error_codes_are_stable() {
        let conflict = SharedSessionError::ConfigConflict {
            connection_id: "conn-a".into(),
            conflict_kind: SharedConfigConflictKind::AgentType,
        };
        for (error, expected) in [
            (conflict, "shared_session_config_conflict"),
            (
                SharedSessionError::ProtocolRequired,
                "shared_session_protocol_required",
            ),
            (
                SharedSessionError::GenerationStale,
                "shared_session_generation_stale",
            ),
            (SharedSessionError::Closing, "shared_session_closing"),
            (
                SharedSessionError::CleanupInProgress,
                "shared_session_cleanup_in_progress",
            ),
            (SharedSessionError::LeaseMissing, "client_lease_missing"),
            (SharedSessionError::LeaseExpired, "client_lease_expired"),
            (
                SharedSessionError::ClientLeaseCapacityExceeded,
                "client_lease_capacity_exceeded",
            ),
            (
                SharedSessionError::ConnectLedgerCapacityExceeded,
                "connect_idempotency_capacity_exceeded",
            ),
            (
                SharedSessionError::PromptLedgerCapacityExceeded,
                "prompt_idempotency_capacity_exceeded",
            ),
            (SharedSessionError::PromptQueueFull, "prompt_queue_full"),
            (
                SharedSessionError::IdempotencyKeyConflict,
                "idempotency_key_conflict",
            ),
            (
                SharedSessionError::QueueItemNotFound,
                "queue_item_not_found",
            ),
            (
                SharedSessionError::QueueItemAlreadyDispatching,
                "queue_item_already_dispatching",
            ),
            (
                SharedSessionError::InteractionAlreadyResolved,
                "interaction_already_resolved",
            ),
            (SharedSessionError::StaleTurn, "stale_turn"),
            (
                SharedSessionError::SessionUnavailable,
                "session_unavailable",
            ),
            (
                SharedSessionError::CompanionInitializationFailed,
                "companion_initialization_failed",
            ),
            (
                SharedSessionError::ConversationKeyConflict,
                "shared_session_conversation_key_conflict",
            ),
            (
                SharedSessionError::InvalidField { field: "device_id" },
                "invalid_shared_session_field",
            ),
        ] {
            assert_eq!(error.code(), expected);
            assert!(validate_failure_code(expected).is_ok());
        }
    }

    async fn failed_cleanup_complete_fixture(id: i32) -> SharedSessionBroker {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(id),
                "failed-connection",
                "failed-client",
                "failed-request",
            ))
            .await
            .unwrap();
        broker
            .mark_failed(
                &first.attachment.connection_id,
                1,
                "companion_initialization_failed",
                false,
            )
            .await
            .unwrap();
        broker
            .mark_cleanup_complete(&first.attachment.connection_id, 1)
            .await
            .unwrap();
        broker
    }

    async fn install_replacement_pointer(broker: &SharedSessionBroker, id: i32) {
        let replacement_request = request(
            SharedSessionKey::Conversation(id),
            "replacement-connection",
            "replacement-client",
            "replacement-request",
        );
        let mut replacement = SharedSessionRecord::reserved(&replacement_request, 2, Some(1));
        replacement
            .attach_or_renew_lease(
                &replacement_request,
                DEFAULT_CLIENT_LEASE_TTL,
                SharedDisposition::Created,
                BrokerLimits::default(),
            )
            .unwrap();
        let mut index = broker.index.lock().await;
        index.by_connection.remove("failed-connection");
        index.by_connection.insert(
            replacement_request.connection_id.clone(),
            replacement_request.key.clone(),
        );
        index
            .sessions
            .insert(replacement_request.key, Arc::new(Mutex::new(replacement)));
    }

    async fn record_for_connection_for_test(
        broker: &SharedSessionBroker,
        connection_id: &str,
    ) -> Arc<Mutex<SharedSessionRecord>> {
        let index = broker.index.lock().await;
        index.record_for_connection(connection_id).unwrap().clone()
    }

    struct BrokerAtIdentityLimits {
        broker: SharedSessionBroker,
        retry: SharedReserveRequest,
    }

    impl BrokerAtIdentityLimits {
        async fn retry_existing_connect(&self) -> Result<SharedReserveOutcome, SharedSessionError> {
            self.broker.reserve_or_attach(self.retry.clone()).await
        }

        async fn connect_new_identity(&self) -> Result<SharedReserveOutcome, SharedSessionError> {
            self.broker
                .reserve_or_attach(request(
                    SharedSessionKey::Conversation(12),
                    "ignored-connect-candidate",
                    "client-0",
                    "request-over-ledger-limit",
                ))
                .await
        }

        async fn attach_new_client(&self) -> Result<SharedReserveOutcome, SharedSessionError> {
            self.broker
                .reserve_or_attach(request(
                    SharedSessionKey::Conversation(12),
                    "ignored-lease-candidate",
                    "client-over-lease-limit",
                    "request-over-both-limits",
                ))
                .await
        }
    }

    async fn broker_at_identity_limits() -> BrokerAtIdentityLimits {
        const TEST_MAX_ACTIVE_LEASES: usize = 3;
        const TEST_MAX_CONNECT_LEDGER_ENTRIES: usize = 4;

        let broker = SharedSessionBroker::with_limits_for_test(
            TEST_MAX_ACTIVE_LEASES,
            TEST_MAX_CONNECT_LEDGER_ENTRIES,
        );
        let retry = request(
            SharedSessionKey::Conversation(12),
            "conn-a",
            "client-0",
            "request-0",
        );
        broker.reserve_or_attach(retry.clone()).await.unwrap();

        for n in 1..TEST_MAX_ACTIVE_LEASES {
            broker
                .reserve_or_attach(request(
                    SharedSessionKey::Conversation(12),
                    "ignored-candidate",
                    &format!("client-{n}"),
                    &format!("request-{n}"),
                ))
                .await
                .unwrap();
        }
        broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(12),
                "ignored-candidate",
                "client-0",
                "request-fill-ledger",
            ))
            .await
            .unwrap();

        BrokerAtIdentityLimits { broker, retry }
    }

    #[derive(Clone)]
    struct ReadyPromptBrokerFixture {
        broker: SharedSessionBroker,
        attachment: SharedSessionAttachment,
        guard: SharedMutationGuard,
    }

    impl ReadyPromptBrokerFixture {
        async fn enqueue(
            &self,
            request: SharedPromptRequest,
        ) -> Result<PromptEnqueueResult, SharedSessionError> {
            let admission = self.broker.enqueue_prompt(request).await?;
            self.publish_admission(&admission).await?;
            self.broker
                .finalize_enqueue_response(
                    &self.attachment.connection_id,
                    self.attachment.generation,
                    &admission.queue_item_id,
                )
                .await
        }

        async fn publish_admission(
            &self,
            admission: &SharedPromptAdmission,
        ) -> Result<(), SharedSessionError> {
            let events = admission.events.clone();
            admission
                .publication
                .get_or_try_init(|| async {
                    self.publish(events).await;
                    let published = self
                        .broker
                        .mark_prompt_admission_published(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            &admission.queue_item_id,
                        )
                        .await?;
                    if published {
                        admission.notify.notify_one();
                    }
                    Ok::<(), SharedSessionError>(())
                })
                .await?;
            Ok(())
        }

        async fn cancel(&self, queue_item_id: &str) -> Result<(), SharedSessionError> {
            let cancelled = self
                .broker
                .cancel_queued_prompt(&self.guard, queue_item_id)
                .await?;
            self.publish(cancelled.events).await;
            cancelled.notify.notify_one();
            Ok(())
        }

        async fn claim_head(&self) -> Result<(), SharedSessionError> {
            match self
                .broker
                .claim_dispatchable_head(
                    &self.attachment.connection_id,
                    self.attachment.generation,
                    "test-turn",
                    &dispatchable_runtime_snapshot(),
                )
                .await?
            {
                DispatchHeadDecision::Claimed(claimed) => {
                    self.publish(claimed.events).await;
                    Ok(())
                }
                DispatchHeadDecision::Blocked | DispatchHeadDecision::Failed(_) => {
                    Err(SharedSessionError::QueueItemNotFound)
                }
            }
        }

        async fn snapshot(&self) -> SharedSessionProjection {
            self.broker
                .diagnostic_for_connection(&self.attachment.connection_id)
                .await
                .expect("fixture record remains authoritative")
        }

        async fn item_state(&self, queue_item_id: &str) -> Option<InternalPromptState> {
            self.broker
                .prompt_state_for_test(&self.attachment.connection_id, queue_item_id)
                .await
        }

        async fn publish(&self, events: Vec<crate::acp::types::AcpEvent>) {
            let (state, emitter) = self
                .broker
                .public_state_and_emitter(&self.attachment.connection_id)
                .await
                .expect("ready fixture has publication handles");
            for event in events {
                crate::web::event_bridge::emit_with_state(&state, &emitter, event).await;
            }
        }
    }

    fn dispatchable_runtime_snapshot() -> SharedRuntimeWorkSnapshot {
        SharedRuntimeWorkSnapshot {
            event_seq: 0,
            status: crate::acp::types::ConnectionStatus::Connected,
            turn_in_flight: false,
            pending_permission_id: None,
            pending_question_id: None,
            pending_plan_approval_id: None,
            continuation_wait: false,
            active_delegations: 0,
            background_outstanding: 0,
            conversation_write_error: None,
        }
    }

    async fn ready_prompt_broker_fixture() -> ReadyPromptBrokerFixture {
        ready_prompt_broker_fixture_with_limits(
            MAX_PROMPT_LEDGER_ENTRIES,
            MAX_WAITING_PROMPTS,
            MAX_WAITING_BYTES,
        )
        .await
    }

    async fn ready_prompt_broker_fixture_with_limits(
        max_prompt_ledger_entries: usize,
        max_waiting_prompts: usize,
        max_waiting_bytes: usize,
    ) -> ReadyPromptBrokerFixture {
        let broker = SharedSessionBroker::with_prompt_limits_for_test(
            max_prompt_ledger_entries,
            max_waiting_prompts,
            max_waiting_bytes,
        );
        ready_prompt_broker_fixture_from_broker(broker).await
    }

    async fn ready_prompt_broker_fixture_from_broker(
        broker: SharedSessionBroker,
    ) -> ReadyPromptBrokerFixture {
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(701),
                "prompt-connection",
                "prompt-client",
                "prompt-connect",
            ))
            .await
            .unwrap();
        let attachment = reservation.attachment;
        let mut state = SessionState::new(
            attachment.connection_id.clone(),
            crate::models::agent::AgentType::Codex,
            None,
            "shared-server".into(),
            Some(701),
        );
        state.connection_incarnation = "prompt-driver".into();
        state.status = crate::acp::types::ConnectionStatus::Connected;
        state.shared_session = Some(SharedSessionProjection {
            generation: attachment.generation,
            phase: SharedSessionPhase::Bootstrapping,
            queue: Vec::new(),
            active_turn: None,
            lease_expires_at: Some(attachment.lease_expires_at),
            expired_lease_tombstone_count: 0,
        });
        let state = Arc::new(RwLock::new(state));
        broker
            .install_registered(
                &attachment.connection_id,
                attachment.generation,
                "prompt-driver".into(),
                state,
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        broker
            .mark_ready(
                &attachment.connection_id,
                attachment.generation,
                "prompt-driver",
            )
            .await
            .unwrap();
        let guard = SharedMutationGuard {
            connection_id: attachment.connection_id.clone(),
            generation: attachment.generation,
            lease_id: attachment.lease_id.clone(),
        };
        ReadyPromptBrokerFixture {
            broker,
            attachment,
            guard,
        }
    }

    fn prompt_request(n: usize) -> SharedPromptRequest {
        prompt_with_ids(
            "prompt-client",
            &format!("prompt-{n}"),
            &format!("text-{n}"),
        )
    }

    fn prompt_with_ids(client: &str, request_id: &str, text: &str) -> SharedPromptRequest {
        SharedPromptRequest {
            guard: SharedMutationGuard {
                connection_id: "prompt-connection".into(),
                generation: 1,
                lease_id: String::new(),
            },
            client_instance_id: client.into(),
            client_request_id: request_id.into(),
            blocks: vec![crate::acp::types::PromptInputBlock::Text { text: text.into() }],
            folder_id: Some(9),
            conversation_id: Some(701),
            client_message_id: format!("message-{request_id}"),
            capture: None,
            submitted_at: chrono::Utc::now(),
        }
    }

    fn with_fixture_guard(
        fixture: &ReadyPromptBrokerFixture,
        mut request: SharedPromptRequest,
    ) -> SharedPromptRequest {
        request.guard = fixture.guard.clone();
        request
    }

    #[tokio::test]
    async fn concurrent_enqueues_assign_contiguous_fifo_sequence() {
        let fixture = ready_prompt_broker_fixture().await;
        let results = futures::future::join_all((0..64).map(|n| {
            let fixture = fixture.clone();
            async move {
                let request = with_fixture_guard(&fixture, prompt_request(n));
                fixture.enqueue(request).await.unwrap()
            }
        }))
        .await;
        let mut seqs: Vec<_> = results
            .into_iter()
            .map(|result| result.enqueue_seq)
            .collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=64).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn identical_retry_returns_original_and_changed_payload_conflicts() {
        let fixture = ready_prompt_broker_fixture().await;
        let first_request =
            with_fixture_guard(&fixture, prompt_with_ids("prompt-client", "retry", "alpha"));
        let first = fixture.enqueue(first_request.clone()).await.unwrap();
        let same = fixture.enqueue(first_request).await.unwrap();
        assert_eq!(first, same);
        assert!(matches!(
            fixture
                .enqueue(with_fixture_guard(
                    &fixture,
                    prompt_with_ids("prompt-client", "retry", "beta"),
                ))
                .await,
            Err(SharedSessionError::IdempotencyKeyConflict)
        ));
    }

    #[tokio::test]
    async fn broker_metrics_balance_queue_lifecycle_and_idempotent_retries() {
        let fixture = ready_prompt_broker_fixture().await;
        let initial = fixture.broker.metrics().snapshot();
        assert_eq!(initial.bootstrap_ready_total, 1);
        assert_eq!(initial.bootstrap_duration_samples, 1);
        assert_eq!(initial.waiting_prompts, 0);
        assert_eq!(initial.waiting_bytes, 0);

        let first_request = with_fixture_guard(&fixture, prompt_request(101));
        let first = fixture.enqueue(first_request.clone()).await.unwrap();
        let after_first = fixture.broker.metrics().snapshot();
        assert_eq!(after_first.enqueue_total, 1);
        assert_eq!(after_first.waiting_prompts, 1);
        assert!(after_first.waiting_bytes > 0);

        let retried = fixture.enqueue(first_request).await.unwrap();
        assert_eq!(retried, first);
        let after_retry = fixture.broker.metrics().snapshot();
        assert_eq!(after_retry.enqueue_total, after_first.enqueue_total);
        assert_eq!(after_retry.waiting_prompts, after_first.waiting_prompts);
        assert_eq!(after_retry.waiting_bytes, after_first.waiting_bytes);

        let second = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(102)))
            .await
            .unwrap();
        let after_second = fixture.broker.metrics().snapshot();
        assert_eq!(after_second.enqueue_total, 2);
        assert_eq!(after_second.waiting_prompts, 2);
        assert!(after_second.waiting_bytes > after_first.waiting_bytes);

        fixture.cancel(&second.queue_item_id).await.unwrap();
        let after_cancel = fixture.broker.metrics().snapshot();
        assert_eq!(after_cancel.cancel_total, 1);
        assert_eq!(after_cancel.waiting_prompts, 1);
        assert_eq!(after_cancel.waiting_bytes, after_first.waiting_bytes);

        fixture.claim_head().await.unwrap();
        let after_dispatch = fixture.broker.metrics().snapshot();
        assert_eq!(after_dispatch.dispatch_total, 1);
        assert_eq!(after_dispatch.waiting_prompts, 0);
        assert_eq!(after_dispatch.waiting_bytes, 0);

        fixture
            .broker
            .fail_claimed_item(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-turn",
                "session_unavailable",
            )
            .await
            .unwrap();
        assert_eq!(
            fixture.broker.metrics().snapshot().queue_item_failed_total,
            1
        );

        fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(103)))
            .await
            .unwrap();
        fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(104)))
            .await
            .unwrap();
        assert_eq!(fixture.broker.metrics().snapshot().waiting_prompts, 2);
        fixture
            .broker
            .mark_failed(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "session_unavailable",
                true,
            )
            .await
            .unwrap();
        let after_fail_all = fixture.broker.metrics().snapshot();
        assert_eq!(after_fail_all.enqueue_total, 4);
        assert_eq!(after_fail_all.queue_item_failed_total, 3);
        assert_eq!(after_fail_all.waiting_prompts, 0);
        assert_eq!(after_fail_all.waiting_bytes, 0);
        assert_eq!(after_fail_all.bootstrap_ready_total, 1);
        assert!(after_fail_all.bootstrap_failed_total.is_empty());
        assert_eq!(after_fail_all.bootstrap_duration_samples, 1);

        let mut replacement = request(
            SharedSessionKey::Conversation(701),
            "prompt-replacement",
            "replacement-client",
            "replacement-request",
        );
        replacement.retry_failed_generation = Some(fixture.attachment.generation);
        fixture.broker.reserve_or_attach(replacement).await.unwrap();
        let after_replacement = fixture.broker.metrics().snapshot();
        assert_eq!(after_replacement.waiting_prompts, 0);
        assert_eq!(after_replacement.waiting_bytes, 0);
        assert_eq!(after_replacement.live_sessions, 1);
        assert_eq!(after_replacement.active_leases, 1);
    }

    #[tokio::test]
    async fn unpublished_admission_blocks_claim_and_remains_recoverable_by_retry() {
        let fixture = ready_prompt_broker_fixture().await;
        let request = with_fixture_guard(
            &fixture,
            prompt_with_ids("prompt-client", "publish-retry", "alpha"),
        );
        let first = fixture
            .broker
            .enqueue_prompt(request.clone())
            .await
            .unwrap();

        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "must-not-claim-before-publication",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Blocked
        ));

        let retry = fixture.broker.enqueue_prompt(request).await.unwrap();
        assert_eq!(retry.queue_item_id, first.queue_item_id);
        assert_eq!(retry.events.len(), 2);

        fixture.publish_admission(&retry).await.unwrap();
        fixture.publish_admission(&first).await.unwrap();
        let claimed = fixture
            .broker
            .claim_dispatchable_head(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "claim-after-publication",
                &dispatchable_runtime_snapshot(),
            )
            .await
            .unwrap();
        let DispatchHeadDecision::Claimed(claimed) = claimed else {
            panic!("published admission must become dispatchable");
        };
        fixture.publish(claimed.events).await;

        let (state, _) = fixture
            .broker
            .public_state_and_emitter(&fixture.attachment.connection_id)
            .await
            .unwrap();
        let state = state.read().await;
        let projection = state.shared_session.as_ref().unwrap();
        assert!(projection
            .queue
            .iter()
            .all(|item| item.queue_item_id != retry.queue_item_id));
        assert_eq!(
            projection
                .active_turn
                .as_ref()
                .map(|turn| turn.queue_item_id.as_str()),
            Some(retry.queue_item_id.as_str())
        );
    }

    #[tokio::test]
    async fn limits_reject_new_item_without_dropping_existing_items() {
        let fixture = ready_prompt_broker_fixture().await;
        for n in 0..MAX_WAITING_PROMPTS {
            let request = with_fixture_guard(&fixture, prompt_request(n));
            fixture.enqueue(request).await.unwrap();
        }
        assert!(matches!(
            fixture
                .enqueue(with_fixture_guard(&fixture, prompt_request(65)))
                .await,
            Err(SharedSessionError::PromptQueueFull)
        ));
        assert_eq!(fixture.snapshot().await.queue.len(), MAX_WAITING_PROMPTS);
    }

    #[tokio::test]
    async fn waiting_byte_limit_rejects_only_the_new_item() {
        let first_request = prompt_with_ids("prompt-client", "bytes-a", "alpha");
        let first_bytes = canonical_prompt_bytes(&first_request).unwrap().len();
        let fixture = ready_prompt_broker_fixture_with_limits(
            MAX_PROMPT_LEDGER_ENTRIES,
            MAX_WAITING_PROMPTS,
            first_bytes,
        )
        .await;
        let first = fixture
            .enqueue(with_fixture_guard(&fixture, first_request))
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .enqueue(with_fixture_guard(
                    &fixture,
                    prompt_with_ids("prompt-client", "bytes-b", "beta"),
                ))
                .await,
            Err(SharedSessionError::PromptQueueFull)
        ));
        assert_eq!(
            fixture.snapshot().await.queue[0].queue_item_id,
            first.queue_item_id
        );
    }

    #[tokio::test]
    async fn prompt_ledger_capacity_keeps_existing_retry_available() {
        let fixture =
            ready_prompt_broker_fixture_with_limits(2, MAX_WAITING_PROMPTS, MAX_WAITING_BYTES)
                .await;
        let first_request = with_fixture_guard(
            &fixture,
            prompt_with_ids("prompt-client", "retry-a", "alpha"),
        );
        let first = fixture.enqueue(first_request.clone()).await.unwrap();
        fixture
            .enqueue(with_fixture_guard(
                &fixture,
                prompt_with_ids("prompt-client", "retry-b", "beta"),
            ))
            .await
            .unwrap();
        assert_eq!(fixture.enqueue(first_request).await.unwrap(), first);
        assert!(matches!(
            fixture
                .enqueue(with_fixture_guard(
                    &fixture,
                    prompt_with_ids("prompt-client", "retry-c", "gamma"),
                ))
                .await,
            Err(SharedSessionError::PromptLedgerCapacityExceeded)
        ));
    }

    #[tokio::test]
    async fn cancel_and_dispatch_have_one_linearizable_winner() {
        let fixture = ready_prompt_broker_fixture().await;
        let item = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let (cancel, claim) =
            tokio::join!(fixture.cancel(&item.queue_item_id), fixture.claim_head());
        assert_ne!(cancel.is_ok(), claim.is_ok());
        assert!(matches!(
            fixture.item_state(&item.queue_item_id).await,
            Some(InternalPromptState::Cancelled | InternalPromptState::Dispatching)
        ));
    }

    #[tokio::test]
    async fn cancel_rejects_terminal_item_whose_response_froze_as_dispatching() {
        let fixture = ready_prompt_broker_fixture().await;
        let admission = fixture
            .broker
            .enqueue_prompt(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        fixture.publish_admission(&admission).await.unwrap();
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "frozen-dispatching-turn",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        assert_eq!(
            fixture
                .broker
                .finalize_enqueue_response(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    &admission.queue_item_id,
                )
                .await
                .unwrap()
                .state,
            SharedQueuedPromptState::Dispatching
        );
        fixture
            .broker
            .settle_active_turn(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "prompt-driver",
                "end_turn",
            )
            .await
            .unwrap();

        assert!(matches!(
            fixture.cancel(&admission.queue_item_id).await,
            Err(SharedSessionError::QueueItemAlreadyDispatching)
        ));
    }

    #[tokio::test]
    async fn conversation_rekey_collision_fails_closed() {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Ephemeral("ephemeral-a".into()),
                "ephemeral-a",
                "client-a",
                "request-a",
            ))
            .await
            .unwrap();
        let second = broker
            .reserve_or_attach(request(
                SharedSessionKey::Ephemeral("ephemeral-b".into()),
                "ephemeral-b",
                "client-b",
                "request-b",
            ))
            .await
            .unwrap();
        broker
            .bind_conversation_key(
                &first.attachment.connection_id,
                first.attachment.generation,
                88,
            )
            .await
            .unwrap();
        assert_eq!(
            broker
                .bind_conversation_key(
                    &second.attachment.connection_id,
                    second.attachment.generation,
                    88,
                )
                .await
                .unwrap_err(),
            SharedSessionError::ConversationKeyConflict
        );
        assert!(matches!(
            broker
                .key_for_connection_for_test(&first.attachment.connection_id)
                .await,
            Some(SharedSessionKey::Conversation(88))
        ));
        assert!(matches!(
            broker
                .key_for_connection_for_test(&second.attachment.connection_id)
                .await,
            Some(SharedSessionKey::Ephemeral(key)) if key == "ephemeral-b"
        ));
    }

    async fn conversation_rekey_fixture(
        conversation_id: i32,
    ) -> (
        SharedSessionBroker,
        SharedSessionAttachment,
        SharedSessionAttachment,
    ) {
        let broker = SharedSessionBroker::default();
        let source = broker
            .reserve_or_attach(request(
                SharedSessionKey::Ephemeral("rekey-source".into()),
                "rekey-source",
                "source-client",
                "source-request",
            ))
            .await
            .unwrap()
            .attachment;
        let destination = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(conversation_id),
                "rekey-destination",
                "destination-client",
                "destination-request",
            ))
            .await
            .unwrap()
            .attachment;

        for (attachment, bound_conversation_id, driver) in [
            (&source, None, "source-driver"),
            (
                &destination,
                Some(conversation_id),
                "destination-driver",
            ),
        ] {
            let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
                attachment.connection_id.clone(),
                crate::models::agent::AgentType::Codex,
                None,
                "shared-server".into(),
                Some(9),
            )));
            state.write().await.conversation_id = bound_conversation_id;
            broker
                .install_registered(
                    &attachment.connection_id,
                    attachment.generation,
                    driver.into(),
                    state,
                    EventEmitter::Noop,
                    Arc::new(std::sync::atomic::AtomicU32::new(0)),
                )
                .await
                .unwrap();
        }

        (broker, source, destination)
    }

    async fn bind_rekey_source(
        broker: &SharedSessionBroker,
        source: &SharedSessionAttachment,
        conversation_id: i32,
        guarded: bool,
    ) -> Result<(), SharedSessionError> {
        if guarded {
            broker
                .bind_conversation_key_guarded(
                    &SharedMutationGuard {
                        connection_id: source.connection_id.clone(),
                        generation: source.generation,
                        lease_id: source.lease_id.clone(),
                    },
                    conversation_id,
                    9,
                )
                .await
        } else {
            broker
                .bind_conversation_key(
                    &source.connection_id,
                    source.generation,
                    conversation_id,
                )
                .await
        }
    }

    #[tokio::test]
    async fn conversation_rekey_rejects_a_closing_destination() {
        for guarded in [false, true] {
            let (broker, source, destination) = conversation_rekey_fixture(188).await;
            broker
                .begin_termination(&destination.connection_id, destination.generation)
                .await
                .unwrap();

            assert_eq!(
                bind_rekey_source(&broker, &source, 188, guarded)
                    .await
                    .unwrap_err(),
                SharedSessionError::ConversationKeyConflict
            );
            assert!(broker
                .is_managed_connection(&destination.connection_id)
                .await);
        }
    }

    #[tokio::test]
    async fn conversation_rekey_rejects_a_cleanup_pending_failed_destination() {
        for guarded in [false, true] {
            let (broker, source, destination) = conversation_rekey_fixture(189).await;
            broker
                .mark_failed(
                    &destination.connection_id,
                    destination.generation,
                    "session_unavailable",
                    false,
                )
                .await
                .unwrap();

            assert_eq!(
                bind_rekey_source(&broker, &source, 189, guarded)
                    .await
                    .unwrap_err(),
                SharedSessionError::ConversationKeyConflict
            );
        }
    }

    #[tokio::test]
    async fn conversation_rekey_rejects_a_cleaned_failed_destination_with_a_lease() {
        for guarded in [false, true] {
            let (broker, source, destination) = conversation_rekey_fixture(190).await;
            broker
                .mark_failed(
                    &destination.connection_id,
                    destination.generation,
                    "session_unavailable",
                    true,
                )
                .await
                .unwrap();

            assert_eq!(
                bind_rekey_source(&broker, &source, 190, guarded)
                    .await
                    .unwrap_err(),
                SharedSessionError::ConversationKeyConflict
            );
        }
    }

    #[tokio::test]
    async fn conversation_rekey_replaces_only_a_cleaned_zero_lease_failed_tombstone() {
        for guarded in [false, true] {
            let (broker, source, destination) = conversation_rekey_fixture(191).await;
            broker
                .mark_failed(
                    &destination.connection_id,
                    destination.generation,
                    "session_unavailable",
                    true,
                )
                .await
                .unwrap();
            broker
                .release_lease(&SharedMutationGuard {
                    connection_id: destination.connection_id.clone(),
                    generation: destination.generation,
                    lease_id: destination.lease_id.clone(),
                })
                .await
                .unwrap();

            let before = broker.metrics().snapshot();
            assert_eq!(before.live_sessions, 2);
            assert_eq!(before.active_leases, 1);
            bind_rekey_source(&broker, &source, 191, guarded)
                .await
                .unwrap();

            assert!(matches!(
                broker
                    .key_for_connection_for_test(&source.connection_id)
                    .await,
                Some(SharedSessionKey::Conversation(191))
            ));
            assert!(broker
                .key_for_connection_for_test(&destination.connection_id)
                .await
                .is_none());
            let index = broker.index.lock().await;
            assert!(index.is_replaced_connection(
                &destination.connection_id,
                destination.generation
            ));
            drop(index);
            let after = broker.metrics().snapshot();
            assert_eq!(after.live_sessions, 1);
            assert_eq!(after.active_leases, 1);
        }
    }

    #[tokio::test]
    async fn enqueue_response_reflects_dispatch_claim_that_won_before_response() {
        let fixture = ready_prompt_broker_fixture().await;
        let admission = fixture
            .broker
            .enqueue_prompt(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        fixture.publish_admission(&admission).await.unwrap();
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "turn-before-response",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        let result = fixture
            .broker
            .finalize_enqueue_response(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                &admission.queue_item_id,
            )
            .await
            .unwrap();
        assert_eq!(result.state, SharedQueuedPromptState::Dispatching);
    }

    #[tokio::test]
    async fn runtime_blockers_leave_fifo_head_queued() {
        let fixture = ready_prompt_broker_fixture().await;
        let item = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let mut snapshots = Vec::new();
        let mut turn = dispatchable_runtime_snapshot();
        turn.turn_in_flight = true;
        snapshots.push(turn);
        let mut permission = dispatchable_runtime_snapshot();
        permission.pending_permission_id = Some("permission-a".into());
        snapshots.push(permission);
        let mut question = dispatchable_runtime_snapshot();
        question.pending_question_id = Some("question-a".into());
        snapshots.push(question);
        let mut approval = dispatchable_runtime_snapshot();
        approval.pending_plan_approval_id = Some("approval-a".into());
        snapshots.push(approval);
        let mut continuation = dispatchable_runtime_snapshot();
        continuation.continuation_wait = true;
        snapshots.push(continuation);
        let mut delegation = dispatchable_runtime_snapshot();
        delegation.active_delegations = 1;
        snapshots.push(delegation);
        let mut background = dispatchable_runtime_snapshot();
        background.background_outstanding = 1;
        snapshots.push(background);

        for (n, snapshot) in snapshots.iter().enumerate() {
            assert!(matches!(
                fixture
                    .broker
                    .claim_dispatchable_head(
                        &fixture.attachment.connection_id,
                        fixture.attachment.generation,
                        &format!("blocked-turn-{n}"),
                        snapshot,
                    )
                    .await
                    .unwrap(),
                DispatchHeadDecision::Blocked
            ));
            assert_eq!(
                fixture.snapshot().await.queue[0].queue_item_id,
                item.queue_item_id
            );
        }
    }

    #[tokio::test]
    async fn active_turn_blocks_tail_until_matching_terminal_settlement() {
        let fixture = ready_prompt_broker_fixture().await;
        let first = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let second = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(2)))
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "first-turn",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "tail-too-early",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Blocked
        ));
        fixture
            .broker
            .settle_active_turn(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "prompt-driver",
                "end_turn",
            )
            .await
            .unwrap();
        assert_eq!(
            fixture.item_state(&first.queue_item_id).await,
            Some(InternalPromptState::Completed)
        );
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "second-turn",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        assert_eq!(
            fixture.item_state(&second.queue_item_id).await,
            Some(InternalPromptState::Dispatching)
        );
    }

    #[tokio::test]
    async fn non_writable_conversation_fails_only_fifo_head() {
        let fixture = ready_prompt_broker_fixture().await;
        let first = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let second = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(2)))
            .await
            .unwrap();
        let mut snapshot = dispatchable_runtime_snapshot();
        snapshot.conversation_write_error = Some("workflow_identity_corrupt");
        let failed = fixture
            .broker
            .claim_dispatchable_head(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "unwritable",
                &snapshot,
            )
            .await
            .unwrap();
        let DispatchHeadDecision::Failed(failed) = failed else {
            panic!("non-writable conversation must fail the FIFO head");
        };
        assert!(failed.events.iter().any(|event| matches!(
            event,
            crate::acp::types::AcpEvent::PromptQueueItemFailed { error_code, .. }
                if error_code == "workflow_identity_corrupt"
        )));
        assert_eq!(
            fixture.item_state(&first.queue_item_id).await,
            Some(InternalPromptState::Failed)
        );
        assert_eq!(
            fixture.snapshot().await.queue[0].queue_item_id,
            second.queue_item_id
        );
    }

    #[tokio::test]
    async fn non_writable_head_preserves_each_stable_failure_code() {
        for error_code in [
            "workflow_v2_retired",
            "workflow_identity_corrupt",
            "legacy_completion_protocol_read_only",
            "unsupported_completion_protocol",
            "session_unavailable",
        ] {
            let fixture = ready_prompt_broker_fixture().await;
            fixture
                .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
                .await
                .unwrap();
            let mut snapshot = dispatchable_runtime_snapshot();
            snapshot.conversation_write_error = Some(error_code);
            let decision = fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "non-writable",
                    &snapshot,
                )
                .await
                .unwrap();
            let DispatchHeadDecision::Failed(failed) = decision else {
                panic!("non-writable conversation must fail the FIFO head");
            };
            assert!(failed.events.iter().any(|event| matches!(
                event,
                crate::acp::types::AcpEvent::PromptQueueItemFailed {
                    error_code: actual,
                    ..
                } if actual == error_code
            )));
        }
    }

    #[tokio::test]
    async fn runtime_reconcile_fails_ready_session_after_missed_disconnect() {
        let fixture = ready_prompt_broker_fixture().await;
        let first = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let second = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(2)))
            .await
            .unwrap();
        let mut snapshot = dispatchable_runtime_snapshot();
        snapshot.status = crate::acp::types::ConnectionStatus::Disconnected;

        let events = fixture
            .broker
            .reconcile_runtime_snapshot(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "prompt-driver",
                &snapshot,
            )
            .await
            .unwrap();

        assert!(matches!(
            fixture.snapshot().await.phase,
            SharedSessionPhase::Failed { .. }
        ));
        assert_eq!(
            fixture.item_state(&first.queue_item_id).await,
            Some(InternalPromptState::Failed)
        );
        assert_eq!(
            fixture.item_state(&second.queue_item_id).await,
            Some(InternalPromptState::Failed)
        );
        assert!(events.iter().any(|event| matches!(
            event,
            crate::acp::types::AcpEvent::SharedSessionPhaseChanged {
                phase: SharedSessionPhase::Failed { .. },
                ..
            }
        )));
    }

    #[tokio::test(start_paused = true)]
    async fn sender_lease_expiry_preserves_waiting_fifo() {
        let fixture =
            ready_prompt_broker_fixture_from_broker(broker_with_ttl(Duration::from_secs(90))).await;
        let first = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let second = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(2)))
            .await
            .unwrap();

        tokio::time::advance(Duration::from_secs(91)).await;
        assert_eq!(
            fixture
                .broker
                .expire_leases(tokio::time::Instant::now())
                .await,
            vec![fixture.attachment.lease_id.clone()]
        );
        assert_eq!(
            fixture
                .snapshot()
                .await
                .queue
                .iter()
                .map(|item| item.queue_item_id.as_str())
                .collect::<Vec<_>>(),
            [first.queue_item_id.as_str(), second.queue_item_id.as_str()]
        );
    }

    #[tokio::test]
    async fn stop_requested_active_turn_quarantines_fifo_tail_until_terminal() {
        let fixture = ready_prompt_broker_fixture().await;
        let first = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(1)))
            .await
            .unwrap();
        let second = fixture
            .enqueue(with_fixture_guard(&fixture, prompt_request(2)))
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "stopping-turn",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        fixture
            .broker
            .with_authoritative_record(&fixture.attachment.connection_id, |record| {
                record
                    .active_turn
                    .as_mut()
                    .expect("claimed head remains active")
                    .projection
                    .stop_requested = true;
                Ok(())
            })
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "tail-before-terminal",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Blocked
        ));
        assert_eq!(
            fixture.snapshot().await.queue[0].queue_item_id,
            second.queue_item_id
        );

        let settled = fixture
            .broker
            .settle_active_turn(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "prompt-driver",
                "end_turn",
            )
            .await
            .unwrap();
        assert!(matches!(
            settled.as_slice(),
            [crate::acp::types::AcpEvent::SharedTurnSettled {
                outcome: SharedTurnOutcome::Cancelled,
                ..
            }]
        ));
        assert_eq!(
            fixture.item_state(&first.queue_item_id).await,
            Some(InternalPromptState::Cancelled)
        );
        assert!(matches!(
            fixture
                .broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "tail-after-terminal",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
    }

    #[derive(Default)]
    struct FakeSharedControlAdapter {
        cancel_calls: AtomicUsize,
        permission_calls: AtomicUsize,
        question_calls: AtomicUsize,
        plan_approval_calls: AtomicUsize,
        cancel_failures: Mutex<VecDeque<SharedControlAdmissionError>>,
        interaction_failures: Mutex<VecDeque<SharedControlAdmissionError>>,
    }

    #[async_trait::async_trait]
    impl SharedControlAdapter for FakeSharedControlAdapter {
        async fn cancel(
            &self,
            _manager: &ConnectionManager,
            _db: &sea_orm::DatabaseConnection,
            _connection_id: &str,
            _claim: &SharedStopClaim,
        ) -> Result<(), SharedControlAdmissionError> {
            self.cancel_calls.fetch_add(1, Ordering::SeqCst);
            self.cancel_failures
                .lock()
                .await
                .pop_front()
                .map_or(Ok(()), Err)
        }

        async fn respond_permission(
            &self,
            _manager: &ConnectionManager,
            _connection_id: &str,
            _request_id: &str,
            _option_id: &str,
        ) -> Result<(), SharedControlAdmissionError> {
            self.permission_calls.fetch_add(1, Ordering::SeqCst);
            self.interaction_failures
                .lock()
                .await
                .pop_front()
                .map_or(Ok(()), Err)
        }

        async fn answer_question(
            &self,
            _manager: &ConnectionManager,
            _connection_id: &str,
            _question_id: &str,
            _answer: QuestionAnswer,
        ) -> Result<(), SharedControlAdmissionError> {
            self.question_calls.fetch_add(1, Ordering::SeqCst);
            self.interaction_failures
                .lock()
                .await
                .pop_front()
                .map_or(Ok(()), Err)
        }

        async fn answer_plan_approval(
            &self,
            _manager: &ConnectionManager,
            _connection_id: &str,
            _approval_id: &str,
            _answer: PlanApprovalAnswer,
        ) -> Result<(), SharedControlAdmissionError> {
            self.plan_approval_calls.fetch_add(1, Ordering::SeqCst);
            self.interaction_failures
                .lock()
                .await
                .pop_front()
                .map_or(Ok(()), Err)
        }
    }

    struct SharedControlFixture {
        manager: ConnectionManager,
        adapter: Arc<FakeSharedControlAdapter>,
        attachment: SharedSessionAttachment,
        guard: SharedMutationGuard,
        db: sea_orm::DatabaseConnection,
        interaction_kind: Option<SharedInteractionKind>,
    }

    impl SharedControlFixture {
        async fn claim(&self, interaction_id: &str) -> Result<(), AcpError> {
            match self.interaction_kind.expect("fixture has an interaction") {
                SharedInteractionKind::Permission => {
                    self.manager
                        .respond_shared_permission(SharedInteractionRequest {
                            guard: self.guard.clone(),
                            interaction_id: interaction_id.to_string(),
                            answer: "allow".to_string(),
                        })
                        .await
                }
                SharedInteractionKind::Question => {
                    self.manager
                        .answer_shared_question(SharedInteractionRequest {
                            guard: self.guard.clone(),
                            interaction_id: interaction_id.to_string(),
                            answer: QuestionAnswer::default(),
                        })
                        .await
                }
                SharedInteractionKind::PlanApproval => {
                    self.manager
                        .answer_shared_plan_approval(SharedInteractionRequest {
                            guard: self.guard.clone(),
                            interaction_id: interaction_id.to_string(),
                            answer: PlanApprovalAnswer {
                                decision: PlanApprovalDecision::Approve,
                                feedback: None,
                            },
                        })
                        .await
                }
            }
        }

        async fn stop(&self, turn_id: &str) -> Result<(), AcpError> {
            self.manager
                .stop_shared_turn(
                    &self.db,
                    SharedStopRequest {
                        guard: self.guard.clone(),
                        turn_id: turn_id.to_string(),
                    },
                )
                .await
        }

        async fn active_turn(&self) -> Option<SharedActiveTurnProjection> {
            self.manager
                .shared_session_broker()
                .diagnostic_for_connection(&self.attachment.connection_id)
                .await
                .and_then(|snapshot| snapshot.active_turn)
        }

        fn cancel_call_count(&self) -> usize {
            self.adapter.cancel_calls.load(Ordering::SeqCst)
        }

        fn interaction_call_count(&self) -> usize {
            match self.interaction_kind.expect("fixture has an interaction") {
                SharedInteractionKind::Permission => {
                    self.adapter.permission_calls.load(Ordering::SeqCst)
                }
                SharedInteractionKind::Question => {
                    self.adapter.question_calls.load(Ordering::SeqCst)
                }
                SharedInteractionKind::PlanApproval => {
                    self.adapter.plan_approval_calls.load(Ordering::SeqCst)
                }
            }
        }

        async fn fail_next_cancel_before_channel_send(&self) {
            self.adapter.cancel_failures.lock().await.push_back(
                SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::ProcessExited),
            );
        }

        async fn fail_next_cancel_after_possible_admission(&self) {
            self.adapter.cancel_failures.lock().await.push_back(
                SharedControlAdmissionError::MayHaveBeenAdmitted(AcpError::ProcessExited),
            );
        }

        async fn fail_next_interaction_before_admission(&self) {
            self.adapter.interaction_failures.lock().await.push_back(
                SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::ProcessExited),
            );
        }

        async fn fail_next_interaction_after_possible_admission(&self) {
            self.adapter.interaction_failures.lock().await.push_back(
                SharedControlAdmissionError::MayHaveBeenAdmitted(AcpError::ProcessExited),
            );
        }

        async fn fail_next_interaction_as_already_resolved(&self) {
            self.adapter.interaction_failures.lock().await.push_back(
                SharedControlAdmissionError::InteractionAlreadyResolved { local_error: None },
            );
        }
    }

    async fn ready_fixture_with_turn(turn_id: &str) -> SharedControlFixture {
        let adapter = Arc::new(FakeSharedControlAdapter::default());
        let manager = ConnectionManager::new_with_shared_control_adapter(adapter.clone());
        let broker = manager.shared_session_broker();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(1701),
                "control-connection",
                "control-client",
                "control-connect",
            ))
            .await
            .unwrap();
        let attachment = reservation.attachment;
        manager
            .install_test_shared_connection(&attachment, Some(1701))
            .await
            .unwrap();
        broker
            .mark_ready(
                &attachment.connection_id,
                attachment.generation,
                "test-driver-1",
            )
            .await
            .unwrap();
        let guard = SharedMutationGuard {
            connection_id: attachment.connection_id.clone(),
            generation: attachment.generation,
            lease_id: attachment.lease_id.clone(),
        };
        let admission = broker
            .enqueue_prompt(SharedPromptRequest {
                guard: guard.clone(),
                client_instance_id: "control-client".into(),
                client_request_id: "control-prompt".into(),
                blocks: vec![crate::acp::types::PromptInputBlock::Text {
                    text: "control prompt".into(),
                }],
                folder_id: Some(1),
                conversation_id: Some(1701),
                client_message_id: "control-message".into(),
                capture: None,
                submitted_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        assert!(broker
            .mark_prompt_admission_published(
                &attachment.connection_id,
                attachment.generation,
                &admission.queue_item_id,
            )
            .await
            .unwrap());
        assert!(matches!(
            broker
                .claim_dispatchable_head(
                    &attachment.connection_id,
                    attachment.generation,
                    turn_id,
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        SharedControlFixture {
            manager,
            adapter,
            attachment,
            guard,
            db: Database::connect("sqlite::memory:").await.unwrap(),
            interaction_kind: None,
        }
    }

    async fn ready_fixture_with_interaction(
        kind: SharedInteractionKind,
        interaction_id: &str,
    ) -> SharedControlFixture {
        let mut fixture = ready_fixture_with_turn("interaction-turn").await;
        fixture
            .manager
            .shared_session_broker()
            .observe_interaction(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                kind,
                interaction_id,
            )
            .await
            .unwrap();
        fixture.interaction_kind = Some(kind);
        fixture
    }

    fn is_interaction_loser(result: &Result<(), AcpError>) -> bool {
        matches!(
            result,
            Err(AcpError::Shared(
                SharedSessionError::InteractionAlreadyResolved
            ))
        )
    }

    async fn assert_two_answers_have_one_winner(kind: SharedInteractionKind, id: &str) {
        let fixture = ready_fixture_with_interaction(kind, id).await;
        let (a, b) = tokio::join!(fixture.claim(id), fixture.claim(id));
        assert_eq!(
            [a.is_ok(), b.is_ok()]
                .into_iter()
                .filter(|won| *won)
                .count(),
            1
        );
        assert!(is_interaction_loser(&a) || is_interaction_loser(&b));
        assert_eq!(fixture.interaction_call_count(), 1);
        let metrics = fixture
            .manager
            .shared_session_broker()
            .metrics()
            .snapshot();
        assert_eq!(metrics.interaction_winner_total, 1);
        assert_eq!(metrics.interaction_stale_total, 1);
    }

    #[tokio::test]
    async fn two_permission_answers_have_one_winner() {
        assert_two_answers_have_one_winner(SharedInteractionKind::Permission, "perm-1").await;
    }

    #[tokio::test]
    async fn two_question_answers_have_one_winner() {
        assert_two_answers_have_one_winner(SharedInteractionKind::Question, "question-1").await;
    }

    #[tokio::test]
    async fn two_plan_approval_answers_have_one_winner() {
        assert_two_answers_have_one_winner(SharedInteractionKind::PlanApproval, "plan-approval-1")
            .await;
    }

    #[tokio::test]
    async fn resolved_event_does_not_erase_the_admitted_interaction_winner_metric() {
        let fixture = ready_fixture_with_interaction(
            SharedInteractionKind::Question,
            "question-resolved-race",
        )
        .await;
        let broker = fixture.manager.shared_session_broker();
        let claim = broker
            .claim_interaction(
                &fixture.guard,
                SharedInteractionKind::Question,
                "question-resolved-race",
            )
            .await
            .unwrap();
        broker
            .observe_interaction_resolved(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                SharedInteractionKind::Question,
                "question-resolved-race",
            )
            .await
            .unwrap();

        broker.complete_interaction(&claim).await.unwrap();

        let metrics = broker.metrics().snapshot();
        assert_eq!(metrics.interaction_winner_total, 1);
        assert_eq!(metrics.interaction_stale_total, 0);
    }

    #[tokio::test]
    async fn downstream_already_resolved_interaction_counts_stale_not_winner() {
        let fixture = ready_fixture_with_interaction(
            SharedInteractionKind::PlanApproval,
            "plan-downstream-stale",
        )
        .await;
        fixture.fail_next_interaction_as_already_resolved().await;

        assert!(is_interaction_loser(
            &fixture.claim("plan-downstream-stale").await
        ));

        let metrics = fixture
            .manager
            .shared_session_broker()
            .metrics()
            .snapshot();
        assert_eq!(metrics.interaction_winner_total, 0);
        assert_eq!(metrics.interaction_stale_total, 1);
    }

    #[tokio::test]
    async fn duplicate_interaction_observation_cannot_reopen_a_resolving_claim() {
        let fixture = ready_fixture_with_interaction(
            SharedInteractionKind::Permission,
            "permission-duplicate",
        )
        .await;
        let broker = fixture.manager.shared_session_broker();
        let claim = broker
            .claim_interaction(
                &fixture.guard,
                SharedInteractionKind::Permission,
                "permission-duplicate",
            )
            .await
            .unwrap();
        broker
            .observe_interaction(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                SharedInteractionKind::Permission,
                "permission-duplicate",
            )
            .await
            .unwrap();

        assert!(matches!(
            broker
                .claim_interaction(
                    &fixture.guard,
                    SharedInteractionKind::Permission,
                    "permission-duplicate",
                )
                .await,
            Err(SharedSessionError::InteractionAlreadyResolved)
        ));
        broker.complete_interaction(&claim).await.unwrap();
    }

    #[test]
    fn event_sequence_fence_rejects_older_interaction_observations_after_snapshot() {
        let mut interactions = SharedInteractions::default();
        let mut snapshot = dispatchable_runtime_snapshot();
        snapshot.event_seq = 42;
        snapshot.pending_question_id = Some("question-current".into());
        interactions.reconcile_snapshot(&snapshot);

        interactions.set_pending(SharedInteractionKind::Question, "question-stale", 40);
        interactions.resolve_matching(SharedInteractionKind::Question, "question-current", 41);

        let current = interactions
            .question
            .as_ref()
            .expect("newer snapshot interaction remains present");
        assert_eq!(current.id, "question-current");
        assert!(current.admission == InteractionAdmissionState::Pending);
        assert_eq!(interactions.last_observed_event_seq, 42);
    }

    #[tokio::test]
    async fn turn_settlement_sequence_fence_rejects_older_interaction_snapshot() {
        let fixture = ready_fixture_with_turn("sequence-fenced-turn").await;
        let broker = fixture.manager.shared_session_broker();
        broker
            .settle_active_turn_at_seq(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                "end_turn",
                51,
            )
            .await
            .unwrap();

        let mut stale_snapshot = dispatchable_runtime_snapshot();
        stale_snapshot.event_seq = 50;
        stale_snapshot.pending_question_id = Some("question-from-completed-turn".into());
        broker
            .reconcile_runtime_snapshot(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                &stale_snapshot,
            )
            .await
            .unwrap();

        assert!(matches!(
            broker
                .claim_interaction(
                    &fixture.guard,
                    SharedInteractionKind::Question,
                    "question-from-completed-turn",
                )
                .await,
            Err(SharedSessionError::InteractionAlreadyResolved)
        ));
    }

    #[tokio::test]
    async fn resolved_event_only_resolves_the_matching_interaction_kind() {
        let fixture = ready_fixture_with_turn("interaction-kind-turn").await;
        let broker = fixture.manager.shared_session_broker();
        for kind in [
            SharedInteractionKind::Permission,
            SharedInteractionKind::Question,
        ] {
            broker
                .observe_interaction(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "test-driver-1",
                    kind,
                    "same-interaction-id",
                )
                .await
                .unwrap();
        }

        broker
            .observe_interaction_resolved(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                SharedInteractionKind::Question,
                "same-interaction-id",
            )
            .await
            .unwrap();

        let permission_claim = broker
            .claim_interaction(
                &fixture.guard,
                SharedInteractionKind::Permission,
                "same-interaction-id",
            )
            .await
            .expect("permission with the same id remains pending");
        assert!(matches!(
            broker
                .claim_interaction(
                    &fixture.guard,
                    SharedInteractionKind::Question,
                    "same-interaction-id",
                )
                .await,
            Err(SharedSessionError::InteractionAlreadyResolved)
        ));
        broker
            .complete_interaction(&permission_claim)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stale_turn_never_cancels_newer_turn_and_exact_stop_is_idempotent() {
        let fixture = ready_fixture_with_turn("turn-new").await;
        assert!(matches!(
            fixture.stop("turn-old").await,
            Err(AcpError::Shared(SharedSessionError::StaleTurn))
        ));
        let (a, b) = tokio::join!(fixture.stop("turn-new"), fixture.stop("turn-new"));
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(fixture.cancel_call_count(), 1);
        assert_eq!(
            fixture
                .manager
                .shared_session_broker()
                .metrics()
                .snapshot()
                .stale_stop_total,
            1
        );
    }

    #[tokio::test]
    async fn stop_claim_is_revalidated_immediately_before_cancel_admission() {
        let fixture = ready_fixture_with_turn("turn-old").await;
        let request = SharedStopRequest {
            guard: fixture.guard.clone(),
            turn_id: "turn-old".into(),
        };
        let SharedStopClaimDecision::Claimed(claim) = fixture
            .manager
            .shared_session_broker()
            .claim_stop_request(&request)
            .await
            .unwrap()
        else {
            panic!("first exact stop must own the admission claim");
        };
        fixture
            .manager
            .shared_session_broker()
            .settle_active_turn(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                "end_turn",
            )
            .await
            .unwrap();

        assert!(matches!(
            fixture
                .manager
                .shared_session_broker()
                .validate_stop_claim(&claim)
                .await,
            Err(SharedSessionError::StaleTurn)
        ));
    }

    #[tokio::test]
    async fn definite_cancel_admission_failure_releases_stop_claim_for_retry() {
        let fixture = ready_fixture_with_turn("turn-new").await;
        fixture.fail_next_cancel_before_channel_send().await;
        assert!(fixture.stop("turn-new").await.is_err());
        assert!(!fixture.active_turn().await.unwrap().stop_requested);
        fixture.stop("turn-new").await.unwrap();
        assert_eq!(fixture.cancel_call_count(), 2);
    }

    #[tokio::test]
    async fn ambiguous_cancel_failure_keeps_stop_quarantined_without_retry() {
        let fixture = ready_fixture_with_turn("turn-new").await;
        fixture.fail_next_cancel_after_possible_admission().await;
        assert!(fixture.stop("turn-new").await.is_err());
        assert!(fixture.active_turn().await.unwrap().stop_requested);
        fixture.stop("turn-new").await.unwrap();
        assert_eq!(fixture.cancel_call_count(), 1);
    }

    #[tokio::test]
    async fn definite_interaction_admission_failure_releases_claim_for_retry() {
        let fixture =
            ready_fixture_with_interaction(SharedInteractionKind::Permission, "permission-retry")
                .await;
        fixture.fail_next_interaction_before_admission().await;
        assert!(fixture.claim("permission-retry").await.is_err());
        fixture.claim("permission-retry").await.unwrap();
        assert_eq!(fixture.interaction_call_count(), 2);
    }

    #[tokio::test]
    async fn ambiguous_interaction_failure_keeps_claim_resolved() {
        let fixture = ready_fixture_with_interaction(
            SharedInteractionKind::Permission,
            "permission-ambiguous",
        )
        .await;
        fixture
            .fail_next_interaction_after_possible_admission()
            .await;
        assert!(fixture.claim("permission-ambiguous").await.is_err());
        assert!(is_interaction_loser(
            &fixture.claim("permission-ambiguous").await
        ));
        assert_eq!(fixture.interaction_call_count(), 1);
    }

    #[tokio::test]
    async fn stopped_turn_interaction_cannot_claim_the_next_turn_interaction() {
        let fixture =
            ready_fixture_with_interaction(SharedInteractionKind::Permission, "permission-old")
                .await;
        fixture.stop("interaction-turn").await.unwrap();
        fixture
            .manager
            .shared_session_broker()
            .settle_active_turn(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                "cancelled",
            )
            .await
            .unwrap();

        let broker = fixture.manager.shared_session_broker();
        let admission = broker
            .enqueue_prompt(SharedPromptRequest {
                guard: fixture.guard.clone(),
                client_instance_id: "control-client".into(),
                client_request_id: "next-prompt".into(),
                blocks: vec![crate::acp::types::PromptInputBlock::Text {
                    text: "next prompt".into(),
                }],
                folder_id: Some(1),
                conversation_id: Some(1701),
                client_message_id: "next-message".into(),
                capture: None,
                submitted_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        broker
            .mark_prompt_admission_published(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                &admission.queue_item_id,
            )
            .await
            .unwrap();
        assert!(matches!(
            broker
                .claim_dispatchable_head(
                    &fixture.attachment.connection_id,
                    fixture.attachment.generation,
                    "next-turn",
                    &dispatchable_runtime_snapshot(),
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        broker
            .observe_interaction(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "test-driver-1",
                SharedInteractionKind::Permission,
                "permission-next",
            )
            .await
            .unwrap();

        assert!(is_interaction_loser(&fixture.claim("permission-old").await));
        fixture.claim("permission-next").await.unwrap();
        assert_eq!(fixture.interaction_call_count(), 1);
    }

    #[derive(Clone, Copy, Debug)]
    enum IdleBlockerCase {
        Lease,
        ActiveTurn { stop_requested: bool },
        Permission,
        Question,
        PlanApproval,
        QueuedPrompt,
        ContinuationWait,
        ActiveDelegation,
        BackgroundWork,
        HostWork,
        NonReadyPhase,
        NonConnectedStatus,
    }

    struct IdleBlockerHandle {
        lease: Option<SharedMutationGuard>,
        queue_item_id: Option<String>,
        host_work: Option<SharedHostWorkPermit>,
    }

    impl IdleBlockerHandle {
        fn empty() -> Self {
            Self {
                lease: None,
                queue_item_id: None,
                host_work: None,
            }
        }
    }

    struct IdleReadyFixture {
        manager: ConnectionManager,
        broker: SharedSessionBroker,
        attachment: SharedSessionAttachment,
        state: Arc<RwLock<SessionState>>,
        key: SharedSessionKey,
        conversation_id: i32,
        next_client: AtomicUsize,
    }

    impl IdleReadyFixture {
        async fn new(conversation_id: i32, ready: bool) -> Self {
            let manager = ConnectionManager::new();
            manager.configure_shared_client_lease_ttl(Duration::from_secs(3_600));
            let broker = manager.shared_session_broker();
            let key = SharedSessionKey::Conversation(conversation_id);
            let attachment = broker
                .reserve_or_attach(request(
                    key.clone(),
                    &format!("idle-connection-{conversation_id}"),
                    "idle-client-0",
                    "idle-connect-0",
                ))
                .await
                .unwrap()
                .attachment;
            manager
                .insert_test_connection(
                    &attachment.connection_id,
                    crate::models::agent::AgentType::Codex,
                    None,
                    EventEmitter::Noop,
                )
                .await;
            let (state, emitter, child_pid, driver_incarnation) = {
                let connections = manager.connections.lock().await;
                let connection = connections
                    .get(&attachment.connection_id)
                    .expect("synthetic manager connection is registered");
                (
                    connection.state.clone(),
                    connection.emitter.clone(),
                    connection.child_pid.clone(),
                    connection.connection_incarnation.clone(),
                )
            };
            {
                let mut state = state.write().await;
                state.conversation_id = Some(conversation_id);
                state.status = crate::acp::types::ConnectionStatus::Connecting;
                state.shared_session = Some(SharedSessionProjection {
                    generation: attachment.generation,
                    phase: SharedSessionPhase::Bootstrapping,
                    queue: Vec::new(),
                    active_turn: None,
                    lease_expires_at: Some(attachment.lease_expires_at),
                    expired_lease_tombstone_count: 0,
                });
            }
            broker
                .install_registered(
                    &attachment.connection_id,
                    attachment.generation,
                    driver_incarnation.clone(),
                    state.clone(),
                    emitter,
                    child_pid,
                )
                .await
                .unwrap();
            if ready {
                broker
                    .mark_ready(
                        &attachment.connection_id,
                        attachment.generation,
                        &driver_incarnation,
                    )
                    .await
                    .unwrap();
            } else {
                state
                    .write()
                    .await
                    .apply_event(&crate::acp::types::AcpEvent::StatusChanged {
                        status: crate::acp::types::ConnectionStatus::Connected,
                    });
            }
            broker
                .release_lease(&SharedMutationGuard {
                    connection_id: attachment.connection_id.clone(),
                    generation: attachment.generation,
                    lease_id: attachment.lease_id.clone(),
                })
                .await
                .unwrap();
            Self {
                manager,
                broker,
                attachment,
                state,
                key,
                conversation_id,
                next_client: AtomicUsize::new(1),
            }
        }

        async fn driver_incarnation(&self) -> String {
            self.manager
                .connections
                .lock()
                .await
                .get(&self.attachment.connection_id)
                .expect("manager connection remains registered")
                .connection_incarnation
                .clone()
        }

        async fn attach_new_lease(
            &self,
        ) -> Result<SharedReserveOutcome, SharedSessionError> {
            let client = self.next_client.fetch_add(1, Ordering::SeqCst);
            self.broker
                .reserve_or_attach(request(
                    self.key.clone(),
                    "ignored-idle-candidate",
                    &format!("idle-client-{client}"),
                    &format!("idle-connect-{client}"),
                ))
                .await
        }

        async fn mutation_guard(&self) -> SharedMutationGuard {
            let attachment = self.attach_new_lease().await.unwrap().attachment;
            SharedMutationGuard {
                connection_id: attachment.connection_id,
                generation: attachment.generation,
                lease_id: attachment.lease_id,
            }
        }

        async fn reap_now(&self) -> SharedSweepReport {
            self.broker
                .expire_leases(tokio::time::Instant::now())
                .await;
            let shared = self
                .manager
                .sweep_shared_sessions(Some(Duration::from_secs(900)), Duration::from_secs(90))
                .await;
            if self
                .manager
                .get_state(&self.attachment.connection_id)
                .await
                .is_some()
            {
                self.state.write().await.last_activity_at =
                    chrono::Utc::now() - chrono::Duration::seconds(901);
            }
            assert_eq!(self.manager.sweep_idle(Duration::from_secs(900)).await, 0);
            shared
        }

        async fn connection_still_registered(&self) -> bool {
            self.broker
                .diagnostic_for_connection(&self.attachment.connection_id)
                .await
                .is_some()
                && self
                    .manager
                    .get_state(&self.attachment.connection_id)
                    .await
                    .is_some()
        }

        async fn apply_event(&self, event: crate::acp::types::AcpEvent) {
            self.state.write().await.apply_event(&event);
        }

        async fn enqueue_blocker(&self, dispatch: bool) -> (SharedMutationGuard, String) {
            let guard = self.mutation_guard().await;
            let client = self.next_client.fetch_add(1, Ordering::SeqCst);
            let admission = self
                .broker
                .enqueue_prompt(SharedPromptRequest {
                    guard: guard.clone(),
                    client_instance_id: format!("idle-prompt-client-{client}"),
                    client_request_id: format!("idle-prompt-request-{client}"),
                    blocks: vec![crate::acp::types::PromptInputBlock::Text {
                        text: "idle blocker".into(),
                    }],
                    folder_id: Some(1),
                    conversation_id: Some(self.conversation_id),
                    client_message_id: format!("idle-message-{client}"),
                    capture: None,
                    submitted_at: chrono::Utc::now(),
                })
                .await
                .unwrap();
            assert!(self
                .broker
                .mark_prompt_admission_published(
                    &self.attachment.connection_id,
                    self.attachment.generation,
                    &admission.queue_item_id,
                )
                .await
                .unwrap());
            if dispatch {
                assert!(matches!(
                    self.broker
                        .claim_dispatchable_head(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            "idle-turn",
                            &dispatchable_runtime_snapshot(),
                        )
                        .await
                        .unwrap(),
                    DispatchHeadDecision::Claimed(_)
                ));
            }
            (guard, admission.queue_item_id)
        }
    }

    impl IdleReadyFixture {
        async fn enable(&self, blocker: IdleBlockerCase) -> IdleBlockerHandle {
            let mut handle = IdleBlockerHandle::empty();
            match blocker {
                IdleBlockerCase::Lease => handle.lease = Some(self.mutation_guard().await),
                IdleBlockerCase::ActiveTurn { stop_requested } => {
                    let (guard, _) = self.enqueue_blocker(true).await;
                    if stop_requested {
                        let claim = match self
                            .broker
                            .claim_stop_request(&SharedStopRequest {
                                guard: guard.clone(),
                                turn_id: "idle-turn".into(),
                            })
                            .await
                            .unwrap()
                        {
                            SharedStopClaimDecision::Claimed(claim) => claim,
                            _ => panic!("fresh idle stop request must be claimed"),
                        };
                        self.broker.complete_stop_request(&claim).await.unwrap();
                    }
                    self.broker.release_lease(&guard).await.unwrap();
                }
                IdleBlockerCase::Permission
                | IdleBlockerCase::Question
                | IdleBlockerCase::PlanApproval => {
                    let kind = match blocker {
                        IdleBlockerCase::Permission => SharedInteractionKind::Permission,
                        IdleBlockerCase::Question => SharedInteractionKind::Question,
                        IdleBlockerCase::PlanApproval => SharedInteractionKind::PlanApproval,
                        _ => unreachable!(),
                    };
                    self.broker
                        .observe_interaction(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            &self.driver_incarnation().await,
                            kind,
                            "idle-interaction",
                        )
                        .await
                        .unwrap();
                }
                IdleBlockerCase::QueuedPrompt => {
                    let (guard, queue_item_id) = self.enqueue_blocker(false).await;
                    self.broker.release_lease(&guard).await.unwrap();
                    handle.queue_item_id = Some(queue_item_id);
                }
                IdleBlockerCase::ContinuationWait => {
                    use crate::acp::delegation::continuation::types::{
                        ContinuationState, ContinuationWaitingProjection,
                    };
                    let now = chrono::Utc::now();
                    self.apply_event(crate::acp::types::AcpEvent::ContinuationWaitingChanged {
                        conversation_id: self.conversation_id,
                        waiting: Some(ContinuationWaitingProjection {
                            conversation_id: self.conversation_id,
                            state: ContinuationState::Waiting,
                            generation: 1,
                            armed_at: now,
                            wake_at: now + chrono::Duration::minutes(10),
                        }),
                    })
                    .await;
                }
                IdleBlockerCase::ActiveDelegation => {
                    let now = chrono::Utc::now();
                    self.apply_event(crate::acp::types::AcpEvent::DelegationStarted {
                        parent_connection_id: self.attachment.connection_id.clone(),
                        parent_tool_use_id: "idle-parent-tool".into(),
                        child_connection_id: "idle-child".into(),
                        child_conversation_id: self.conversation_id + 10_000,
                        agent_type: crate::models::agent::AgentType::Codex,
                        task_preview: "idle task".into(),
                        task_id: "idle-task".into(),
                        started_at: now,
                        runtime_stats:
                            crate::acp::delegation::runtime_stats::DelegationRuntimeStats::empty(
                                now,
                            ),
                        attention_request: None,
                    })
                    .await;
                }
                IdleBlockerCase::BackgroundWork => {
                    self.apply_event(crate::acp::types::AcpEvent::BackgroundActivity {
                        session_id: "idle-session".into(),
                        turns: Vec::new(),
                        outstanding: 1,
                        settled: Vec::new(),
                        watermark: 0,
                    })
                    .await;
                }
                IdleBlockerCase::HostWork => {
                    handle.host_work = Some(
                        self.broker
                            .begin_host_work(
                                &self.attachment.connection_id,
                                self.attachment.generation,
                            )
                            .await
                            .unwrap(),
                    );
                }
                IdleBlockerCase::NonReadyPhase => {}
                IdleBlockerCase::NonConnectedStatus => {
                    self.apply_event(crate::acp::types::AcpEvent::StatusChanged {
                        status: crate::acp::types::ConnectionStatus::Prompting,
                    })
                    .await;
                }
            }
            handle
        }

        async fn clear(&self, blocker: IdleBlockerCase, mut handle: IdleBlockerHandle) {
            match blocker {
                IdleBlockerCase::Lease => {
                    self.broker
                        .release_lease(handle.lease.as_ref().unwrap())
                        .await
                        .unwrap();
                }
                IdleBlockerCase::ActiveTurn { .. } => {
                    self.broker
                        .settle_active_turn(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            &self.driver_incarnation().await,
                            "end_turn",
                        )
                        .await
                        .unwrap();
                }
                IdleBlockerCase::Permission
                | IdleBlockerCase::Question
                | IdleBlockerCase::PlanApproval => {
                    self.broker
                        .reconcile_runtime_snapshot(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            &self.driver_incarnation().await,
                            &dispatchable_runtime_snapshot(),
                        )
                        .await
                        .unwrap();
                }
                IdleBlockerCase::QueuedPrompt => {
                    let guard = self.mutation_guard().await;
                    self.broker
                        .cancel_queued_prompt(&guard, handle.queue_item_id.as_deref().unwrap())
                        .await
                        .unwrap();
                    self.broker.release_lease(&guard).await.unwrap();
                }
                IdleBlockerCase::ContinuationWait => {
                    self.apply_event(crate::acp::types::AcpEvent::ContinuationWaitingChanged {
                        conversation_id: self.conversation_id,
                        waiting: None,
                    })
                    .await;
                }
                IdleBlockerCase::ActiveDelegation => {
                    let now = chrono::Utc::now();
                    self.apply_event(crate::acp::types::AcpEvent::DelegationCompleted {
                        parent_connection_id: self.attachment.connection_id.clone(),
                        parent_tool_use_id: "idle-parent-tool".into(),
                        child_connection_id: "idle-child".into(),
                        child_conversation_id: self.conversation_id + 10_000,
                        agent_type: crate::models::agent::AgentType::Codex,
                        task_id: "idle-task".into(),
                        runtime_stats:
                            crate::acp::delegation::runtime_stats::DelegationRuntimeStats::empty(
                                now,
                            ),
                        result: crate::acp::types::DelegationResultSummary::Ok {
                            duration_ms: 1,
                            text_preview: None,
                        },
                        card_summary: None,
                    })
                    .await;
                }
                IdleBlockerCase::BackgroundWork => {
                    self.apply_event(crate::acp::types::AcpEvent::BackgroundActivity {
                        session_id: "idle-session".into(),
                        turns: Vec::new(),
                        outstanding: 0,
                        settled: Vec::new(),
                        watermark: 1,
                    })
                    .await;
                }
                IdleBlockerCase::HostWork => {
                    self.broker
                        .end_host_work(handle.host_work.take().unwrap())
                        .await;
                }
                IdleBlockerCase::NonReadyPhase => {
                    self.broker
                        .mark_ready(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            &self.driver_incarnation().await,
                        )
                        .await
                        .unwrap();
                }
                IdleBlockerCase::NonConnectedStatus => {
                    self.apply_event(crate::acp::types::AcpEvent::StatusChanged {
                        status: crate::acp::types::ConnectionStatus::Connected,
                    })
                    .await;
                }
            }
        }
    }

    async fn idle_ready_fixture() -> IdleReadyFixture {
        IdleReadyFixture::new(2_001, true).await
    }

    #[tokio::test(start_paused = true)]
    async fn idle_blocker_matrix_resets_the_full_grace() {
        let cases = [
            IdleBlockerCase::Lease,
            IdleBlockerCase::ActiveTurn {
                stop_requested: false,
            },
            IdleBlockerCase::ActiveTurn {
                stop_requested: true,
            },
            IdleBlockerCase::Permission,
            IdleBlockerCase::Question,
            IdleBlockerCase::PlanApproval,
            IdleBlockerCase::QueuedPrompt,
            IdleBlockerCase::ContinuationWait,
            IdleBlockerCase::ActiveDelegation,
            IdleBlockerCase::BackgroundWork,
            IdleBlockerCase::HostWork,
            IdleBlockerCase::NonReadyPhase,
            IdleBlockerCase::NonConnectedStatus,
        ];

        for (offset, blocker) in cases.into_iter().enumerate() {
            let fixture = IdleReadyFixture::new(
                2_100 + i32::try_from(offset).unwrap(),
                !matches!(blocker, IdleBlockerCase::NonReadyPhase),
            )
            .await;
            assert!(!fixture.reap_now().await.removed, "{blocker:?}");
            let handle = fixture.enable(blocker).await;
            tokio::time::advance(Duration::from_secs(901)).await;
            assert!(!fixture.reap_now().await.removed, "{blocker:?}");

            fixture.clear(blocker, handle).await;
            assert!(!fixture.reap_now().await.removed, "{blocker:?}");
            tokio::time::advance(Duration::from_secs(899)).await;
            assert!(!fixture.reap_now().await.removed, "{blocker:?}");
            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(fixture.reap_now().await.removed, "{blocker:?}");
            assert!(!fixture.connection_still_registered().await, "{blocker:?}");
            assert_eq!(fixture.manager.shared_teardown_count_for_test(), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn attach_racing_final_reclaim_has_one_winner() {
        let fixture = idle_ready_fixture().await;
        assert!(!fixture.reap_now().await.removed);
        tokio::time::advance(Duration::from_secs(900)).await;
        fixture.broker.install_idle_final_cas_barrier_for_test(2);
        let (attach, reap) = tokio::join!(fixture.attach_new_lease(), fixture.reap_now());
        assert_ne!(attach.is_ok(), reap.removed);
        if attach.is_ok() {
            assert!(fixture.connection_still_registered().await);
            assert_eq!(fixture.broker.metrics().snapshot().idle_cas_lost_total, 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn idle_host_work_end_drop_stale_generation_and_dispatcher_wake() {
        let fixture = idle_ready_fixture().await;
        let subscription = fixture
            .broker
            .runtime_subscription(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();

        let permit = fixture
            .broker
            .begin_host_work(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();
        let duplicate_identity = permit.identity.clone().unwrap();
        tokio::time::timeout(Duration::from_millis(50), subscription.notify.notified())
            .await
            .expect("begin host work wakes dispatcher");
        assert!(fixture.broker.end_host_work(permit).await);
        assert!(!release_host_work(fixture.broker.index.clone(), duplicate_identity).await);
        tokio::time::timeout(Duration::from_millis(50), subscription.notify.notified())
            .await
            .expect("end host work wakes dispatcher");

        let dropped = fixture
            .broker
            .begin_host_work(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(50), subscription.notify.notified())
            .await
            .expect("second begin host work wakes dispatcher");
        drop(dropped);
        tokio::task::yield_now().await;
        tokio::time::timeout(Duration::from_millis(50), subscription.notify.notified())
            .await
            .expect("dropped host work wakes dispatcher");

        let stale = fixture
            .broker
            .begin_host_work(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();
        fixture
            .broker
            .mark_failed(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
                "session_unavailable",
                true,
            )
            .await
            .unwrap();
        let mut retry = request(
            fixture.key.clone(),
            "idle-replacement",
            "idle-replacement-client",
            "idle-replacement-request",
        );
        retry.retry_failed_generation = Some(fixture.attachment.generation);
        let replacement = fixture.broker.reserve_or_attach(retry).await.unwrap();
        assert_eq!(replacement.attachment.generation, 2);
        assert!(!fixture.broker.end_host_work(stale).await);
        assert_eq!(
            fixture
                .broker
                .diagnostic_for_connection(&replacement.attachment.connection_id)
                .await
                .unwrap()
                .generation,
            2
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_host_work_cross_thread_drop_releases_and_wakes_dispatcher() {
        let fixture = idle_ready_fixture().await;
        assert!(!fixture.reap_now().await.removed);
        let subscription = fixture
            .broker
            .runtime_subscription(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();
        let permit = fixture
            .broker
            .begin_host_work(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), subscription.notify.notified())
            .await
            .expect("begin host work wakes dispatcher");

        std::thread::spawn(move || drop(permit)).join().unwrap();
        tokio::time::timeout(Duration::from_secs(1), subscription.notify.notified())
            .await
            .expect("cross-thread permit drop wakes dispatcher");

        assert!(!fixture.reap_now().await.removed);
        tokio::time::advance(Duration::from_secs(900)).await;
        assert!(fixture.reap_now().await.removed);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_tombstone_reaps_only_after_cleanup_clients_and_grace() {
        let manager = ConnectionManager::new();
        manager.configure_shared_client_lease_ttl(Duration::from_secs(90));
        let broker = manager.shared_session_broker();
        let key = SharedSessionKey::Conversation(2_500);
        let attachment = broker
            .reserve_or_attach(request(
                key.clone(),
                "failed-idle-connection",
                "failed-idle-client",
                "failed-idle-connect",
            ))
            .await
            .unwrap()
            .attachment;
        broker
            .mark_failed(
                &attachment.connection_id,
                attachment.generation,
                "session_unavailable",
                false,
            )
            .await
            .unwrap();
        assert!(broker
            .release_lease(&SharedMutationGuard {
                connection_id: attachment.connection_id.clone(),
                generation: attachment.generation,
                lease_id: attachment.lease_id.clone(),
            })
            .await
            .unwrap());
        let observer = broker
            .reserve_or_attach(request(
                key.clone(),
                "failed-idle-observer-ignored",
                "failed-idle-observer",
                "failed-idle-observe",
            ))
            .await
            .unwrap()
            .attachment;
        assert!(matches!(
            observer.phase,
            SharedSessionPhase::Failed {
                cleanup_complete: false,
                ..
            }
        ));
        assert!(!manager
            .sweep_shared_sessions(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .removed);

        broker
            .mark_cleanup_complete(&attachment.connection_id, attachment.generation)
            .await
            .unwrap();
        assert!(broker
            .validate_and_bind_lease(
                &observer.connection_id,
                Some(observer.generation),
                Some(&observer.lease_id),
            )
            .await
            .is_ok());
        assert!(!manager
            .sweep_shared_sessions(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .removed);
        let guard = SharedMutationGuard {
            connection_id: observer.connection_id,
            generation: observer.generation,
            lease_id: observer.lease_id,
        };
        assert!(broker.release_lease(&guard).await.unwrap());
        assert!(!manager
            .sweep_shared_sessions(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .removed);
        tokio::time::advance(Duration::from_secs(89)).await;
        assert!(!manager
            .sweep_shared_sessions(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .removed);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(manager
            .sweep_shared_sessions(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .removed);
        assert!(broker
            .diagnostic_for_connection(&attachment.connection_id)
            .await
            .is_none());

        let recreated = broker
            .reserve_or_attach(request(
                key,
                "failed-idle-new-incarnation",
                "failed-idle-new-client",
                "failed-idle-new-connect",
            ))
            .await
            .unwrap();
        assert_eq!(recreated.attachment.generation, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_tombstone_pointer_cas_never_removes_replacement_generation() {
        let broker = broker_with_ttl(Duration::from_secs(90));
        let key = SharedSessionKey::Conversation(2_501);
        let attachment = broker
            .reserve_or_attach(request(
                key.clone(),
                "failed-cas-old",
                "failed-cas-client",
                "failed-cas-connect",
            ))
            .await
            .unwrap()
            .attachment;
        broker
            .mark_failed(
                &attachment.connection_id,
                attachment.generation,
                "session_unavailable",
                true,
            )
            .await
            .unwrap();
        broker
            .release_lease(&SharedMutationGuard {
                connection_id: attachment.connection_id.clone(),
                generation: attachment.generation,
                lease_id: attachment.lease_id,
            })
            .await
            .unwrap();
        assert!(broker
            .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .is_empty());
        tokio::time::advance(Duration::from_secs(90)).await;
        let candidate = broker
            .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .pop()
            .expect("failed tombstone reached its grace");

        let mut retry = request(
            key,
            "failed-cas-replacement",
            "failed-cas-replacement-client",
            "failed-cas-replacement-connect",
        );
        retry.retry_failed_generation = Some(attachment.generation);
        let replacement = broker.reserve_or_attach(retry).await.unwrap();
        assert!(!broker.remove_sweep_candidate(&candidate).await);
        assert_eq!(
            broker
                .diagnostic_for_connection(&replacement.attachment.connection_id)
                .await
                .unwrap()
                .generation,
            2
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failed_tombstone_candidate_loses_after_attach_and_release() {
        let broker = broker_with_ttl(Duration::from_secs(90));
        let key = SharedSessionKey::Conversation(2_502);
        let attachment = broker
            .reserve_or_attach(request(
                key.clone(),
                "failed-grace-old",
                "failed-grace-client",
                "failed-grace-connect",
            ))
            .await
            .unwrap()
            .attachment;
        broker
            .mark_failed(
                &attachment.connection_id,
                attachment.generation,
                "session_unavailable",
                true,
            )
            .await
            .unwrap();
        broker
            .release_lease(&SharedMutationGuard {
                connection_id: attachment.connection_id.clone(),
                generation: attachment.generation,
                lease_id: attachment.lease_id,
            })
            .await
            .unwrap();
        assert!(broker
            .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .is_empty());
        tokio::time::advance(Duration::from_secs(90)).await;
        let stale_candidate = broker
            .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .pop()
            .expect("failed tombstone reached its first grace");

        let attached = tokio::time::timeout(
            Duration::from_secs(1),
            broker.reserve_or_attach(request(
                key,
                "failed-grace-ignored",
                "failed-grace-new-client",
                "failed-grace-new-connect",
            )),
        )
        .await
        .expect("failed tombstone attach must make bounded progress")
        .unwrap();
        let attached = attached.attachment;
        let released = tokio::time::timeout(
            Duration::from_secs(1),
            broker.release_lease(&SharedMutationGuard {
                connection_id: attached.connection_id,
                generation: attached.generation,
                lease_id: attached.lease_id,
            }),
        )
        .await
        .expect("failed tombstone lease release must make bounded progress")
        .unwrap();
        assert!(released);

        assert!(
            !tokio::time::timeout(
                Duration::from_secs(1),
                broker.remove_sweep_candidate(&stale_candidate),
            )
            .await
            .expect("failed tombstone removal CAS must make bounded progress")
        );
        assert!(broker
            .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .is_empty());
        tokio::time::advance(Duration::from_secs(89)).await;
        assert!(broker
            .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
            .await
            .is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            broker
                .evaluate_idle(Some(Duration::from_secs(900)), Duration::from_secs(90))
                .await
                .len(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_legacy_disconnect_touch_and_explicit_termination_are_fenced() {
        let fixture = idle_ready_fixture().await;
        assert!(!fixture.manager.touch(&fixture.attachment.connection_id).await);
        assert!(matches!(
            fixture
                .manager
                .disconnect_if_owner(
                    &fixture.attachment.connection_id,
                    None,
                    None,
                    None,
                    crate::acp::termination::AcpDisconnectOrigin::ProviderUnmount,
                )
                .await,
            Err(AcpError::Shared(SharedSessionError::ProtocolRequired))
        ));
        assert!(fixture.connection_still_registered().await);

        fixture
            .manager
            .terminate_shared_session(
                &fixture.attachment.connection_id,
                fixture.attachment.generation,
            )
            .await
            .unwrap();
        assert!(!fixture.connection_still_registered().await);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_cleanup_timeout_retains_closing_and_blocks_new_incarnation() {
        let fixture = idle_ready_fixture().await;
        {
            let connections = fixture.manager.connections.lock().await;
            connections
                .get(&fixture.attachment.connection_id)
                .unwrap()
                .child_pid
                .store(42, Ordering::SeqCst);
        }
        assert!(!fixture.reap_now().await.removed);
        tokio::time::advance(Duration::from_secs(900)).await;
        let report = fixture.reap_now().await;
        assert!(!report.removed);
        assert_eq!(report.cleanup_incomplete, 1);
        assert_eq!(
            fixture
                .broker
                .diagnostic_for_connection(&fixture.attachment.connection_id)
                .await
                .unwrap()
                .phase,
            SharedSessionPhase::Closing
        );
        assert!(matches!(
            fixture.attach_new_lease().await,
            Err(SharedSessionError::Closing)
        ));
        assert_eq!(
            fixture.broker.metrics().snapshot().cleanup_incomplete_total,
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_explicit_termination_removes_failed_tombstone_without_public_state() {
        let manager = ConnectionManager::new();
        let broker = manager.shared_session_broker();
        let attachment = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(2_700),
                "failed-explicit-termination",
                "failed-explicit-client",
                "failed-explicit-connect",
            ))
            .await
            .unwrap()
            .attachment;
        broker
            .mark_failed(
                &attachment.connection_id,
                attachment.generation,
                "session_unavailable",
                true,
            )
            .await
            .unwrap();
        manager
            .terminate_shared_session(&attachment.connection_id, attachment.generation)
            .await
            .unwrap();
        assert!(broker
            .diagnostic_for_connection(&attachment.connection_id)
            .await
            .is_none());
    }
}
