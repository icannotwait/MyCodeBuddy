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
        install_test_registration(&broker, &first.attachment.connection_id, 1, Some(8)).await;
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
        broker
            .mark_cleanup_complete(&first.attachment.connection_id, 1)
            .await
            .unwrap();
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
        let (cleanup_state, cleanup_emitter, cleanup_events) = broker
            .mark_cleanup_complete(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
            )
            .await
            .unwrap();
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
        install_test_registration(
            &broker,
            &first.attachment.connection_id,
            1,
            Some(id),
        )
        .await;
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

    async fn install_test_registration(
        broker: &SharedSessionBroker,
        connection_id: &str,
        generation: u64,
        conversation_id: Option<i32>,
    ) {
        broker
            .install_registered(
                connection_id,
                generation,
                "test-driver-incarnation".into(),
                Arc::new(tokio::sync::RwLock::new(SessionState::new(
                    connection_id.into(),
                    crate::models::agent::AgentType::Codex,
                    None,
                    "shared-server".into(),
                    conversation_id,
                ))),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
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
}
