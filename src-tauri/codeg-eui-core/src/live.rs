use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use codeg_lib::acp::session_state::visible_assistant_text;
use codeg_lib::acp::types::PermissionOptionInfo;
use codeg_lib::acp::{AcpEvent, EventEnvelope, LiveSessionSnapshot, SessionState};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tokio::task::JoinHandle;

use crate::model::SharedModel;
use crate::perf::native_timestamp_ns;

pub const LIVE_CONTROL_CAPACITY: usize = 128;
pub const INTERACTIVE_PROMPT_NOTICE: &str = "Interactive prompts require the main app";

pub type InteractionFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
pub type LiveFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait InteractionBackend: Send + Sync {
    fn respond_permission<'a>(
        &'a self,
        connection_id: &'a str,
        request_id: &'a str,
        option_id: &'a str,
    ) -> InteractionFuture<'a>;

    fn cancel_active_turn<'a>(&'a self, connection_id: &'a str) -> InteractionFuture<'a>;

    fn cancel_question<'a>(
        &'a self,
        connection_id: &'a str,
        question_id: &'a str,
    ) -> InteractionFuture<'a>;

    fn cancel_plan_approvals_by_parent<'a>(
        &'a self,
        connection_id: &'a str,
    ) -> InteractionFuture<'a>;
}

pub trait LiveBackend: InteractionBackend {
    fn get_state<'a>(
        &'a self,
        connection_id: &'a str,
    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>>;
}

pub(crate) struct AppLiveBackend {
    state: Arc<codeg_lib::app_state::AppState>,
}

impl AppLiveBackend {
    pub(crate) fn new(state: Arc<codeg_lib::app_state::AppState>) -> Self {
        Self { state }
    }
}

impl InteractionBackend for AppLiveBackend {
    fn respond_permission<'a>(
        &'a self,
        connection_id: &'a str,
        request_id: &'a str,
        option_id: &'a str,
    ) -> InteractionFuture<'a> {
        Box::pin(async move {
            self.state
                .connection_manager
                .respond_permission(connection_id, request_id, option_id)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn cancel_active_turn<'a>(&'a self, connection_id: &'a str) -> InteractionFuture<'a> {
        Box::pin(async move {
            self.state
                .connection_manager
                .cancel(&self.state.db.conn, connection_id)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn cancel_question<'a>(
        &'a self,
        connection_id: &'a str,
        question_id: &'a str,
    ) -> InteractionFuture<'a> {
        Box::pin(async move {
            self.state
                .connection_manager
                .cancel_question(connection_id, question_id)
                .await;
            Ok(())
        })
    }

    fn cancel_plan_approvals_by_parent<'a>(
        &'a self,
        connection_id: &'a str,
    ) -> InteractionFuture<'a> {
        Box::pin(async move {
            self.state
                .connection_manager
                .cancel_plan_approvals_by_parent(connection_id)
                .await;
            Ok(())
        })
    }
}

