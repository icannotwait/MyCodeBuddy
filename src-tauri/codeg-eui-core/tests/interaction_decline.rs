use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codeg_eui_core::{
    decline_interaction, decline_once, reconcile_snapshot_interactions, InteractionBackend,
    InteractionKey, LiveBackend, LiveFuture, LiveProjector, PendingInteraction, ReceiveOutcome,
    SharedModel, INTERACTIVE_PROMPT_NOTICE,
};
use codeg_lib::acp::types::PermissionOptionInfo;
use codeg_lib::acp::{AcpEvent, EventEnvelope, SessionState};
use codeg_lib::models::AgentType;
use tokio::sync::RwLock;

type ActionFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Permission(String, String),
    CancelTurn,
    Question(String),
    Plan,
}

#[derive(Clone, Default)]
struct RecordingBackend {
    actions: Arc<Mutex<Vec<Action>>>,
    state: Option<Arc<RwLock<SessionState>>>,
}

#[derive(Clone)]
struct FailingBackend {
    state: Arc<RwLock<SessionState>>,
    cancel_count: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct ParkedBackend {
    expected: ParkedAction,
    responder: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParkedAction {
    Permission,
    CancelTurn,
    Question,
    Plan,
}

impl ParkedBackend {
    fn release(&self, action: ParkedAction) -> ActionFuture<'_> {
        if action != self.expected {
            return Box::pin(async { Err("wrong decline path".to_string()) });
        }
        let responder = self.responder.lock().unwrap().take();
        Box::pin(async move {
            responder
                .ok_or_else(|| "responder already released".to_string())?
                .send(())
                .map_err(|_| "parked receiver closed".to_string())
        })
    }
}

impl InteractionBackend for ParkedBackend {
    fn respond_permission<'a>(
        &'a self,
        _connection_id: &'a str,
        _request_id: &'a str,
        _option_id: &'a str,
    ) -> ActionFuture<'a> {
        self.release(ParkedAction::Permission)
    }

    fn cancel_active_turn<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
        self.release(ParkedAction::CancelTurn)
    }

    fn cancel_question<'a>(
        &'a self,
        _connection_id: &'a str,
        _question_id: &'a str,
    ) -> ActionFuture<'a> {
        self.release(ParkedAction::Question)
    }

    fn cancel_plan_approvals_by_parent<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
        self.release(ParkedAction::Plan)
    }
}

impl LiveBackend for FailingBackend {
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

impl InteractionBackend for FailingBackend {
    fn respond_permission<'a>(
        &'a self,
        _connection_id: &'a str,
        _request_id: &'a str,
        _option_id: &'a str,
    ) -> ActionFuture<'a> {
        Box::pin(async { Err("permission responder closed".to_string()) })
    }

    fn cancel_active_turn<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
        self.cancel_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn cancel_question<'a>(
        &'a self,
        _connection_id: &'a str,
        _question_id: &'a str,
    ) -> ActionFuture<'a> {
        Box::pin(async { Err("question responder closed".to_string()) })
    }

    fn cancel_plan_approvals_by_parent<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
        Box::pin(async { Err("plan responder closed".to_string()) })
    }
}

impl RecordingBackend {
    fn actions(&self) -> Vec<Action> {
        self.actions.lock().unwrap().clone()
    }

    fn push(&self, action: Action) -> ActionFuture<'_> {
        self.actions.lock().unwrap().push(action);
        Box::pin(async { Ok(()) })
    }

    fn with_state(state: Arc<RwLock<SessionState>>) -> Self {
        Self {
            actions: Arc::new(Mutex::new(Vec::new())),
            state: Some(state),
        }
    }
}

impl LiveBackend for RecordingBackend {
    fn get_state<'a>(
        &'a self,
        connection_id: &'a str,
    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>> {
        let state = self.state.clone();
        Box::pin(async move {
            let state = state?;
            let matches = state.read().await.connection_id == connection_id;
            matches.then_some(state)
        })
    }
}

impl InteractionBackend for RecordingBackend {
    fn respond_permission<'a>(
        &'a self,
        _connection_id: &'a str,
        request_id: &'a str,
        option_id: &'a str,
    ) -> ActionFuture<'a> {
        self.push(Action::Permission(
            request_id.to_string(),
            option_id.to_string(),
        ))
    }

    fn cancel_active_turn<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
        self.push(Action::CancelTurn)
    }

    fn cancel_question<'a>(
        &'a self,
        _connection_id: &'a str,
        question_id: &'a str,
    ) -> ActionFuture<'a> {
        self.push(Action::Question(question_id.to_string()))
    }

    fn cancel_plan_approvals_by_parent<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
        self.push(Action::Plan)
    }
}

