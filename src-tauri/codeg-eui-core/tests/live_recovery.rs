use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use codeg_eui_core::{
    snapshot_and_subscribe_observed, InteractionBackend, InteractionFuture, LiveBackend,
    LiveFuture, LiveProjector, Projection, ReceiveOutcome, SharedModel,
};
use codeg_lib::acp::types::PermissionOptionInfo;
use codeg_lib::acp::{AcpEvent, EventEnvelope, SessionState};
use codeg_lib::models::AgentType;
use tokio::sync::RwLock;

fn state(connection_id: &str) -> Arc<RwLock<SessionState>> {
    Arc::new(RwLock::new(SessionState::new(
        connection_id.to_string(),
        AgentType::Codex,
        None,
        "eui-test".to_string(),
        None,
    )))
}

async fn emit(state: &Arc<RwLock<SessionState>>, payload: AcpEvent) {
    let (stream, envelope) = {
        let mut guard = state.write().await;
        let _ = guard.apply_event(&payload);
        guard.event_seq += 1;
        let envelope = Arc::new(EventEnvelope {
            seq: guard.event_seq,
            connection_id: guard.connection_id.clone(),
            payload,
        });
        (guard.event_stream(), envelope)
    };
    stream.send(envelope);
}

#[derive(Clone)]
struct StateBackend {
    state: Arc<RwLock<SessionState>>,
    declines: Arc<AtomicUsize>,
}

impl InteractionBackend for StateBackend {
    fn respond_permission<'a>(
        &'a self,
        _connection_id: &'a str,
        _request_id: &'a str,
        _option_id: &'a str,
    ) -> InteractionFuture<'a> {
        self.declines.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn cancel_active_turn<'a>(&'a self, _connection_id: &'a str) -> InteractionFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn cancel_question<'a>(
        &'a self,
        _connection_id: &'a str,
        _question_id: &'a str,
    ) -> InteractionFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn cancel_plan_approvals_by_parent<'a>(
        &'a self,
        _connection_id: &'a str,
    ) -> InteractionFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

