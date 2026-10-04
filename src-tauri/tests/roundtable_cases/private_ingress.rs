//! P07b private runtime ingress and the completion boundary.
//!
//! Raw ACP update, permission, and lifecycle events are diverted before the
//! public emit path. The completion wait reuses the P05 barrier: admission
//! closes inside the gate, and the wait runs outside it.

use std::str::FromStr;
use std::sync::{Arc, Mutex};

use codeg_lib::acp::types::PermissionOptionInfo;
use codeg_lib::acp::{AcpEvent, EventBusMetrics, InternalEventBus, SessionState};
use codeg_lib::auto_title::ConnectionPurpose;
use codeg_lib::models::AgentType;
use codeg_lib::roundtable::{
    bind_private_ingress, BarrierFact, CompletionCoordinator, CompletionMarker, ConnectionOwner,
    IngressKind, PrivateRuntimeSink, RuntimeIngress,
};
use codeg_lib::web::event_bridge::{
    emit_event, emit_with_state, EventEmitter, WebEventBroadcaster,
};
use roundtable_protocol::{
    canonical_bytes, submit_candidate, AcpUpdate, ActorEffect, AttemptId, BindingId,
    CandidateState, DecisionKind, Epoch, ErrorCode, Fence, FinishKind, GateTraceEvent, HandlerId,
    Hash256, IncarnationId, PhaseId, PhaseKind, ResultScope, Revision, RtResult, SpeakerId,
    UpdateDisposition, VisibleAliases,
};
use serde_json::json;
use tokio::sync::{broadcast, RwLock};

const UPDATE_BODY: &str = "roundtable-raw-update-body";
const PERMISSION_BODY: &str = "roundtable-raw-permission-body";
const LIFECYCLE_BODY: &str = "roundtable-raw-lifecycle-body";
const NEXT_BODY: &str = "roundtable-raw-update-next-incarnation";
const PUBLIC_BODY: &str = "ordinary-public-body";

fn id_text(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn id<T: FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    id_text(n).parse().expect("id")
}

fn fence(attempt: u8, binding: u8, incarnation: u8) -> Fence {
    Fence {
        boot_epoch: Epoch(7),
        run_epoch: Epoch(7),
        phase_id: id::<PhaseId>(1),
        phase_revision: Revision(1),
        attempt_id: id::<AttemptId>(attempt),
        binding_id: id::<BindingId>(binding),
        incarnation: id::<IncarnationId>(incarnation),
        context_hash: Hash256::from_bytes([0x11; 32]),
        policy_hash: Hash256::from_bytes([0x22; 32]),
    }
}

fn session(connection_id: &str, purpose: ConnectionPurpose) -> Arc<RwLock<SessionState>> {
    let mut state = SessionState::new(
        connection_id.to_string(),
        AgentType::Codex,
        None,
        codeg_lib::roundtable::ROUNDTABLE_SERVICE_LABEL.to_string(),
        None,
    );
    state.purpose = purpose;
    state.active_turn_generation = Some(4);
    Arc::new(RwLock::new(state))
}

struct RecordingSink {
    events: Mutex<Vec<RuntimeIngress>>,
}

impl PrivateRuntimeSink for RecordingSink {
    fn attach(&self, _connection_id: &str, _owner: &ConnectionOwner) {}

    fn push(&self, ingress: RuntimeIngress) -> RtResult<()> {
        self.events.lock().expect("sink").push(ingress);
        Ok(())
    }
}

fn contains_roundtable_body(value: &serde_json::Value) -> bool {
    value.to_string().contains("roundtable-raw-")
}

fn bus_bodies(
    rx: &mut broadcast::Receiver<Arc<codeg_lib::acp::InternalEventEnvelope>>,
) -> Vec<AcpEvent> {
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(envelope) => events.push(envelope.payload.clone()),
            Err(_) => break,
        }
    }
    events
}

