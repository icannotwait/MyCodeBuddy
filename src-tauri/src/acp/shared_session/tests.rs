mod tests {
    use super::*;

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
        broker
            .reserve_or_attach(replacement_request)
            .await
            .unwrap();

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
        SharedSessionBroker {
            lease_ttl: ttl,
            ..SharedSessionBroker::default()
        }
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
            state.shared_session.as_ref().map(|projection| &projection.phase),
            Some(&SharedSessionPhase::Ready)
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
        assert!(broker
            .is_current_bootstrapping_driver(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-new",
            )
            .await);
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
                    self.broker
                        .mark_prompt_admission_published(
                            &self.attachment.connection_id,
                            self.attachment.generation,
                            &admission.queue_item_id,
                        )
                        .await?;
                    admission.notify.notify_one();
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
        prompt_with_ids("prompt-client", &format!("prompt-{n}"), &format!("text-{n}"))
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
        let mut seqs: Vec<_> = results.into_iter().map(|result| result.enqueue_seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=64).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn identical_retry_returns_original_and_changed_payload_conflicts() {
        let fixture = ready_prompt_broker_fixture().await;
        let first_request = with_fixture_guard(
            &fixture,
            prompt_with_ids("prompt-client", "retry", "alpha"),
        );
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
    async fn unpublished_admission_blocks_claim_and_remains_recoverable_by_retry() {
        let fixture = ready_prompt_broker_fixture().await;
        let request = with_fixture_guard(
            &fixture,
            prompt_with_ids("prompt-client", "publish-retry", "alpha"),
        );
        let first = fixture.broker.enqueue_prompt(request.clone()).await.unwrap();

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
        assert_eq!(fixture.snapshot().await.queue[0].queue_item_id, first.queue_item_id);
    }

    #[tokio::test]
    async fn prompt_ledger_capacity_keeps_existing_retry_available() {
        let fixture = ready_prompt_broker_fixture_with_limits(
            2,
            MAX_WAITING_PROMPTS,
            MAX_WAITING_BYTES,
        )
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
        let (cancel, claim) = tokio::join!(
            fixture.cancel(&item.queue_item_id),
            fixture.claim_head()
        );
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
            assert_eq!(fixture.snapshot().await.queue[0].queue_item_id, item.queue_item_id);
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
        assert_eq!(fixture.snapshot().await.queue[0].queue_item_id, second.queue_item_id);
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
        let fixture = ready_prompt_broker_fixture_from_broker(broker_with_ttl(
            Duration::from_secs(90),
        ))
        .await;
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
        assert_eq!(fixture.snapshot().await.queue[0].queue_item_id, second.queue_item_id);

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
}