fn option(option_id: &str, name: &str, kind: &str) -> PermissionOptionInfo {
    PermissionOptionInfo {
        option_id: option_id.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
    }
}

fn session_state(connection_id: &str) -> Arc<RwLock<SessionState>> {
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

#[tokio::test]
async fn permission_uses_kind_then_name_then_id_or_cancels_turn() {
    let backend = RecordingBackend::default();
    decline_interaction(
        &backend,
        "c1",
        PendingInteraction::Permission {
            request_id: "r1".to_string(),
            options: vec![
                option("reject-by-id", "Allow", "allow_once"),
                option("deny-by-name", "Deny", "allow_once"),
                option("kind-wins", "Allow", "reject_once"),
            ],
        },
    )
    .await
    .unwrap();
    decline_interaction(
        &backend,
        "c1",
        PendingInteraction::Permission {
            request_id: "r2".to_string(),
            options: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        backend.actions(),
        vec![
            Action::Permission("r1".to_string(), "kind-wins".to_string()),
            Action::CancelTurn,
        ]
    );
}

#[tokio::test]
async fn all_decline_paths_release_a_parked_responder() {
    #[derive(Clone, Copy)]
    enum ParkedCase {
        Permission,
        PermissionCancel,
        Question,
        Plan,
    }

    for case in [
        ParkedCase::Permission,
        ParkedCase::PermissionCancel,
        ParkedCase::Question,
        ParkedCase::Plan,
    ] {
        let (released, parked) = tokio::sync::oneshot::channel();
        let backend = ParkedBackend {
            expected: match case {
                ParkedCase::Permission => ParkedAction::Permission,
                ParkedCase::PermissionCancel => ParkedAction::CancelTurn,
                ParkedCase::Question => ParkedAction::Question,
                ParkedCase::Plan => ParkedAction::Plan,
            },
            responder: Arc::new(Mutex::new(Some(released))),
        };
        let interaction = match case {
            ParkedCase::Permission => PendingInteraction::Permission {
                request_id: "permission".to_string(),
                options: vec![option("deny", "Deny", "reject_once")],
            },
            ParkedCase::PermissionCancel => PendingInteraction::Permission {
                request_id: "permission-cancel".to_string(),
                options: Vec::new(),
            },
            ParkedCase::Question => PendingInteraction::Question {
                question_id: "question".to_string(),
            },
            ParkedCase::Plan => PendingInteraction::Plan {
                approval_id: "plan".to_string(),
            },
        };

        decline_interaction(&backend, "parked", interaction)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), parked)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn snapshot_and_live_event_share_one_deduplicated_decline_policy() {
    let backend = RecordingBackend::default();
    let mut state = SessionState::new(
        "c1".to_string(),
        AgentType::Codex,
        None,
        "eui-test".to_string(),
        None,
    );
    let _ = state.apply_event(&AcpEvent::PermissionRequest {
        request_id: "p1".to_string(),
        tool_call: serde_json::json!({}),
        options: vec![option("deny", "Deny", "reject_once")],
    });
    let _ = state.apply_event(&AcpEvent::QuestionRequest {
        question_id: "q1".to_string(),
        questions: Vec::new(),
    });
    let _ = state.apply_event(&AcpEvent::PlanApprovalRequest {
        approval_id: "a1".to_string(),
        tool_call_id: "tool-1".to_string(),
        plan_markdown: "plan".to_string(),
    });
    let snapshot = state.to_snapshot();
    let mut seen = HashSet::<InteractionKey>::new();

    reconcile_snapshot_interactions(&backend, "c1", &snapshot, &mut seen)
        .await
        .unwrap();
    reconcile_snapshot_interactions(&backend, "c1", &snapshot, &mut seen)
        .await
        .unwrap();
    decline_once(
        &backend,
        "c1",
        PendingInteraction::Question {
            question_id: "q1".to_string(),
        },
        &mut seen,
    )
    .await
    .unwrap();

    assert_eq!(
        backend.actions(),
        vec![
            Action::Permission("p1".to_string(), "deny".to_string()),
            Action::Question("q1".to_string()),
            Action::Plan,
        ]
    );
}

#[tokio::test]
async fn snapshot_pending_interactions_decline_before_event_resume_once() {
    let state = session_state("snapshot-only");
    {
        let mut guard = state.write().await;
        let _ = guard.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p1".to_string(),
            tool_call: serde_json::json!({}),
            options: vec![option("deny", "Deny", "reject_once")],
        });
        let _ = guard.apply_event(&AcpEvent::QuestionRequest {
            question_id: "q1".to_string(),
            questions: Vec::new(),
        });
        let _ = guard.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "a1".to_string(),
            tool_call_id: "tool-1".to_string(),
            plan_markdown: "plan".to_string(),
        });
    }
    let backend = RecordingBackend::with_state(Arc::clone(&state));
    let projector = LiveProjector::new(Arc::new(backend.clone()), SharedModel::new());

    let mut attachment = projector.attach("snapshot-only", 0).await.unwrap();

    assert_eq!(
        backend.actions(),
        vec![
            Action::Permission("p1".to_string(), "deny".to_string()),
            Action::Question("q1".to_string()),
            Action::Plan,
        ]
    );
    assert_eq!(attachment.queued_control_events(), 0);
    assert_eq!(attachment.snapshot().error_strip, INTERACTIVE_PROMPT_NOTICE);

    attachment.resync().await.unwrap();
    assert_eq!(backend.actions().len(), 3);
}