#[tokio::test]
async fn private_sink_receives_raw_events_only() {
    let broadcaster = Arc::new(WebEventBroadcaster::new());
    let bus = Arc::new(InternalEventBus::new(Arc::new(EventBusMetrics::default())));
    let emitter = EventEmitter::web_only(Arc::clone(&broadcaster), Arc::clone(&bus));
    let mut bus_rx = bus.subscribe();
    let mut web_rx = broadcaster.subscribe();
    emit_event(
        &emitter,
        "conversation://changed",
        &json!({ "marker": "public-web-control" }),
    );
    let control = web_rx.try_recv().expect("web subscription is live");
    assert_eq!(
        control
            .payload
            .get("marker")
            .and_then(|value| value.as_str()),
        Some("public-web-control")
    );
    assert!(!contains_roundtable_body(&control.payload));

    let ordinary = session("ordinary-public", ConnectionPurpose::User);
    emit_with_state(
        &ordinary,
        &emitter,
        AcpEvent::ContentDelta {
            text: PUBLIC_BODY.to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    let public_events = bus_bodies(&mut bus_rx);
    assert!(
        public_events.iter().any(|event| matches!(
            event,
            AcpEvent::ContentDelta { text, .. } if text == PUBLIC_BODY
        )),
        "ordinary sessions still reach the legacy bus"
    );

    let first = fence(2, 3, 4);
    let second = fence(5, 6, 7);
    let sink = Arc::new(RecordingSink {
        events: Mutex::new(Vec::new()),
    });
    let primary = session("rt-primary", ConnectionPurpose::Roundtable);
    let follow = session("rt-follow", ConnectionPurpose::Roundtable);
    let mut primary_stream = primary.read().await.event_stream().subscribe();
    let mut follow_stream = follow.read().await.event_stream().subscribe();
    bind_private_ingress("rt-primary", sink.clone(), first.clone(), 4);
    bind_private_ingress("rt-follow", sink.clone(), second.clone(), 4);

    emit_with_state(
        &primary,
        &emitter,
        AcpEvent::ContentDelta {
            text: UPDATE_BODY.to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    emit_with_state(
        &primary,
        &emitter,
        AcpEvent::PermissionRequest {
            request_id: "perm-raw".into(),
            tool_call: json!({ "title": PERMISSION_BODY }),
            options: vec![PermissionOptionInfo {
                option_id: "deny".into(),
                name: "Deny".into(),
                kind: "reject_once".into(),
                meta: None,
            }],
            queued: 0,
        },
    )
    .await;
    emit_with_state(
        &primary,
        &emitter,
        AcpEvent::SessionStarted {
            session_id: LIFECYCLE_BODY.to_string(),
        },
    )
    .await;
    emit_with_state(
        &follow,
        &emitter,
        AcpEvent::ContentDelta {
            text: NEXT_BODY.to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;

    let recorded = sink.events.lock().expect("sink").clone();
    assert_eq!(
        recorded.len(),
        4,
        "private sink receives only the raw events"
    );
    assert_eq!(
        recorded
            .iter()
            .map(|ingress| ingress.kind)
            .collect::<Vec<_>>(),
        vec![
            IngressKind::Update,
            IngressKind::Permission,
            IngressKind::Lifecycle,
            IngressKind::Update,
        ]
    );
    assert_eq!(
        recorded
            .iter()
            .take(3)
            .map(|ingress| ingress.ingress_seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        recorded[3].ingress_seq, 1,
        "each incarnation has its own seq"
    );
    for ingress in recorded.iter().take(3) {
        assert_eq!(ingress.fence, first);
        assert_eq!(ingress.turn_generation, 4);
    }
    assert_eq!(recorded[3].fence, second);
    assert_eq!(recorded[3].turn_generation, 4);
    assert!(matches!(
        &recorded[0].raw,
        AcpEvent::ContentDelta { text, .. } if text == UPDATE_BODY
    ));
    assert!(matches!(
        &recorded[1].raw,
        AcpEvent::PermissionRequest { tool_call, .. } if tool_call.get("title").and_then(|v| v.as_str()) == Some(PERMISSION_BODY)
    ));
    assert!(matches!(
        &recorded[2].raw,
        AcpEvent::SessionStarted { session_id } if session_id == LIFECYCLE_BODY
    ));
    assert!(matches!(
        &recorded[3].raw,
        AcpEvent::ContentDelta { text, .. } if text == NEXT_BODY
    ));

    let leaked = bus_bodies(&mut bus_rx);
    assert!(
        leaked.iter().all(|event| !serde_json::to_string(event)
            .unwrap_or_default()
            .contains("roundtable-raw-")),
        "legacy bus received roundtable body: {leaked:?}"
    );
    loop {
        match web_rx.try_recv() {
            Ok(event) => assert!(
                !contains_roundtable_body(&event.payload),
                "global web broadcast received roundtable body: {}",
                event.payload
            ),
            Err(_) => break,
        }
    }
    assert!(primary_stream.try_recv().is_err());
    assert!(follow_stream.try_recv().is_err());
    assert_eq!(primary.read().await.event_seq, 0);
    assert_eq!(follow.read().await.event_seq, 0);
    assert!(primary
        .read()
        .await
        .recent_events_after(0)
        .unwrap_or_default()
        .is_empty());
    assert_eq!(ordinary.read().await.event_seq, 1);
}

#[test]
fn actor_remains_responsive_while_completion_barrier_waits() {
    let fence = fence(8, 9, 10);
    let handler_id = HandlerId::new("waiting-tool");
    let mut coordinator = CompletionCoordinator::open(fence.clone());
    coordinator
        .admit_handler(handler_id.clone())
        .expect("mcp open");
    let receipt = sealed_candidate();
    assert_eq!(receipt.state, CandidateState::Staged);
    coordinator
        .stage_candidate(receipt.clone())
        .expect("staged");
    let barrier = coordinator.begin(CompletionMarker {
        fence: fence.clone(),
        turn_generation: 4,
        ingress_watermark: 1,
        prompt_response_seq: 1,
        finish: FinishKind::Normal,
    });
    assert!(coordinator.is_waiting());
    assert!(coordinator.close_sampled_inside_gate());
    assert!(!coordinator.gate_held());
    assert!(!coordinator.waited_inside_gate());
    assert!(barrier.pending_tools.contains(&handler_id));
    assert!(barrier.mcp_closed);
    assert_eq!(
        coordinator.gate_trace(),
        vec![
            GateTraceEvent::Acquired,
            GateTraceEvent::ClosedMcp,
            GateTraceEvent::CapturedHandlers,
            GateTraceEvent::Released,
        ]
    );
    assert!(coordinator.poll_barrier());
    assert_eq!(
        coordinator.gate_trace().last().copied(),
        Some(GateTraceEvent::Waited)
    );
    assert!(!coordinator.gate_held());
    assert!(!coordinator.waited_inside_gate());

    coordinator.request_reply(handler_id.clone());
    assert_eq!(
        coordinator.handle_actor(),
        Some(ActorEffect::Replied(handler_id.clone()))
    );
    coordinator.request_stop();
    assert_eq!(coordinator.handle_actor(), Some(ActorEffect::Stopped));
    assert_eq!(coordinator.stops_handled(), 1);
    assert_eq!(coordinator.reply_count(), 1);
    assert!(coordinator.is_waiting());
    assert!(!coordinator.gate_held());
    assert_eq!(
        coordinator.applied_ingress(),
        0,
        "handler reply is not ACP ingress"
    );

    let late = coordinator
        .late_submit(&HandlerId::new("never-admitted"))
        .expect_err("not admitted");
    assert_eq!(late.code, ErrorCode::InvalidState);
    assert_eq!(late.details.reason.as_deref(), Some("attempt_closed"));
    assert!(ErrorCode::parse("attempt_closed").is_none());
    let closed = coordinator
        .admit_handler(HandlerId::new("after-close"))
        .expect_err("admission closed");
    assert_eq!(closed.code, ErrorCode::InvalidState);
    assert_eq!(closed.details.reason.as_deref(), Some("attempt_closed"));

    coordinator.finish_handler(&handler_id);
    assert_eq!(coordinator.applied_ingress(), 0);
    assert!(coordinator.poll_barrier());
    coordinator.apply_update(AcpUpdate {
        seq: 1,
        attempt_id: fence.attempt_id,
        binding_id: fence.binding_id,
        incarnation: fence.incarnation,
        cancelled: false,
    });
    assert!(!coordinator.poll_barrier());
    assert!(!coordinator.is_waiting());
    assert!(!coordinator.waited_inside_gate());
    assert!(!coordinator.gate_held());

    let wrong = BarrierFact {
        fence: fence.clone(),
        admitted_handlers: Default::default(),
        acp_watermark: barrier.ingress_watermark,
    };
    let mismatch = coordinator
        .on_barrier_drained(wrong)
        .expect_err("fact is not an accept");
    assert_eq!(mismatch.code, ErrorCode::InvalidState);
    assert_eq!(
        coordinator.staged_candidate().map(|receipt| receipt.state),
        Some(CandidateState::Staged)
    );

    let fact = BarrierFact {
        fence: fence.clone(),
        admitted_handlers: barrier.pending_tools.clone(),
        acp_watermark: barrier.ingress_watermark,
    };
    let done = coordinator
        .on_barrier_drained(fact)
        .expect("drained barrier");
    assert_eq!(done.finish_reason, "normal");
    assert_ne!(done.finish_reason, "accepted");
    assert_eq!(done.fence, fence);
    assert!(done.tool_barrier.drained);
    assert_eq!(
        done.candidate_id.as_deref(),
        Some(receipt.candidate_id.as_str())
    );
    assert_eq!(
        coordinator.staged_candidate().map(|receipt| receipt.state),
        Some(CandidateState::Staged),
        "completion does not accept the candidate"
    );
}

#[test]
fn late_update_never_belongs_to_next_attempt() {
    let current = fence(11, 12, 13);
    let mut coordinator = CompletionCoordinator::open(current.clone());
    assert_eq!(
        coordinator.apply_update(AcpUpdate {
            seq: 1,
            attempt_id: current.attempt_id,
            binding_id: current.binding_id,
            incarnation: current.incarnation,
            cancelled: false,
        }),
        UpdateDisposition::Applied
    );
    assert_eq!(
        coordinator.apply_update(AcpUpdate {
            seq: 2,
            attempt_id: current.attempt_id,
            binding_id: current.binding_id,
            incarnation: current.incarnation,
            cancelled: true,
        }),
        UpdateDisposition::Applied,
        "a cancelled tail with matching attribution stays on this attempt"
    );
    assert!(coordinator.claim(&current.binding_id).is_ok());
    assert_eq!(coordinator.applied_ingress(), 2);

    let next_attempt = id::<AttemptId>(14);
    let other_incarnation = id::<IncarnationId>(15);
    let trailing = AcpUpdate {
        seq: 3,
        attempt_id: next_attempt,
        binding_id: current.binding_id,
        incarnation: other_incarnation,
        cancelled: true,
    };
    assert_eq!(
        coordinator.apply_update(trailing.clone()),
        UpdateDisposition::Retired
    );
    assert_eq!(coordinator.applied_ingress(), 2);
    let retired = coordinator
        .claim(&current.binding_id)
        .expect_err("retired binding");
    assert_eq!(retired.code, ErrorCode::InvalidState);
    assert_eq!(retired.details.reason.as_deref(), Some("binding_retired"));

    let fresh_binding = id::<BindingId>(16);
    let fresh = fence(14, 16, 17);
    let mut next =
        CompletionCoordinator::open_with_ledger(fresh.clone(), coordinator.ledger_snapshot());
    assert!(next.claim(&fresh_binding).is_ok());
    let reused = next.claim(&current.binding_id).expect_err("cannot reuse");
    assert_eq!(reused.details.reason.as_deref(), Some("binding_retired"));
    assert_ne!(
        next.apply_update(trailing),
        UpdateDisposition::Applied,
        "the late update is not the next attempt's ingress"
    );
    assert_eq!(next.applied_ingress(), 0);
    assert_ne!(fresh.attempt_id, current.attempt_id);
    assert_ne!(fresh.incarnation, current.incarnation);
}

fn sealed_candidate() -> roundtable_protocol::CandidateReceipt {
    let scope = ResultScope {
        phase_kind: PhaseKind::Proposal,
        speaker_id: id::<SpeakerId>(18),
        aliases: VisibleAliases::default(),
        mandatory_targets: Vec::new(),
        published: roundtable_protocol::PublishedHistory::default(),
        quota_bytes: 8 * 1024,
    };
    let raw = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "成员摘要",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "low"
        }]
    }))
    .unwrap();
    let decision = submit_candidate(
        &roundtable_protocol::SubmissionState::open(),
        &roundtable_protocol::SubmissionId::from_str("seal-1").unwrap(),
        &raw,
        &scope,
    );
    match decision.outcome {
        DecisionKind::Staged(receipt) => receipt,
        other => panic!("expected one staged candidate, got {other:?}"),
    }
}