impl LiveBackend for StateBackend {
    fn get_state<'a>(
        &'a self,
        connection_id: &'a str,
    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let matches = state.read().await.connection_id == connection_id;
            matches.then_some(state)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_cannot_miss_event_between_snapshot_and_subscribe() {
    let state = state("atomic");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let attach_state = Arc::clone(&state);
    let attach_entered = Arc::clone(&entered);
    let attach_release = Arc::clone(&release);
    let attach = tokio::spawn(async move {
        snapshot_and_subscribe_observed(&attach_state, move || {
            attach_entered.wait();
            attach_release.wait();
        })
        .await
    });

    entered.wait();
    let writer_attempted = Arc::new(tokio::sync::Notify::new());
    let writer_state = Arc::clone(&state);
    let writer_signal = Arc::clone(&writer_attempted);
    let writer = tokio::spawn(async move {
        writer_signal.notify_one();
        emit(
            &writer_state,
            AcpEvent::ContentDelta {
                text: "hello".to_string(),
                parent_tool_use_id: None,
            },
        )
        .await;
    });
    writer_attempted.notified().await;
    release.wait();

    let mut attach = attach.await.unwrap();
    writer.await.unwrap();

    assert_eq!(attach.snapshot.event_seq, 0);
    let event = attach.receiver.recv().await.expect("subscribed event");
    assert_eq!(event.seq, 1);
}

#[test]
fn sequence_gap_marks_projection_for_authoritative_resync() {
    let state = state("gap");
    let snapshot = state.blocking_read().to_snapshot();
    let mut projection = Projection::default();
    projection.replace_from_snapshot(&snapshot, 10);

    let outcome = projection.apply_envelope(
        &EventEnvelope {
            seq: 2,
            connection_id: "gap".to_string(),
            payload: AcpEvent::ContentDelta {
                text: "must-not-apply".to_string(),
                parent_tool_use_id: None,
            },
        },
        20,
    );

    assert!(outcome.needs_resync());
    assert!(projection.needs_resync);
    assert_eq!(projection.event_seq, 0);
    assert!(projection.live_assistant.is_empty());
}

#[test]
fn user_message_starts_a_new_assistant_generation_and_marker_window() {
    let mut projection = Projection {
        connection_id: "turns".to_string(),
        event_seq: 4,
        live_assistant: "old answer".to_string(),
        assistant_generation: 8,
        transcript_generation: 3,
        t_first_token_ns: 10,
        t_end_ns: 20,
        error_strip: "old error".to_string(),
        ..Projection::default()
    };

    projection.apply_envelope(
        &EventEnvelope {
            seq: 5,
            connection_id: "turns".to_string(),
            payload: AcpEvent::UserMessage {
                message_id: "message-2".to_string(),
                blocks: Vec::new(),
            },
        },
        30,
    );

    assert!(projection.live_assistant.is_empty());
    assert_eq!(projection.t_first_token_ns, 0);
    assert_eq!(projection.t_end_ns, 0);
    assert_eq!(projection.assistant_generation, 9);
    assert_eq!(projection.transcript_generation, 4);
    assert!(projection.error_strip.is_empty());
}

#[test]
fn turn_attempt_rollback_forces_authoritative_recovery() {
    let mut projection = Projection {
        connection_id: "rollback".to_string(),
        live_assistant: "speculative".to_string(),
        stream_active: true,
        ..Projection::default()
    };

    let outcome = projection.apply_envelope(
        &EventEnvelope {
            seq: 1,
            connection_id: "rollback".to_string(),
            payload: AcpEvent::TurnAttemptRollback { attempt: 2 },
        },
        30,
    );

    assert!(outcome.needs_resync());
    assert!(projection.needs_resync);
}

#[test]
fn active_turn_hard_error_sets_terminal_marker_without_connection_death() {
    let mut projection = Projection {
        connection_id: "error".to_string(),
        stream_active: true,
        ..Projection::default()
    };

    projection.apply_envelope(
        &EventEnvelope {
            seq: 1,
            connection_id: "error".to_string(),
            payload: AcpEvent::Error {
                message: "turn failed".to_string(),
                agent_type: "codex".to_string(),
                code: Some("turn_failed".to_string()),
                terminal: false,
            },
        },
        44,
    );

    assert_eq!(projection.t_end_ns, 44);
    assert!(!projection.stream_active);
    assert_eq!(projection.error_strip, "turn failed");
}

#[tokio::test]
async fn snapshot_replacement_coalesces_text_and_reduces_tool_summaries() {
    let state = state("snapshot");
    emit(
        &state,
        AcpEvent::ContentDelta {
            text: "hel".to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    emit(
        &state,
        AcpEvent::ContentDelta {
            text: "lo".to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    emit(
        &state,
        AcpEvent::ToolCall {
            tool_call_id: "tool-1".to_string(),
            title: "Read file".to_string(),
            kind: "read".to_string(),
            status: "in_progress".to_string(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        },
    )
    .await;

    let snapshot = state.read().await.to_snapshot();
    let mut projection = Projection::default();
    projection.replace_from_snapshot(&snapshot, 50);

    assert_eq!(projection.live_assistant, "hello");
    assert_eq!(projection.tools.len(), 1);
    assert_eq!(projection.tools[0].name, "Read file");
    assert_eq!(projection.tools[0].status, "in_progress");
    assert_eq!(projection.event_seq, 3);
    assert_eq!(projection.assistant_generation, 1);
    assert_eq!(projection.transcript_generation, 1);
}

#[tokio::test]
async fn snapshot_hard_error_sets_terminal_marker_after_dropped_event() {
    let state = state("snapshot-error");
    emit(
        &state,
        AcpEvent::UserMessage {
            message_id: "failed-message".to_string(),
            blocks: Vec::new(),
        },
    )
    .await;
    emit(
        &state,
        AcpEvent::ContentDelta {
            text: "partial".to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    emit(
        &state,
        AcpEvent::Error {
            message: "hard failure".to_string(),
            agent_type: "codex".to_string(),
            code: Some("turn_failed".to_string()),
            terminal: false,
        },
    )
    .await;
    let snapshot = state.read().await.to_snapshot();
    let mut projection = Projection::default();

    projection.replace_from_snapshot(&snapshot, 55);

    assert_eq!(projection.error_strip, "hard failure");
    assert_eq!(projection.t_end_ns, 55);
    assert!(!projection.stream_active);
}

#[tokio::test]
async fn snapshot_new_user_message_resets_prior_turn_markers() {
    let state = state("snapshot-turn");
    emit(
        &state,
        AcpEvent::UserMessage {
            message_id: "old-message".to_string(),
            blocks: Vec::new(),
        },
    )
    .await;
    emit(
        &state,
        AcpEvent::ContentDelta {
            text: "old answer".to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    let mut projection = Projection::default();
    projection.replace_from_snapshot(&state.read().await.to_snapshot(), 10);
    projection.t_end_ns = 15;

    emit(
        &state,
        AcpEvent::UserMessage {
            message_id: "new-message".to_string(),
            blocks: Vec::new(),
        },
    )
    .await;
    projection.replace_from_snapshot(&state.read().await.to_snapshot(), 20);

    assert!(projection.live_assistant.is_empty());
    assert_eq!(projection.t_first_token_ns, 0);
    assert_eq!(projection.t_end_ns, 0);
}

#[tokio::test]
async fn sequence_gap_replaces_projection_from_authoritative_snapshot() {
    let state = state("recovery");
    let backend: Arc<dyn LiveBackend> = Arc::new(StateBackend {
        state: Arc::clone(&state),
        declines: Arc::new(AtomicUsize::new(0)),
    });
    let projector = LiveProjector::new(backend, SharedModel::new());
    let mut attachment = projector.attach("recovery", 0).await.unwrap();

    let (stream, second) = {
        let mut guard = state.write().await;
        let _ = guard.apply_event(&AcpEvent::ContentDelta {
            text: "fi".to_string(),
            parent_tool_use_id: None,
        });
        guard.event_seq = 1;
        let payload = AcpEvent::ContentDelta {
            text: "nal".to_string(),
            parent_tool_use_id: None,
        };
        let _ = guard.apply_event(&payload);
        guard.event_seq = 2;
        let envelope = Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "recovery".to_string(),
            payload,
        });
        (guard.event_stream(), envelope)
    };
    stream.send(second);

    assert_eq!(
        attachment.receive_next().await.unwrap(),
        ReceiveOutcome::Recovered
    );
    assert_eq!(attachment.snapshot().event_seq, 2);
    assert_eq!(attachment.snapshot().live_assistant, "final");
    assert!(!attachment.snapshot().needs_resync);
}

#[tokio::test]
async fn control_overflow_resyncs_and_declines_snapshot_permission_once() {
    let state = state("overflow");
    let declines = Arc::new(AtomicUsize::new(0));
    let backend: Arc<dyn LiveBackend> = Arc::new(StateBackend {
        state: Arc::clone(&state),
        declines: Arc::clone(&declines),
    });
    let projector = LiveProjector::with_control_capacity(backend, SharedModel::new(), 1);
    let mut attachment = projector.attach("overflow", 0).await.unwrap();

    emit(
        &state,
        AcpEvent::ContentDelta {
            text: "partial".to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    for _ in 0..1_000 {
        if attachment.queued_control_events() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(attachment.queued_control_events(), 1);

    emit(
        &state,
        AcpEvent::PermissionRequest {
            request_id: "overflow-permission".to_string(),
            tool_call: serde_json::json!({}),
            options: vec![PermissionOptionInfo {
                option_id: "deny".to_string(),
                name: "Deny".to_string(),
                kind: "reject_once".to_string(),
            }],
        },
    )
    .await;
    for _ in 0..1_000 {
        if attachment.recovery_pending() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(attachment.recovery_pending());

    assert_eq!(
        attachment.receive_next().await.unwrap(),
        ReceiveOutcome::Recovered
    );
    assert_eq!(attachment.snapshot().event_seq, 2);
    assert_eq!(declines.load(Ordering::SeqCst), 1);

    emit(
        &state,
        AcpEvent::TurnComplete {
            session_id: "session".to_string(),
            stop_reason: "end_turn".to_string(),
            agent_type: "codex".to_string(),
            mark_awaiting_reply: true,
            termination_source: None,
            provider_turn_id: None,
        },
    )
    .await;
    assert_eq!(
        attachment.receive_next().await.unwrap(),
        ReceiveOutcome::Applied
    );
    assert!(attachment.snapshot().t_end_ns > 0);
    assert!(!attachment.snapshot().stream_active);

    attachment.resync().await.unwrap();
    let authoritative = state.read().await.to_snapshot();
    let mut expected = Projection::default();
    expected.replace_from_snapshot(&authoritative, 1);
    assert_eq!(attachment.snapshot().event_seq, expected.event_seq);
    assert_eq!(
        attachment.snapshot().live_assistant,
        expected.live_assistant
    );
    assert_eq!(attachment.snapshot().tools, expected.tools);
    assert_eq!(attachment.snapshot().stream_active, expected.stream_active);
    assert_eq!(declines.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn broadcast_lag_recovers_without_blocking_the_producer() {
    let state = state("lag");
    let backend: Arc<dyn LiveBackend> = Arc::new(StateBackend {
        state: Arc::clone(&state),
        declines: Arc::new(AtomicUsize::new(0)),
    });
    let projector = LiveProjector::with_control_capacity(backend, SharedModel::new(), 5_000);
    let mut attachment = projector.attach("lag", 0).await.unwrap();

    {
        let mut guard = state.write().await;
        let stream = guard.event_stream();
        for seq in 1..=4_097 {
            let payload = AcpEvent::ContentDelta {
                text: if seq == 4_097 {
                    "final".to_string()
                } else {
                    String::new()
                },
                parent_tool_use_id: None,
            };
            let _ = guard.apply_event(&payload);
            guard.event_seq = seq;
            stream.send(Arc::new(EventEnvelope {
                seq,
                connection_id: "lag".to_string(),
                payload,
            }));
        }
    }

    assert_eq!(
        attachment.receive_next().await.unwrap(),
        ReceiveOutcome::Recovered
    );
    assert_eq!(attachment.snapshot().event_seq, 4_097);
    assert_eq!(attachment.snapshot().live_assistant, "final");
    assert!(!attachment.snapshot().needs_resync);
}