impl LiveBackend for AppLiveBackend {
    fn get_state<'a>(
        &'a self,
        connection_id: &'a str,
    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>> {
        Box::pin(async move { self.state.connection_manager.get_state(connection_id).await })
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum InteractionKey {
    Permission(String),
    Question(String),
    Plan(String),
}

#[derive(Clone, Debug)]
pub enum PendingInteraction {
    Permission {
        request_id: String,
        options: Vec<PermissionOptionInfo>,
    },
    Question {
        question_id: String,
    },
    Plan {
        approval_id: String,
    },
}

impl PendingInteraction {
    pub fn key(&self) -> InteractionKey {
        match self {
            Self::Permission { request_id, .. } => InteractionKey::Permission(request_id.clone()),
            Self::Question { question_id } => InteractionKey::Question(question_id.clone()),
            Self::Plan { approval_id } => InteractionKey::Plan(approval_id.clone()),
        }
    }
}

pub async fn decline_interaction<B: InteractionBackend + ?Sized>(
    backend: &B,
    connection_id: &str,
    interaction: PendingInteraction,
) -> Result<(), String> {
    match interaction {
        PendingInteraction::Permission {
            request_id,
            options,
        } => match reject_option(&options) {
            Some(option_id) => {
                backend
                    .respond_permission(connection_id, &request_id, option_id)
                    .await
            }
            None => backend.cancel_active_turn(connection_id).await,
        },
        PendingInteraction::Question { question_id } => {
            backend.cancel_question(connection_id, &question_id).await
        }
        PendingInteraction::Plan { .. } => {
            backend.cancel_plan_approvals_by_parent(connection_id).await
        }
    }
}

pub async fn decline_once<B: InteractionBackend + ?Sized>(
    backend: &B,
    connection_id: &str,
    interaction: PendingInteraction,
    seen: &mut HashSet<InteractionKey>,
) -> Result<(), String> {
    if !seen.insert(interaction.key()) {
        return Ok(());
    }
    decline_interaction(backend, connection_id, interaction).await
}

pub async fn reconcile_snapshot_interactions<B: InteractionBackend + ?Sized>(
    backend: &B,
    connection_id: &str,
    snapshot: &LiveSessionSnapshot,
    seen: &mut HashSet<InteractionKey>,
) -> Result<(), String> {
    if let Some(permission) = &snapshot.pending_permission {
        decline_once(
            backend,
            connection_id,
            PendingInteraction::Permission {
                request_id: permission.request_id.clone(),
                options: permission.options.clone(),
            },
            seen,
        )
        .await?;
    }
    if let Some(question) = &snapshot.pending_question {
        decline_once(
            backend,
            connection_id,
            PendingInteraction::Question {
                question_id: question.question_id.clone(),
            },
            seen,
        )
        .await?;
    }
    if let Some(plan) = &snapshot.pending_plan_approval {
        decline_once(
            backend,
            connection_id,
            PendingInteraction::Plan {
                approval_id: plan.approval_id.clone(),
            },
            seen,
        )
        .await?;
    }
    Ok(())
}

pub fn pending_interaction(event: &AcpEvent) -> Option<PendingInteraction> {
    match event {
        AcpEvent::PermissionRequest {
            request_id,
            options,
            ..
        } => Some(PendingInteraction::Permission {
            request_id: request_id.clone(),
            options: options.clone(),
        }),
        AcpEvent::QuestionRequest { question_id, .. } => Some(PendingInteraction::Question {
            question_id: question_id.clone(),
        }),
        AcpEvent::PlanApprovalRequest { approval_id, .. } => Some(PendingInteraction::Plan {
            approval_id: approval_id.clone(),
        }),
        _ => None,
    }
}

fn reject_option(options: &[PermissionOptionInfo]) -> Option<&str> {
    options
        .iter()
        .find(|option| is_reject_or_deny(&option.kind))
        .or_else(|| {
            options
                .iter()
                .find(|option| is_reject_or_deny(&option.name))
        })
        .or_else(|| {
            options
                .iter()
                .find(|option| is_reject_or_deny(&option.option_id))
        })
        .map(|option| option.option_id.as_str())
}

fn is_reject_or_deny(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.contains("reject") || normalized.contains("deny")
}

pub struct AttachPoint {
    pub snapshot: LiveSessionSnapshot,
    pub receiver: broadcast::Receiver<Arc<EventEnvelope>>,
}

pub async fn snapshot_and_subscribe(state: &Arc<RwLock<SessionState>>) -> AttachPoint {
    snapshot_and_subscribe_observed(state, || {}).await
}

#[doc(hidden)]
pub async fn snapshot_and_subscribe_observed(
    state: &Arc<RwLock<SessionState>>,
    while_read_locked: impl FnOnce(),
) -> AttachPoint {
    let guard = state.read().await;
    let snapshot = guard.to_snapshot();
    while_read_locked();
    AttachPoint {
        snapshot,
        receiver: guard.event_stream().subscribe(),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveError {
    #[error("ACP connection not found: {0}")]
    ConnectionNotFound(String),
    #[error("live selection changed")]
    SelectionChanged,
    #[error("interactive decline failed: {0}")]
    Interaction(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveOutcome {
    Applied,
    Ignored,
    Recovered,
    SelectionChanged,
    Closed,
}

pub struct LiveProjector {
    backend: Arc<dyn LiveBackend>,
    model: SharedModel,
    control_capacity: usize,
}

impl LiveProjector {
    pub fn new(backend: Arc<dyn LiveBackend>, model: SharedModel) -> Self {
        Self {
            backend,
            model,
            control_capacity: LIVE_CONTROL_CAPACITY,
        }
    }

    pub fn with_control_capacity(
        backend: Arc<dyn LiveBackend>,
        model: SharedModel,
        control_capacity: usize,
    ) -> Self {
        Self {
            backend,
            model,
            control_capacity: control_capacity.max(1),
        }
    }

    pub async fn attach(
        &self,
        connection_id: impl Into<String>,
        selection_epoch: u64,
    ) -> Result<LiveAttachment, LiveError> {
        let connection_id = connection_id.into();
        let state = self
            .backend
            .get_state(&connection_id)
            .await
            .ok_or_else(|| LiveError::ConnectionNotFound(connection_id.clone()))?;
        let attach = snapshot_and_subscribe(&state).await;
        let mut projection = Projection::default();
        if !self
            .model
            .seed_projection(selection_epoch, &connection_id, &mut projection)
        {
            return Err(LiveError::SelectionChanged);
        }
        let now_ns = native_timestamp_ns();
        projection.replace_from_snapshot(&attach.snapshot, now_ns);
        let mut seen = HashSet::new();
        if snapshot_has_interaction(&attach.snapshot) {
            projection.error_strip = INTERACTIVE_PROMPT_NOTICE.to_string();
        }
        if let Err(error) = reconcile_snapshot_interactions(
            self.backend.as_ref(),
            &connection_id,
            &attach.snapshot,
            &mut seen,
        )
        .await
        {
            terminalize_decline_failure(
                self.backend.as_ref(),
                &self.model,
                selection_epoch,
                &connection_id,
                &mut projection,
                error.clone(),
            )
            .await;
            return Err(LiveError::Interaction(error));
        }
        if !self
            .model
            .apply_live_projection(selection_epoch, &projection, now_ns)
        {
            return Err(LiveError::SelectionChanged);
        }
        let pump = ControlPump::start(attach.receiver, self.control_capacity);
        Ok(LiveAttachment {
            backend: Arc::clone(&self.backend),
            model: self.model.clone(),
            connection_id,
            selection_epoch,
            projection,
            seen,
            selection_rx: self.model.selection_receiver(),
            control_capacity: self.control_capacity,
            pump,
        })
    }
}

pub struct LiveAttachment {
    backend: Arc<dyn LiveBackend>,
    model: SharedModel,
    connection_id: String,
    selection_epoch: u64,
    projection: Projection,
    seen: HashSet<InteractionKey>,
    selection_rx: watch::Receiver<u64>,
    control_capacity: usize,
    pump: ControlPump,
}

impl LiveAttachment {
    pub fn snapshot(&self) -> &Projection {
        &self.projection
    }

    pub fn queued_control_events(&self) -> usize {
        self.pump.receiver.len()
    }

    pub fn recovery_pending(&self) -> bool {
        self.pump.needs_resync()
    }

    pub async fn receive_next(&mut self) -> Result<ReceiveOutcome, LiveError> {
        if *self.selection_rx.borrow() != self.selection_epoch {
            self.pump.abort();
            return Ok(ReceiveOutcome::SelectionChanged);
        }
        if self.pump.needs_resync() {
            return self.resync().await;
        }

        let envelope = tokio::select! {
            biased;
            changed = self.selection_rx.changed() => {
                if changed.is_err() || *self.selection_rx.borrow() != self.selection_epoch {
                    self.pump.abort();
                    return Ok(ReceiveOutcome::SelectionChanged);
                }
                return Ok(ReceiveOutcome::Ignored);
            }
            envelope = self.pump.receiver.recv() => envelope,
        };
        let Some(envelope) = envelope else {
            if self.pump.needs_resync() {
                return self.resync().await;
            }
            return Ok(ReceiveOutcome::Closed);
        };
        if self.pump.needs_resync() {
            return self.resync().await;
        }

        let now_ns = native_timestamp_ns();
        match self.projection.apply_envelope(&envelope, now_ns) {
            ApplyOutcome::NeedsResync => {
                let _ = self.model.apply_live_projection(
                    self.selection_epoch,
                    &self.projection,
                    now_ns,
                );
                self.resync().await
            }
            ApplyOutcome::Ignored => Ok(ReceiveOutcome::Ignored),
            ApplyOutcome::Applied => {
                if let Some(interaction) = pending_interaction(&envelope.payload) {
                    self.projection.error_strip = INTERACTIVE_PROMPT_NOTICE.to_string();
                    if let Err(error) = decline_once(
                        self.backend.as_ref(),
                        &self.connection_id,
                        interaction,
                        &mut self.seen,
                    )
                    .await
                    {
                        terminalize_decline_failure(
                            self.backend.as_ref(),
                            &self.model,
                            self.selection_epoch,
                            &self.connection_id,
                            &mut self.projection,
                            error.clone(),
                        )
                        .await;
                        self.pump.abort();
                        return Err(LiveError::Interaction(error));
                    }
                }
                if !self
                    .model
                    .apply_live_projection(self.selection_epoch, &self.projection, now_ns)
                {
                    self.pump.abort();
                    return Ok(ReceiveOutcome::SelectionChanged);
                }
                Ok(ReceiveOutcome::Applied)
            }
        }
    }

    pub async fn resync(&mut self) -> Result<ReceiveOutcome, LiveError> {
        self.projection.mark_needs_resync();
        let now_ns = native_timestamp_ns();
        if !self
            .model
            .apply_live_projection(self.selection_epoch, &self.projection, now_ns)
        {
            self.pump.abort();
            return Ok(ReceiveOutcome::SelectionChanged);
        }
        self.pump.abort();

        let state = self
            .backend
            .get_state(&self.connection_id)
            .await
            .ok_or_else(|| LiveError::ConnectionNotFound(self.connection_id.clone()))?;
        let attach = snapshot_and_subscribe(&state).await;
        self.projection
            .replace_from_snapshot(&attach.snapshot, native_timestamp_ns());
        if snapshot_has_interaction(&attach.snapshot) {
            self.projection.error_strip = INTERACTIVE_PROMPT_NOTICE.to_string();
        }
        if let Err(error) = reconcile_snapshot_interactions(
            self.backend.as_ref(),
            &self.connection_id,
            &attach.snapshot,
            &mut self.seen,
        )
        .await
        {
            terminalize_decline_failure(
                self.backend.as_ref(),
                &self.model,
                self.selection_epoch,
                &self.connection_id,
                &mut self.projection,
                error.clone(),
            )
            .await;
            return Err(LiveError::Interaction(error));
        }
        if !self.model.apply_live_projection(
            self.selection_epoch,
            &self.projection,
            native_timestamp_ns(),
        ) {
            return Ok(ReceiveOutcome::SelectionChanged);
        }
        self.pump = ControlPump::start(attach.receiver, self.control_capacity);
        Ok(ReceiveOutcome::Recovered)
    }

    pub async fn run(mut self) {
        loop {
            match self.receive_next().await {
                Ok(ReceiveOutcome::SelectionChanged | ReceiveOutcome::Closed) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}

impl Drop for LiveAttachment {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

const PUMP_RUNNING: u8 = 0;
const PUMP_LAGGED: u8 = 1;
const PUMP_OVERFLOW: u8 = 2;
const PUMP_CLOSED: u8 = 3;

struct ControlPump {
    receiver: mpsc::Receiver<Arc<EventEnvelope>>,
    status: Arc<AtomicU8>,
    task: JoinHandle<()>,
}

impl ControlPump {
    fn start(mut source: broadcast::Receiver<Arc<EventEnvelope>>, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        let status = Arc::new(AtomicU8::new(PUMP_RUNNING));
        let pump_status = Arc::clone(&status);
        let task = tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(envelope) => match sender.try_send(envelope) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            pump_status.store(PUMP_OVERFLOW, Ordering::Release);
                            break;
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        pump_status.store(PUMP_LAGGED, Ordering::Release);
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        pump_status.store(PUMP_CLOSED, Ordering::Release);
                        break;
                    }
                }
            }
        });
        Self {
            receiver,
            status,
            task,
        }
    }

    fn needs_resync(&self) -> bool {
        matches!(
            self.status.load(Ordering::Acquire),
            PUMP_LAGGED | PUMP_OVERFLOW
        )
    }

    fn abort(&self) {
        self.task.abort();
    }
}

fn snapshot_has_interaction(snapshot: &LiveSessionSnapshot) -> bool {
    snapshot.pending_permission.is_some()
        || snapshot.pending_question.is_some()
        || snapshot.pending_plan_approval.is_some()
}

async fn terminalize_decline_failure(
    backend: &dyn LiveBackend,
    model: &SharedModel,
    selection_epoch: u64,
    connection_id: &str,
    projection: &mut Projection,
    error: String,
) {
    let _ = backend.cancel_active_turn(connection_id).await;
    let now_ns = native_timestamp_ns();
    projection.error_strip = format!("{INTERACTIVE_PROMPT_NOTICE}: {error}");
    projection.stream_active = false;
    projection.t_end_ns = now_ns;
    let _ = model.apply_live_projection(selection_epoch, projection, now_ns);
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSummary {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    Ignored,
    NeedsResync,
}

impl ApplyOutcome {
    pub fn needs_resync(self) -> bool {
        self == Self::NeedsResync
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub connection_id: String,
    pub event_seq: u64,
    pub transcript_json: Vec<u8>,
    pub live_assistant: String,
    pub tools: Vec<ToolSummary>,
    pub stream_active: bool,
    pub needs_resync: bool,
    pub error_strip: String,
    pub assistant_generation: u64,
    pub transcript_generation: u64,
    pub t_first_token_ns: u64,
    pub t_end_ns: u64,
}

impl Projection {
    pub fn replace_from_snapshot(&mut self, snapshot: &LiveSessionSnapshot, now_ns: u64) {
        self.connection_id.clone_from(&snapshot.connection_id);
        self.event_seq = snapshot.event_seq;
        self.live_assistant = visible_assistant_text(snapshot.live_message.as_ref());
        self.tools = snapshot
            .active_tool_calls
            .iter()
            .map(|tool| ToolSummary {
                id: tool.id.clone(),
                name: tool.label.clone(),
                status: serialized_name(&tool.status),
            })
            .collect();
        self.stream_active = snapshot.live_message.is_some();
        self.needs_resync = false;
        self.error_strip = snapshot
            .last_error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default();
        if snapshot.last_error.is_some() && snapshot.live_message.is_some() {
            self.stream_active = false;
            self.t_end_ns = now_ns;
        }
        self.assistant_generation = self.assistant_generation.saturating_add(1);
        self.transcript_generation = self.transcript_generation.saturating_add(1);
        self.record_first_token(now_ns);
    }

    pub fn mark_needs_resync(&mut self) {
        self.needs_resync = true;
    }

    pub fn apply_envelope(&mut self, envelope: &EventEnvelope, now_ns: u64) -> ApplyOutcome {
        if envelope.connection_id != self.connection_id || envelope.seq <= self.event_seq {
            return ApplyOutcome::Ignored;
        }
        if envelope.seq != self.event_seq.saturating_add(1) {
            self.needs_resync = true;
            return ApplyOutcome::NeedsResync;
        }

        self.event_seq = envelope.seq;
        match &envelope.payload {
            AcpEvent::UserMessage { .. } => {
                self.live_assistant.clear();
                self.tools.clear();
                self.stream_active = true;
                self.assistant_generation = self.assistant_generation.saturating_add(1);
                self.transcript_generation = self.transcript_generation.saturating_add(1);
                self.t_first_token_ns = 0;
                self.t_end_ns = 0;
            }
            AcpEvent::ContentDelta {
                text,
                parent_tool_use_id: None,
            } => {
                if !text.is_empty() {
                    self.live_assistant.push_str(text);
                    self.assistant_generation = self.assistant_generation.saturating_add(1);
                    self.stream_active = true;
                    self.record_first_token(now_ns);
                }
            }
            AcpEvent::ToolCall {
                tool_call_id,
                title,
                status,
                ..
            } => {
                upsert_tool(&mut self.tools, tool_call_id, Some(title), Some(status));
                self.assistant_generation = self.assistant_generation.saturating_add(1);
                self.stream_active = true;
            }
            AcpEvent::ToolCallUpdate {
                tool_call_id,
                title,
                status,
                ..
            } => {
                upsert_tool(
                    &mut self.tools,
                    tool_call_id,
                    title.as_ref(),
                    status.as_ref(),
                );
                self.assistant_generation = self.assistant_generation.saturating_add(1);
            }
            AcpEvent::TurnComplete { .. } => {
                self.stream_active = false;
                self.tools.clear();
                self.transcript_generation = self.transcript_generation.saturating_add(1);
                self.t_end_ns = now_ns;
            }
            AcpEvent::Error {
                message, terminal, ..
            } => {
                self.error_strip.clone_from(message);
                if self.stream_active || *terminal {
                    self.stream_active = false;
                    self.t_end_ns = now_ns;
                }
            }
            AcpEvent::SessionLoadFailed { message, .. } => {
                self.error_strip.clone_from(message);
                self.stream_active = false;
                self.t_end_ns = now_ns;
            }
            _ => {}
        }
        ApplyOutcome::Applied
    }

    fn record_first_token(&mut self, now_ns: u64) {
        if self.t_first_token_ns == 0 && !self.live_assistant.is_empty() {
            self.t_first_token_ns = now_ns;
        }
    }
}

fn serialized_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn upsert_tool(
    tools: &mut Vec<ToolSummary>,
    id: &str,
    name: Option<&String>,
    status: Option<&String>,
) {
    if let Some(tool) = tools.iter_mut().find(|tool| tool.id == id) {
        if let Some(name) = name {
            tool.name.clone_from(name);
        }
        if let Some(status) = status {
            tool.status.clone_from(status);
        }
        return;
    }
    tools.push(ToolSummary {
        id: id.to_string(),
        name: name.cloned().unwrap_or_default(),
        status: status.cloned().unwrap_or_default(),
    });
}