#[tokio::test]
async fn live_interactions_decline_and_turn_reaches_terminal_marker() {
    let state = session_state("live-interactions");
    let backend = RecordingBackend::with_state(Arc::clone(&state));
    let projector = LiveProjector::new(Arc::new(backend.clone()), SharedModel::new());
    let mut attachment = projector.attach("live-interactions", 0).await.unwrap();

    emit(
        &state,
        AcpEvent::QuestionRequest {
            question_id: "q-live".to_string(),
            questions: Vec::new(),
        },
    )
    .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), attachment.receive_next())
            .await
            .unwrap()
            .unwrap(),
        ReceiveOutcome::Applied
    );
    emit(
        &state,
        AcpEvent::PlanApprovalRequest {
            approval_id: "a-live".to_string(),
            tool_call_id: "tool-live".to_string(),
            plan_markdown: "plan".to_string(),
        },
    )
    .await;
    attachment.receive_next().await.unwrap();
    emit(
        &state,
        AcpEvent::PermissionRequest {
            request_id: "p-live".to_string(),
            tool_call: serde_json::json!({}),
            options: vec![option("deny-live", "Deny", "reject_once")],
        },
    )
    .await;
    attachment.receive_next().await.unwrap();
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
    tokio::time::timeout(Duration::from_secs(2), attachment.receive_next())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        backend.actions(),
        vec![
            Action::Question("q-live".to_string()),
            Action::Plan,
            Action::Permission("p-live".to_string(), "deny-live".to_string()),
        ]
    );
    assert_eq!(attachment.snapshot().error_strip, INTERACTIVE_PROMPT_NOTICE);
    assert!(attachment.snapshot().t_end_ns > 0);
    assert!(!attachment.snapshot().stream_active);
}

#[tokio::test]
async fn decline_failure_cancels_terminalizes_and_stops_receiving() {
    let state = session_state("decline-failure");
    let cancel_count = Arc::new(AtomicUsize::new(0));
    let backend = FailingBackend {
        state: Arc::clone(&state),
        cancel_count: Arc::clone(&cancel_count),
    };
    let projector = LiveProjector::new(Arc::new(backend), SharedModel::new());
    let mut attachment = projector.attach("decline-failure", 0).await.unwrap();

    emit(
        &state,
        AcpEvent::PermissionRequest {
            request_id: "permission".to_string(),
            tool_call: serde_json::json!({}),
            options: vec![option("deny", "Deny", "reject_once")],
        },
    )
    .await;
    let error = attachment.receive_next().await.unwrap_err();

    assert!(error.to_string().contains("permission responder closed"));
    assert_eq!(cancel_count.load(Ordering::SeqCst), 1);
    assert!(!attachment.snapshot().stream_active);
    assert!(attachment.snapshot().t_end_ns > 0);
    assert!(attachment
        .snapshot()
        .error_strip
        .contains("permission responder closed"));

    emit(
        &state,
        AcpEvent::ContentDelta {
            text: "must not resume".to_string(),
            parent_tool_use_id: None,
        },
    )
    .await;
    tokio::task::yield_now().await;
    assert_eq!(attachment.queued_control_events(), 0);
}
