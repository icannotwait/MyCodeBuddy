# Task 6 Review Package
BASE: 1b4712060387299c21c6780ccdf3a346fed63864 HEAD: 90372cf57d9fe5d5536842da2ca63e02ce5cdabd
Parent: SKIP all full cargo test
Reviewer routing: codex (separate) + grok

90372cf5 fix(eui): harden live turn recovery boundaries
9cf90829 feat(eui): add recoverable live stream projection
 src-tauri/codeg-eui-core/src/lib.rs                |   9 +
 src-tauri/codeg-eui-core/src/live.rs               | 867 +++++++++++++++++++++
 src-tauri/codeg-eui-core/src/model.rs              | 182 ++++-
 src-tauri/codeg-eui-core/src/perf.rs               |   7 +
 src-tauri/codeg-eui-core/src/runtime.rs            | 119 ++-
 .../codeg-eui-core/tests/interaction_decline.rs    | 329 ++++++++
 src-tauri/codeg-eui-core/tests/live_recovery.rs    | 532 +++++++++++++
 7 files changed, 2017 insertions(+), 28 deletions(-)
diff --git a/src-tauri/codeg-eui-core/src/lib.rs b/src-tauri/codeg-eui-core/src/lib.rs
index 90d4010c..95c98048 100644
--- a/src-tauri/codeg-eui-core/src/lib.rs
+++ b/src-tauri/codeg-eui-core/src/lib.rs
@@ -2,13 +2,22 @@ mod abi;
 mod bootstrap;
 mod commands;
 mod data_root;
+mod live;
 mod model;
+mod perf;
 mod runtime;
 
 pub use abi::*;
 pub use bootstrap::{BootstrapError, EuiBootstrap, StartedServices};
 pub use commands::Operation;
 pub use data_root::{pin_eui_data_root, resolve_eui_data_root, DataRootError, EuiRootInputs};
+pub use live::{
+    decline_interaction, decline_once, pending_interaction, reconcile_snapshot_interactions,
+    snapshot_and_subscribe, snapshot_and_subscribe_observed, ApplyOutcome, AttachPoint,
+    InteractionBackend, InteractionFuture, InteractionKey, LiveAttachment, LiveBackend, LiveError,
+    LiveFuture, LiveProjector, PendingInteraction, Projection, ReceiveOutcome, ToolSummary,
+    INTERACTIVE_PROMPT_NOTICE, LIVE_CONTROL_CAPACITY,
+};
 pub use model::{
     CodegEuiCompletion, CodegEuiSessionSummary, CodegEuiSlice, CompletionStatus, SharedModel,
     CODEG_EUI_COMPLETION_CANCELLED, CODEG_EUI_COMPLETION_ERROR, CODEG_EUI_COMPLETION_OK,
diff --git a/src-tauri/codeg-eui-core/src/live.rs b/src-tauri/codeg-eui-core/src/live.rs
new file mode 100644
index 00000000..44ad63d6
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/live.rs
@@ -0,0 +1,867 @@
+use std::collections::HashSet;
+use std::future::Future;
+use std::pin::Pin;
+use std::sync::atomic::{AtomicU8, Ordering};
+use std::sync::Arc;
+
+use codeg_lib::acp::session_state::visible_assistant_text;
+use codeg_lib::acp::types::PermissionOptionInfo;
+use codeg_lib::acp::{AcpEvent, EventEnvelope, LiveSessionSnapshot, SessionState};
+use serde::{Deserialize, Serialize};
+use tokio::sync::{broadcast, mpsc, watch, RwLock};
+use tokio::task::JoinHandle;
+
+use crate::model::SharedModel;
+use crate::perf::native_timestamp_ns;
+
+pub const LIVE_CONTROL_CAPACITY: usize = 128;
+pub const INTERACTIVE_PROMPT_NOTICE: &str = "Interactive prompts require the main app";
+
+pub type InteractionFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
+pub type LiveFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
+
+pub trait InteractionBackend: Send + Sync {
+    fn respond_permission<'a>(
+        &'a self,
+        connection_id: &'a str,
+        request_id: &'a str,
+        option_id: &'a str,
+    ) -> InteractionFuture<'a>;
+
+    fn cancel_active_turn<'a>(&'a self, connection_id: &'a str) -> InteractionFuture<'a>;
+
+    fn cancel_question<'a>(
+        &'a self,
+        connection_id: &'a str,
+        question_id: &'a str,
+    ) -> InteractionFuture<'a>;
+
+    fn cancel_plan_approvals_by_parent<'a>(
+        &'a self,
+        connection_id: &'a str,
+    ) -> InteractionFuture<'a>;
+}
+
+pub trait LiveBackend: InteractionBackend {
+    fn get_state<'a>(
+        &'a self,
+        connection_id: &'a str,
+    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>>;
+}
+
+pub(crate) struct AppLiveBackend {
+    state: Arc<codeg_lib::app_state::AppState>,
+}
+
+impl AppLiveBackend {
+    pub(crate) fn new(state: Arc<codeg_lib::app_state::AppState>) -> Self {
+        Self { state }
+    }
+}
+
+impl InteractionBackend for AppLiveBackend {
+    fn respond_permission<'a>(
+        &'a self,
+        connection_id: &'a str,
+        request_id: &'a str,
+        option_id: &'a str,
+    ) -> InteractionFuture<'a> {
+        Box::pin(async move {
+            self.state
+                .connection_manager
+                .respond_permission(connection_id, request_id, option_id)
+                .await
+                .map_err(|error| error.to_string())
+        })
+    }
+
+    fn cancel_active_turn<'a>(&'a self, connection_id: &'a str) -> InteractionFuture<'a> {
+        Box::pin(async move {
+            self.state
+                .connection_manager
+                .cancel(&self.state.db.conn, connection_id)
+                .await
+                .map_err(|error| error.to_string())
+        })
+    }
+
+    fn cancel_question<'a>(
+        &'a self,
+        connection_id: &'a str,
+        question_id: &'a str,
+    ) -> InteractionFuture<'a> {
+        Box::pin(async move {
+            self.state
+                .connection_manager
+                .cancel_question(connection_id, question_id)
+                .await;
+            Ok(())
+        })
+    }
+
+    fn cancel_plan_approvals_by_parent<'a>(
+        &'a self,
+        connection_id: &'a str,
+    ) -> InteractionFuture<'a> {
+        Box::pin(async move {
+            self.state
+                .connection_manager
+                .cancel_plan_approvals_by_parent(connection_id)
+                .await;
+            Ok(())
+        })
+    }
+}
+
+impl LiveBackend for AppLiveBackend {
+    fn get_state<'a>(
+        &'a self,
+        connection_id: &'a str,
+    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>> {
+        Box::pin(async move { self.state.connection_manager.get_state(connection_id).await })
+    }
+}
+
+#[derive(Clone, Debug, Hash, PartialEq, Eq)]
+pub enum InteractionKey {
+    Permission(String),
+    Question(String),
+    Plan(String),
+}
+
+#[derive(Clone, Debug)]
+pub enum PendingInteraction {
+    Permission {
+        request_id: String,
+        options: Vec<PermissionOptionInfo>,
+    },
+    Question {
+        question_id: String,
+    },
+    Plan {
+        approval_id: String,
+    },
+}
+
+impl PendingInteraction {
+    pub fn key(&self) -> InteractionKey {
+        match self {
+            Self::Permission { request_id, .. } => InteractionKey::Permission(request_id.clone()),
+            Self::Question { question_id } => InteractionKey::Question(question_id.clone()),
+            Self::Plan { approval_id } => InteractionKey::Plan(approval_id.clone()),
+        }
+    }
+}
+
+pub async fn decline_interaction<B: InteractionBackend + ?Sized>(
+    backend: &B,
+    connection_id: &str,
+    interaction: PendingInteraction,
+) -> Result<(), String> {
+    match interaction {
+        PendingInteraction::Permission {
+            request_id,
+            options,
+        } => match reject_option(&options) {
+            Some(option_id) => {
+                backend
+                    .respond_permission(connection_id, &request_id, option_id)
+                    .await
+            }
+            None => backend.cancel_active_turn(connection_id).await,
+        },
+        PendingInteraction::Question { question_id } => {
+            backend.cancel_question(connection_id, &question_id).await
+        }
+        PendingInteraction::Plan { .. } => {
+            backend.cancel_plan_approvals_by_parent(connection_id).await
+        }
+    }
+}
+
+pub async fn decline_once<B: InteractionBackend + ?Sized>(
+    backend: &B,
+    connection_id: &str,
+    interaction: PendingInteraction,
+    seen: &mut HashSet<InteractionKey>,
+) -> Result<(), String> {
+    if !seen.insert(interaction.key()) {
+        return Ok(());
+    }
+    decline_interaction(backend, connection_id, interaction).await
+}
+
+pub async fn reconcile_snapshot_interactions<B: InteractionBackend + ?Sized>(
+    backend: &B,
+    connection_id: &str,
+    snapshot: &LiveSessionSnapshot,
+    seen: &mut HashSet<InteractionKey>,
+) -> Result<(), String> {
+    if let Some(permission) = &snapshot.pending_permission {
+        decline_once(
+            backend,
+            connection_id,
+            PendingInteraction::Permission {
+                request_id: permission.request_id.clone(),
+                options: permission.options.clone(),
+            },
+            seen,
+        )
+        .await?;
+    }
+    if let Some(question) = &snapshot.pending_question {
+        decline_once(
+            backend,
+            connection_id,
+            PendingInteraction::Question {
+                question_id: question.question_id.clone(),
+            },
+            seen,
+        )
+        .await?;
+    }
+    if let Some(plan) = &snapshot.pending_plan_approval {
+        decline_once(
+            backend,
+            connection_id,
+            PendingInteraction::Plan {
+                approval_id: plan.approval_id.clone(),
+            },
+            seen,
+        )
+        .await?;
+    }
+    Ok(())
+}
+
+pub fn pending_interaction(event: &AcpEvent) -> Option<PendingInteraction> {
+    match event {
+        AcpEvent::PermissionRequest {
+            request_id,
+            options,
+            ..
+        } => Some(PendingInteraction::Permission {
+            request_id: request_id.clone(),
+            options: options.clone(),
+        }),
+        AcpEvent::QuestionRequest { question_id, .. } => Some(PendingInteraction::Question {
+            question_id: question_id.clone(),
+        }),
+        AcpEvent::PlanApprovalRequest { approval_id, .. } => Some(PendingInteraction::Plan {
+            approval_id: approval_id.clone(),
+        }),
+        _ => None,
+    }
+}
+
+fn reject_option(options: &[PermissionOptionInfo]) -> Option<&str> {
+    options
+        .iter()
+        .find(|option| is_reject_or_deny(&option.kind))
+        .or_else(|| {
+            options
+                .iter()
+                .find(|option| is_reject_or_deny(&option.name))
+        })
+        .or_else(|| {
+            options
+                .iter()
+                .find(|option| is_reject_or_deny(&option.option_id))
+        })
+        .map(|option| option.option_id.as_str())
+}
+
+fn is_reject_or_deny(value: &str) -> bool {
+    let normalized = value.trim().to_ascii_lowercase();
+    normalized.contains("reject") || normalized.contains("deny")
+}
+
+pub struct AttachPoint {
+    pub snapshot: LiveSessionSnapshot,
+    pub receiver: broadcast::Receiver<Arc<EventEnvelope>>,
+}
+
+pub async fn snapshot_and_subscribe(state: &Arc<RwLock<SessionState>>) -> AttachPoint {
+    snapshot_and_subscribe_observed(state, || {}).await
+}
+
+#[doc(hidden)]
+pub async fn snapshot_and_subscribe_observed(
+    state: &Arc<RwLock<SessionState>>,
+    while_read_locked: impl FnOnce(),
+) -> AttachPoint {
+    let guard = state.read().await;
+    let snapshot = guard.to_snapshot();
+    while_read_locked();
+    AttachPoint {
+        snapshot,
+        receiver: guard.event_stream().subscribe(),
+    }
+}
+
+#[derive(Debug, thiserror::Error, PartialEq, Eq)]
+pub enum LiveError {
+    #[error("ACP connection not found: {0}")]
+    ConnectionNotFound(String),
+    #[error("live selection changed")]
+    SelectionChanged,
+    #[error("interactive decline failed: {0}")]
+    Interaction(String),
+}
+
+#[derive(Clone, Copy, Debug, PartialEq, Eq)]
+pub enum ReceiveOutcome {
+    Applied,
+    Ignored,
+    Recovered,
+    SelectionChanged,
+    Closed,
+}
+
+pub struct LiveProjector {
+    backend: Arc<dyn LiveBackend>,
+    model: SharedModel,
+    control_capacity: usize,
+}
+
+impl LiveProjector {
+    pub fn new(backend: Arc<dyn LiveBackend>, model: SharedModel) -> Self {
+        Self {
+            backend,
+            model,
+            control_capacity: LIVE_CONTROL_CAPACITY,
+        }
+    }
+
+    pub fn with_control_capacity(
+        backend: Arc<dyn LiveBackend>,
+        model: SharedModel,
+        control_capacity: usize,
+    ) -> Self {
+        Self {
+            backend,
+            model,
+            control_capacity: control_capacity.max(1),
+        }
+    }
+
+    pub async fn attach(
+        &self,
+        connection_id: impl Into<String>,
+        selection_epoch: u64,
+    ) -> Result<LiveAttachment, LiveError> {
+        let connection_id = connection_id.into();
+        let state = self
+            .backend
+            .get_state(&connection_id)
+            .await
+            .ok_or_else(|| LiveError::ConnectionNotFound(connection_id.clone()))?;
+        let attach = snapshot_and_subscribe(&state).await;
+        let mut projection = Projection::default();
+        if !self
+            .model
+            .seed_projection(selection_epoch, &connection_id, &mut projection)
+        {
+            return Err(LiveError::SelectionChanged);
+        }
+        let now_ns = native_timestamp_ns();
+        projection.replace_from_snapshot(&attach.snapshot, now_ns);
+        let mut seen = HashSet::new();
+        if snapshot_has_interaction(&attach.snapshot) {
+            projection.error_strip = INTERACTIVE_PROMPT_NOTICE.to_string();
+        }
+        if let Err(error) = reconcile_snapshot_interactions(
+            self.backend.as_ref(),
+            &connection_id,
+            &attach.snapshot,
+            &mut seen,
+        )
+        .await
+        {
+            terminalize_decline_failure(
+                self.backend.as_ref(),
+                &self.model,
+                selection_epoch,
+                &connection_id,
+                &mut projection,
+                error.clone(),
+            )
+            .await;
+            return Err(LiveError::Interaction(error));
+        }
+        if !self
+            .model
+            .apply_live_projection(selection_epoch, &projection, now_ns)
+        {
+            return Err(LiveError::SelectionChanged);
+        }
+        let pump = ControlPump::start(attach.receiver, self.control_capacity);
+        Ok(LiveAttachment {
+            backend: Arc::clone(&self.backend),
+            model: self.model.clone(),
+            connection_id,
+            selection_epoch,
+            projection,
+            seen,
+            selection_rx: self.model.selection_receiver(),
+            control_capacity: self.control_capacity,
+            pump,
+        })
+    }
+}
+
+pub struct LiveAttachment {
+    backend: Arc<dyn LiveBackend>,
+    model: SharedModel,
+    connection_id: String,
+    selection_epoch: u64,
+    projection: Projection,
+    seen: HashSet<InteractionKey>,
+    selection_rx: watch::Receiver<u64>,
+    control_capacity: usize,
+    pump: ControlPump,
+}
+
+impl LiveAttachment {
+    pub fn snapshot(&self) -> &Projection {
+        &self.projection
+    }
+
+    pub fn queued_control_events(&self) -> usize {
+        self.pump.receiver.len()
+    }
+
+    pub fn recovery_pending(&self) -> bool {
+        self.pump.needs_resync()
+    }
+
+    pub async fn receive_next(&mut self) -> Result<ReceiveOutcome, LiveError> {
+        if *self.selection_rx.borrow() != self.selection_epoch {
+            self.pump.abort();
+            return Ok(ReceiveOutcome::SelectionChanged);
+        }
+        if self.pump.needs_resync() {
+            return self.resync().await;
+        }
+
+        let envelope = tokio::select! {
+            biased;
+            changed = self.selection_rx.changed() => {
+                if changed.is_err() || *self.selection_rx.borrow() != self.selection_epoch {
+                    self.pump.abort();
+                    return Ok(ReceiveOutcome::SelectionChanged);
+                }
+                return Ok(ReceiveOutcome::Ignored);
+            }
+            envelope = self.pump.receiver.recv() => envelope,
+        };
+        let Some(envelope) = envelope else {
+            if self.pump.needs_resync() {
+                return self.resync().await;
+            }
+            return Ok(ReceiveOutcome::Closed);
+        };
+        if self.pump.needs_resync() {
+            return self.resync().await;
+        }
+
+        let now_ns = native_timestamp_ns();
+        match self.projection.apply_envelope(&envelope, now_ns) {
+            ApplyOutcome::NeedsResync => {
+                let _ = self.model.apply_live_projection(
+                    self.selection_epoch,
+                    &self.projection,
+                    now_ns,
+                );
+                self.resync().await
+            }
+            ApplyOutcome::Ignored => Ok(ReceiveOutcome::Ignored),
+            ApplyOutcome::Applied => {
+                if let Some(interaction) = pending_interaction(&envelope.payload) {
+                    self.projection.error_strip = INTERACTIVE_PROMPT_NOTICE.to_string();
+                    if let Err(error) = decline_once(
+                        self.backend.as_ref(),
+                        &self.connection_id,
+                        interaction,
+                        &mut self.seen,
+                    )
+                    .await
+                    {
+                        terminalize_decline_failure(
+                            self.backend.as_ref(),
+                            &self.model,
+                            self.selection_epoch,
+                            &self.connection_id,
+                            &mut self.projection,
+                            error.clone(),
+                        )
+                        .await;
+                        self.pump.abort();
+                        return Err(LiveError::Interaction(error));
+                    }
+                }
+                if !self
+                    .model
+                    .apply_live_projection(self.selection_epoch, &self.projection, now_ns)
+                {
+                    self.pump.abort();
+                    return Ok(ReceiveOutcome::SelectionChanged);
+                }
+                Ok(ReceiveOutcome::Applied)
+            }
+        }
+    }
+
+    pub async fn resync(&mut self) -> Result<ReceiveOutcome, LiveError> {
+        self.projection.mark_needs_resync();
+        let now_ns = native_timestamp_ns();
+        if !self
+            .model
+            .apply_live_projection(self.selection_epoch, &self.projection, now_ns)
+        {
+            self.pump.abort();
+            return Ok(ReceiveOutcome::SelectionChanged);
+        }
+        self.pump.abort();
+
+        let state = self
+            .backend
+            .get_state(&self.connection_id)
+            .await
+            .ok_or_else(|| LiveError::ConnectionNotFound(self.connection_id.clone()))?;
+        let attach = snapshot_and_subscribe(&state).await;
+        self.projection
+            .replace_from_snapshot(&attach.snapshot, native_timestamp_ns());
+        if snapshot_has_interaction(&attach.snapshot) {
+            self.projection.error_strip = INTERACTIVE_PROMPT_NOTICE.to_string();
+        }
+        if let Err(error) = reconcile_snapshot_interactions(
+            self.backend.as_ref(),
+            &self.connection_id,
+            &attach.snapshot,
+            &mut self.seen,
+        )
+        .await
+        {
+            terminalize_decline_failure(
+                self.backend.as_ref(),
+                &self.model,
+                self.selection_epoch,
+                &self.connection_id,
+                &mut self.projection,
+                error.clone(),
+            )
+            .await;
+            return Err(LiveError::Interaction(error));
+        }
+        if !self.model.apply_live_projection(
+            self.selection_epoch,
+            &self.projection,
+            native_timestamp_ns(),
+        ) {
+            return Ok(ReceiveOutcome::SelectionChanged);
+        }
+        self.pump = ControlPump::start(attach.receiver, self.control_capacity);
+        Ok(ReceiveOutcome::Recovered)
+    }
+
+    pub async fn run(mut self) {
+        loop {
+            match self.receive_next().await {
+                Ok(ReceiveOutcome::SelectionChanged | ReceiveOutcome::Closed) | Err(_) => break,
+                Ok(_) => {}
+            }
+        }
+    }
+}
+
+impl Drop for LiveAttachment {
+    fn drop(&mut self) {
+        self.pump.abort();
+    }
+}
+
+const PUMP_RUNNING: u8 = 0;
+const PUMP_LAGGED: u8 = 1;
+const PUMP_OVERFLOW: u8 = 2;
+const PUMP_CLOSED: u8 = 3;
+
+struct ControlPump {
+    receiver: mpsc::Receiver<Arc<EventEnvelope>>,
+    status: Arc<AtomicU8>,
+    task: JoinHandle<()>,
+}
+
+impl ControlPump {
+    fn start(mut source: broadcast::Receiver<Arc<EventEnvelope>>, capacity: usize) -> Self {
+        let (sender, receiver) = mpsc::channel(capacity);
+        let status = Arc::new(AtomicU8::new(PUMP_RUNNING));
+        let pump_status = Arc::clone(&status);
+        let task = tokio::spawn(async move {
+            loop {
+                match source.recv().await {
+                    Ok(envelope) => match sender.try_send(envelope) {
+                        Ok(()) => {}
+                        Err(mpsc::error::TrySendError::Full(_)) => {
+                            pump_status.store(PUMP_OVERFLOW, Ordering::Release);
+                            break;
+                        }
+                        Err(mpsc::error::TrySendError::Closed(_)) => break,
+                    },
+                    Err(broadcast::error::RecvError::Lagged(_)) => {
+                        pump_status.store(PUMP_LAGGED, Ordering::Release);
+                        break;
+                    }
+                    Err(broadcast::error::RecvError::Closed) => {
+                        pump_status.store(PUMP_CLOSED, Ordering::Release);
+                        break;
+                    }
+                }
+            }
+        });
+        Self {
+            receiver,
+            status,
+            task,
+        }
+    }
+
+    fn needs_resync(&self) -> bool {
+        matches!(
+            self.status.load(Ordering::Acquire),
+            PUMP_LAGGED | PUMP_OVERFLOW
+        )
+    }
+
+    fn abort(&self) {
+        self.task.abort();
+    }
+}
+
+fn snapshot_has_interaction(snapshot: &LiveSessionSnapshot) -> bool {
+    snapshot.pending_permission.is_some()
+        || snapshot.pending_question.is_some()
+        || snapshot.pending_plan_approval.is_some()
+}
+
+async fn terminalize_decline_failure(
+    backend: &dyn LiveBackend,
+    model: &SharedModel,
+    selection_epoch: u64,
+    connection_id: &str,
+    projection: &mut Projection,
+    error: String,
+) {
+    let _ = backend.cancel_active_turn(connection_id).await;
+    let now_ns = native_timestamp_ns();
+    projection.error_strip = format!("{INTERACTIVE_PROMPT_NOTICE}: {error}");
+    projection.stream_active = false;
+    projection.t_end_ns = now_ns;
+    let _ = model.apply_live_projection(selection_epoch, projection, now_ns);
+}
+
+#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
+pub struct ToolSummary {
+    pub id: String,
+    pub name: String,
+    pub status: String,
+}
+
+#[derive(Clone, Copy, Debug, PartialEq, Eq)]
+pub enum ApplyOutcome {
+    Applied,
+    Ignored,
+    NeedsResync,
+}
+
+impl ApplyOutcome {
+    pub fn needs_resync(self) -> bool {
+        self == Self::NeedsResync
+    }
+}
+
+#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
+pub struct Projection {
+    pub connection_id: String,
+    pub event_seq: u64,
+    pub transcript_json: Vec<u8>,
+    pub live_assistant: String,
+    pub tools: Vec<ToolSummary>,
+    pub stream_active: bool,
+    pub needs_resync: bool,
+    pub error_strip: String,
+    pub assistant_generation: u64,
+    pub transcript_generation: u64,
+    pub turn_message_id: Option<String>,
+    pub t_first_token_ns: u64,
+    pub t_end_ns: u64,
+}
+
+impl Projection {
+    pub fn replace_from_snapshot(&mut self, snapshot: &LiveSessionSnapshot, now_ns: u64) {
+        let turn_message_id = snapshot
+            .pending_user_message
+            .as_ref()
+            .map(|message| message.message_id.clone());
+        let starts_new_turn = turn_message_id.is_some() && turn_message_id != self.turn_message_id;
+        self.connection_id.clone_from(&snapshot.connection_id);
+        self.event_seq = snapshot.event_seq;
+        self.live_assistant = visible_assistant_text(snapshot.live_message.as_ref());
+        self.tools = snapshot
+            .active_tool_calls
+            .iter()
+            .map(|tool| ToolSummary {
+                id: tool.id.clone(),
+                name: tool.label.clone(),
+                status: serialized_name(&tool.status),
+            })
+            .collect();
+        self.stream_active = snapshot.live_message.is_some();
+        self.needs_resync = false;
+        self.error_strip = snapshot
+            .last_error
+            .as_ref()
+            .map(|error| error.message.clone())
+            .unwrap_or_default();
+        if starts_new_turn {
+            self.t_first_token_ns = 0;
+            self.t_end_ns = 0;
+        }
+        if snapshot.last_error.is_some() && snapshot.live_message.is_some() {
+            self.stream_active = false;
+            if self.t_end_ns == 0 {
+                self.t_end_ns = now_ns;
+            }
+        }
+        self.turn_message_id = turn_message_id;
+        self.assistant_generation = self.assistant_generation.saturating_add(1);
+        self.transcript_generation = self.transcript_generation.saturating_add(1);
+        self.record_first_token(now_ns);
+    }
+
+    pub fn mark_needs_resync(&mut self) {
+        self.needs_resync = true;
+    }
+
+    pub fn apply_envelope(&mut self, envelope: &EventEnvelope, now_ns: u64) -> ApplyOutcome {
+        if envelope.connection_id != self.connection_id || envelope.seq <= self.event_seq {
+            return ApplyOutcome::Ignored;
+        }
+        if envelope.seq != self.event_seq.saturating_add(1) {
+            self.needs_resync = true;
+            return ApplyOutcome::NeedsResync;
+        }
+
+        self.event_seq = envelope.seq;
+        match &envelope.payload {
+            AcpEvent::UserMessage { message_id, .. } => {
+                self.live_assistant.clear();
+                self.tools.clear();
+                self.stream_active = true;
+                self.error_strip.clear();
+                self.assistant_generation = self.assistant_generation.saturating_add(1);
+                self.transcript_generation = self.transcript_generation.saturating_add(1);
+                self.turn_message_id = Some(message_id.clone());
+                self.t_first_token_ns = 0;
+                self.t_end_ns = 0;
+            }
+            AcpEvent::TurnAttemptRollback { .. } => {
+                self.needs_resync = true;
+                return ApplyOutcome::NeedsResync;
+            }
+            AcpEvent::ContentDelta {
+                text,
+                parent_tool_use_id: None,
+            } => {
+                if !text.is_empty() {
+                    self.live_assistant.push_str(text);
+                    self.assistant_generation = self.assistant_generation.saturating_add(1);
+                    self.stream_active = true;
+                    self.record_first_token(now_ns);
+                }
+            }
+            AcpEvent::ToolCall {
+                tool_call_id,
+                title,
+                status,
+                ..
+            } => {
+                upsert_tool(&mut self.tools, tool_call_id, Some(title), Some(status));
+                self.assistant_generation = self.assistant_generation.saturating_add(1);
+                self.stream_active = true;
+            }
+            AcpEvent::ToolCallUpdate {
+                tool_call_id,
+                title,
+                status,
+                ..
+            } => {
+                upsert_tool(
+                    &mut self.tools,
+                    tool_call_id,
+                    title.as_ref(),
+                    status.as_ref(),
+                );
+                self.assistant_generation = self.assistant_generation.saturating_add(1);
+            }
+            AcpEvent::TurnComplete { .. } => {
+                self.stream_active = false;
+                self.tools.clear();
+                self.turn_message_id = None;
+                self.transcript_generation = self.transcript_generation.saturating_add(1);
+                self.t_end_ns = now_ns;
+            }
+            AcpEvent::Error {
+                message, terminal, ..
+            } => {
+                self.error_strip.clone_from(message);
+                if self.stream_active || *terminal {
+                    self.stream_active = false;
+                    self.t_end_ns = now_ns;
+                }
+            }
+            AcpEvent::SessionLoadFailed { message, .. } => {
+                self.error_strip.clone_from(message);
+                self.stream_active = false;
+                self.t_end_ns = now_ns;
+            }
+            _ => {}
+        }
+        ApplyOutcome::Applied
+    }
+
+    fn record_first_token(&mut self, now_ns: u64) {
+        if self.t_first_token_ns == 0 && !self.live_assistant.is_empty() {
+            self.t_first_token_ns = now_ns;
+        }
+    }
+}
+
+fn serialized_name<T: Serialize>(value: &T) -> String {
+    serde_json::to_value(value)
+        .ok()
+        .and_then(|value| value.as_str().map(str::to_owned))
+        .unwrap_or_default()
+}
+
+fn upsert_tool(
+    tools: &mut Vec<ToolSummary>,
+    id: &str,
+    name: Option<&String>,
+    status: Option<&String>,
+) {
+    if let Some(tool) = tools.iter_mut().find(|tool| tool.id == id) {
+        if let Some(name) = name {
+            tool.name.clone_from(name);
+        }
+        if let Some(status) = status {
+            tool.status.clone_from(status);
+        }
+        return;
+    }
+    tools.push(ToolSummary {
+        id: id.to_string(),
+        name: name.cloned().unwrap_or_default(),
+        status: status.cloned().unwrap_or_default(),
+    });
+}
diff --git a/src-tauri/codeg-eui-core/src/model.rs b/src-tauri/codeg-eui-core/src/model.rs
index eace9ea2..9767d8af 100644
--- a/src-tauri/codeg-eui-core/src/model.rs
+++ b/src-tauri/codeg-eui-core/src/model.rs
@@ -3,8 +3,11 @@ use std::num::NonZeroU64;
 use std::sync::atomic::{AtomicBool, Ordering};
 use std::sync::{Arc, Mutex, MutexGuard};
 
+use tokio::sync::watch;
+
 use crate::abi::{CodegEuiFrame, LifecycleState, CODEG_EUI_API_VERSION};
 use crate::commands::Operation;
+use crate::live::Projection;
 use crate::{CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_INTERNAL, CODEG_EUI_ERR_QUEUE_FULL};
 
 pub const CODEG_EUI_COMPLETION_OK: u32 = CompletionStatus::Ok as u32;
@@ -194,6 +197,8 @@ struct ModelState {
     event_seq: u64,
     transcript_json: Vec<u8>,
     live_assistant: Vec<u8>,
+    assistant_generation: u64,
+    transcript_generation: u64,
     stream_active: bool,
     needs_resync: bool,
     error_strip: Vec<u8>,
@@ -203,8 +208,21 @@ struct ModelState {
     ledger: CompletionLedger,
 }
 
-#[derive(Clone, Default)]
-pub struct SharedModel(Arc<Mutex<ModelState>>);
+#[derive(Clone)]
+pub struct SharedModel {
+    state: Arc<Mutex<ModelState>>,
+    selection_tx: watch::Sender<u64>,
+}
+
+impl Default for SharedModel {
+    fn default() -> Self {
+        let (selection_tx, _) = watch::channel(0);
+        Self {
+            state: Arc::new(Mutex::new(ModelState::default())),
+            selection_tx,
+        }
+    }
+}
 
 impl SharedModel {
     pub fn new() -> Self {
@@ -212,7 +230,7 @@ impl SharedModel {
     }
 
     fn lock(&self) -> MutexGuard<'_, ModelState> {
-        self.0.lock().unwrap_or_else(|error| error.into_inner())
+        self.state.lock().unwrap_or_else(|error| error.into_inner())
     }
 
     pub(crate) fn selection_epoch(&self) -> u64 {
@@ -245,17 +263,20 @@ impl SharedModel {
             state.event_seq = 0;
             state.transcript_json.clear();
             state.live_assistant.clear();
+            state.assistant_generation = 0;
+            state.transcript_generation = 0;
             state.stream_active = false;
             state.needs_resync = false;
             state.t0_ns = 0;
             state.t_first_token_ns = 0;
             state.t_end_ns = 0;
+            self.selection_tx.send_replace(captured_epoch);
         }
         Ok(())
     }
 
     pub(crate) fn terminalize(&self, captured_selection_epoch: u64, completion: OwnedCompletion) {
-        self.terminalize_with_update(captured_selection_epoch, completion, None);
+        let _ = self.terminalize_with_update(captured_selection_epoch, completion, None);
     }
 
     pub(crate) fn terminalize_with_update(
@@ -263,10 +284,11 @@ impl SharedModel {
         captured_selection_epoch: u64,
         completion: OwnedCompletion,
         update: Option<ModelUpdate>,
-    ) {
+    ) -> bool {
         let mut state = self.lock();
         let current_selection_epoch = state.selection_epoch;
-        if captured_selection_epoch == current_selection_epoch {
+        let is_current = captured_selection_epoch == current_selection_epoch;
+        if is_current {
             match update {
                 Some(ModelUpdate::Workspace { sessions }) => {
                     state.sessions = sessions;
@@ -279,6 +301,7 @@ impl SharedModel {
                     state.sessions = sessions;
                     state.connection_id = connection_id;
                     state.transcript_json = transcript_json;
+                    state.transcript_generation = state.transcript_generation.saturating_add(1);
                 }
                 None => {}
             }
@@ -288,6 +311,7 @@ impl SharedModel {
             captured_selection_epoch,
             completion,
         );
+        is_current
     }
 
     pub(crate) fn cancel_all(&self) {
@@ -301,6 +325,86 @@ impl SharedModel {
         state.t_end_ns = 0;
     }
 
+    pub(crate) fn selection_receiver(&self) -> watch::Receiver<u64> {
+        self.selection_tx.subscribe()
+    }
+
+    pub(crate) fn seed_projection(
+        &self,
+        selection_epoch: u64,
+        connection_id: &str,
+        projection: &mut Projection,
+    ) -> bool {
+        let mut state = self.lock();
+        if state.selection_epoch != selection_epoch {
+            return false;
+        }
+        if state.connection_id.is_empty() {
+            state.connection_id = connection_id.as_bytes().to_vec();
+        } else if state.connection_id.as_slice() != connection_id.as_bytes() {
+            return false;
+        }
+        projection
+            .transcript_json
+            .clone_from(&state.transcript_json);
+        projection.assistant_generation = state.assistant_generation;
+        projection.transcript_generation = state.transcript_generation;
+        true
+    }
+
+    pub(crate) fn apply_live_projection(
+        &self,
+        selection_epoch: u64,
+        projection: &Projection,
+        observed_at_ns: u64,
+    ) -> bool {
+        let mut state = self.lock();
+        if state.selection_epoch != selection_epoch
+            || state.connection_id.as_slice() != projection.connection_id.as_bytes()
+        {
+            return false;
+        }
+        state.event_seq = projection.event_seq;
+        state.live_assistant = projection.live_assistant.as_bytes().to_vec();
+        state.stream_active = projection.stream_active;
+        state.needs_resync = projection.needs_resync;
+        state.error_strip = projection.error_strip.as_bytes().to_vec();
+        state.assistant_generation = projection.assistant_generation;
+        if projection.transcript_generation >= state.transcript_generation {
+            state
+                .transcript_json
+                .clone_from(&projection.transcript_json);
+            state.transcript_generation = projection.transcript_generation;
+        }
+        if state.t0_ns != 0 && state.t_first_token_ns == 0 && !projection.live_assistant.is_empty()
+        {
+            state.t_first_token_ns = observed_at_ns;
+        }
+        if projection.t_end_ns >= state.t0_ns && projection.t_end_ns != 0 {
+            state.t_end_ns = projection.t_end_ns;
+        }
+        true
+    }
+
+    pub(crate) fn set_live_error(
+        &self,
+        selection_epoch: u64,
+        connection_id: &str,
+        message: String,
+        ended_at_ns: u64,
+    ) -> bool {
+        let mut state = self.lock();
+        if state.selection_epoch != selection_epoch
+            || state.connection_id.as_slice() != connection_id.as_bytes()
+        {
+            return false;
+        }
+        state.error_strip = message.into_bytes();
+        state.stream_active = false;
+        state.t_end_ns = ended_at_ns;
+        true
+    }
+
     pub fn set_error_strip(&self, message: Vec<u8>) {
         self.lock().error_strip = message;
     }
@@ -471,9 +575,12 @@ fn ptr_or_null<T>(values: &[T]) -> *const T {
 #[cfg(test)]
 mod tests {
     use std::num::NonZeroU64;
+    use std::sync::atomic::AtomicBool;
 
     use super::{CompletionStatus, OwnedCompletion, SharedModel};
+    use crate::abi::LifecycleState;
     use crate::commands::Operation;
+    use crate::live::Projection;
 
     #[test]
     fn accepted_workspace_and_session_changes_advance_the_selection_epoch() {
@@ -543,4 +650,67 @@ mod tests {
             OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
         );
     }
+
+    #[test]
+    fn live_markers_and_resync_visibility_are_frame_backed() {
+        let model = SharedModel::new();
+        model.lock().connection_id = b"c1".to_vec();
+        model.record_send_accepted(100);
+        let mut projection = Projection {
+            connection_id: "c1".to_string(),
+            event_seq: 1,
+            live_assistant: "hello".to_string(),
+            assistant_generation: 1,
+            stream_active: true,
+            ..Projection::default()
+        };
+
+        assert!(model.apply_live_projection(0, &projection, 150));
+        let (first, _) = model.build_frame(false, &AtomicBool::new(false));
+        let first = first.as_abi(LifecycleState::Running, 1, false);
+        assert_eq!(first.t_first_token_ns, 150);
+        assert_eq!(first.t_end_ns, 0);
+
+        projection.needs_resync = true;
+        assert!(model.apply_live_projection(0, &projection, 160));
+        let (resyncing, _) = model.build_frame(false, &AtomicBool::new(false));
+        let resyncing = resyncing.as_abi(LifecycleState::Running, 2, false);
+        assert_eq!(resyncing.needs_resync, 1);
+        assert_eq!(resyncing.t_first_token_ns, 150);
+
+        projection.needs_resync = false;
+        projection.event_seq = 2;
+        projection.t_end_ns = 200;
+        projection.stream_active = false;
+        assert!(model.apply_live_projection(0, &projection, 200));
+        let (complete, _) = model.build_frame(false, &AtomicBool::new(false));
+        let complete = complete.as_abi(LifecycleState::Running, 3, false);
+        assert_eq!(complete.needs_resync, 0);
+        assert_eq!(complete.t_first_token_ns, 150);
+        assert_eq!(complete.t_end_ns, 200);
+    }
+
+    #[test]
+    fn old_live_projection_cannot_overwrite_a_new_selection() {
+        let model = SharedModel::new();
+        model.lock().connection_id = b"old".to_vec();
+        let projection = Projection {
+            connection_id: "old".to_string(),
+            event_seq: 9,
+            live_assistant: "stale".to_string(),
+            assistant_generation: 1,
+            ..Projection::default()
+        };
+        let selection_request = NonZeroU64::new(91).unwrap();
+
+        model
+            .reserve(selection_request, Operation::SetWorkspace, 0)
+            .unwrap();
+
+        assert!(!model.apply_live_projection(0, &projection, 50));
+        let (frame, _) = model.build_frame(false, &AtomicBool::new(false));
+        let frame = frame.as_abi(LifecycleState::Running, 1, false);
+        assert_eq!(frame.event_seq, 0);
+        assert_eq!(frame.live_assistant.len, 0);
+    }
 }
diff --git a/src-tauri/codeg-eui-core/src/perf.rs b/src-tauri/codeg-eui-core/src/perf.rs
new file mode 100644
index 00000000..e73ac056
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/perf.rs
@@ -0,0 +1,7 @@
+pub(crate) fn native_timestamp_ns() -> u64 {
+    std::time::SystemTime::now()
+        .duration_since(std::time::UNIX_EPOCH)
+        .unwrap_or_default()
+        .as_nanos()
+        .min(u64::MAX as u128) as u64
+}
diff --git a/src-tauri/codeg-eui-core/src/runtime.rs b/src-tauri/codeg-eui-core/src/runtime.rs
index abbeea7f..87146000 100644
--- a/src-tauri/codeg-eui-core/src/runtime.rs
+++ b/src-tauri/codeg-eui-core/src/runtime.rs
@@ -14,7 +14,9 @@ use tokio::sync::{mpsc, watch};
 use tokio::task::{Id, JoinHandle, JoinSet};
 
 use crate::commands::{CommandPayload, Operation, RuntimeCommand};
+use crate::live::{AppLiveBackend, LiveBackend, LiveError, LiveProjector};
 use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary, SharedModel};
+use crate::perf::native_timestamp_ns;
 use crate::{
     EuiBootstrap, CODEG_EUI_COMMAND_QUEUE_CAPACITY, CODEG_EUI_ERR_INTERNAL,
     CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
@@ -25,6 +27,7 @@ pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<CoreResult, Stri
 pub(crate) struct CoreResult {
     payload: Vec<u8>,
     update: Option<ModelUpdate>,
+    live_connection_id: Option<String>,
 }
 
 impl CoreResult {
@@ -32,6 +35,7 @@ impl CoreResult {
         Self {
             payload,
             update: None,
+            live_connection_id: None,
         }
     }
 }
@@ -152,6 +156,7 @@ impl CoreOps for AppCoreOps {
             Ok(CoreResult {
                 payload,
                 update: Some(ModelUpdate::Workspace { sessions }),
+                live_connection_id: None,
             })
         })
     }
@@ -290,6 +295,7 @@ fn selection_result(
         serde_json::to_vec(&selection.transcript).map_err(|error| error.to_string())?;
     let sessions = owned_sessions(&workspace.sessions);
     let connection_id = selection.connection_id.as_bytes().to_vec();
+    let live_connection_id = selection.connection_id.clone();
     let mut current = context.lock().unwrap_or_else(|error| error.into_inner());
     if selection_epoch == current.selection_epoch {
         current.selection_epoch = selection_epoch;
@@ -303,6 +309,7 @@ fn selection_result(
             connection_id,
             transcript_json,
         }),
+        live_connection_id: Some(live_connection_id),
     })
 }
 
@@ -368,6 +375,8 @@ impl RuntimeOwner {
             state: Arc::clone(&bootstrap.state),
             context: Arc::new(Mutex::new(AppCommandContext::default())),
         });
+        let live_backend: Arc<dyn LiveBackend> =
+            Arc::new(AppLiveBackend::new(Arc::clone(&bootstrap.state)));
         let worker = bootstrap.runtime_handle().spawn(run_worker(
             command_rx,
             shutdown_rx,
@@ -376,6 +385,7 @@ impl RuntimeOwner {
             Arc::clone(&admission),
             Arc::clone(&quiesced),
             Arc::clone(&core_ops),
+            Some(live_backend),
         ));
 
         Self {
@@ -460,14 +470,6 @@ impl RuntimeOwner {
     }
 }
 
-fn native_timestamp_ns() -> u64 {
-    std::time::SystemTime::now()
-        .duration_since(std::time::UNIX_EPOCH)
-        .unwrap_or_default()
-        .as_nanos()
-        .min(u64::MAX as u128) as u64
-}
-
 fn next_request_id() -> Result<NonZeroU64, i32> {
     let value = NEXT_REQUEST_ID
         .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
@@ -485,6 +487,7 @@ async fn run_worker(
     admission: Arc<Mutex<()>>,
     quiesced: Arc<AtomicBool>,
     core_ops: Arc<dyn CoreOps>,
+    live_backend: Option<Arc<dyn LiveBackend>>,
 ) {
     let _exit_guard = WorkerExitGuard {
         model: model.clone(),
@@ -493,6 +496,7 @@ async fn run_worker(
     };
     let mut tasks = JoinSet::new();
     let mut metadata = HashMap::<Id, CommandMetadata>::new();
+    let mut live_task: Option<JoinHandle<()>> = None;
 
     loop {
         tokio::select! {
@@ -503,7 +507,33 @@ async fn run_worker(
                 }
             }
             completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
-                terminalize_task(&model, &mut metadata, completed);
+                if let Some(selection) = terminalize_task(&model, &mut metadata, completed) {
+                    if let Some(task) = live_task.take() {
+                        task.abort();
+                    }
+                    if let Some(backend) = live_backend.as_ref() {
+                        let backend = Arc::clone(backend);
+                        let live_model = model.clone();
+                        live_task = Some(tokio::spawn(async move {
+                            let projector = LiveProjector::new(backend, live_model.clone());
+                            match projector
+                                .attach(&selection.connection_id, selection.selection_epoch)
+                                .await
+                            {
+                                Ok(attachment) => attachment.run().await,
+                                Err(LiveError::SelectionChanged) => {}
+                                Err(error) => {
+                                    let _ = live_model.set_live_error(
+                                        selection.selection_epoch,
+                                        &selection.connection_id,
+                                        error.to_string(),
+                                        native_timestamp_ns(),
+                                    );
+                                }
+                            }
+                        }));
+                    }
+                }
             }
             command = commands.recv() => {
                 let Some(command) = command else {
@@ -527,6 +557,10 @@ async fn run_worker(
     }
 
     commands.close();
+    if let Some(task) = live_task {
+        task.abort();
+        let _ = task.await;
+    }
     tasks.abort_all();
     while let Some(completed) = tasks.join_next_with_id().await {
         if let Ok((id, _)) = &completed {
@@ -543,13 +577,18 @@ async fn run_worker(
         .await;
 }
 
+struct LiveSelection {
+    connection_id: String,
+    selection_epoch: u64,
+}
+
 fn terminalize_task(
     model: &SharedModel,
     metadata: &mut HashMap<Id, CommandMetadata>,
     completed: Option<Result<(Id, Result<CoreResult, String>), tokio::task::JoinError>>,
-) {
+) -> Option<LiveSelection> {
     let Some(completed) = completed else {
-        return;
+        return None;
     };
     let (task_id, result) = match completed {
         Ok((task_id, result)) => (task_id, result),
@@ -562,15 +601,29 @@ fn terminalize_task(
         .remove(&task_id)
         .expect("metadata exists for every worker task");
     match result {
-        Ok(result) => model.terminalize_with_update(
-            command.selection_epoch,
-            OwnedCompletion::ok(command.request_id, command.op, result.payload),
-            result.update,
-        ),
-        Err(error) => model.terminalize(
-            command.selection_epoch,
-            OwnedCompletion::error(command.request_id, command.op, error),
-        ),
+        Ok(result) => {
+            let live_connection_id = result.live_connection_id;
+            let is_current = model.terminalize_with_update(
+                command.selection_epoch,
+                OwnedCompletion::ok(command.request_id, command.op, result.payload),
+                result.update,
+            );
+            if is_current {
+                live_connection_id.map(|connection_id| LiveSelection {
+                    connection_id,
+                    selection_epoch: command.selection_epoch,
+                })
+            } else {
+                None
+            }
+        }
+        Err(error) => {
+            model.terminalize(
+                command.selection_epoch,
+                OwnedCompletion::error(command.request_id, command.op, error),
+            );
+            None
+        }
     }
 }
 
@@ -641,8 +694,8 @@ mod tests {
     use tokio::task::JoinSet;
 
     use super::{
-        capture_command_context, execute_command, run_worker, terminalize_task, AppCommandContext,
-        CommandContext, CommandMetadata, CoreFuture, CoreOps, CoreResult,
+        capture_command_context, execute_command, run_worker, selection_result, terminalize_task,
+        AppCommandContext, CommandContext, CommandMetadata, CoreFuture, CoreOps, CoreResult,
     };
     use crate::commands::{CommandPayload, Operation, RuntimeCommand};
     use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary};
@@ -677,6 +730,24 @@ mod tests {
         }
     }
 
+    #[test]
+    fn successful_selection_requests_live_attachment_for_its_connection() {
+        let workspace = test_workspace(11, "/workspace");
+        let selection = test_selection(&workspace, 101, "connection-live");
+        let context = Arc::new(Mutex::new(AppCommandContext {
+            selection_epoch: 7,
+            workspace: Some(workspace.clone()),
+            selection: None,
+        }));
+
+        let result = selection_result(context, 7, workspace, selection).unwrap();
+
+        assert_eq!(
+            result.live_connection_id.as_deref(),
+            Some("connection-live")
+        );
+    }
+
     #[test]
     fn accepted_commands_keep_their_original_workspace_and_selection() {
         let workspace_a = test_workspace(11, "/workspace-a");
@@ -908,6 +979,7 @@ mod tests {
             Arc::new(SlowProbeOps {
                 gate: Arc::clone(&gate),
             }),
+            None,
         ));
         command_tx
             .send(RuntimeCommand {
@@ -979,6 +1051,7 @@ mod tests {
                         connection_id: b"old".to_vec(),
                         transcript_json: b"[]".to_vec(),
                     }),
+                    live_connection_id: Some("old".to_string()),
                 })
             })
         }
@@ -1033,6 +1106,7 @@ mod tests {
                 started: Arc::clone(&started),
                 gate: Arc::clone(&gate),
             }),
+            None,
         ));
         command_tx
             .send(RuntimeCommand {
@@ -1179,6 +1253,7 @@ mod tests {
                 gate: Arc::clone(&gate),
                 linked: Arc::clone(&linked),
             }),
+            None,
         ));
         command_tx
             .send(RuntimeCommand {
diff --git a/src-tauri/codeg-eui-core/tests/interaction_decline.rs b/src-tauri/codeg-eui-core/tests/interaction_decline.rs
new file mode 100644
index 00000000..7b9e8976
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/interaction_decline.rs
@@ -0,0 +1,329 @@
+use std::collections::HashSet;
+use std::future::Future;
+use std::pin::Pin;
+use std::sync::{Arc, Mutex};
+use std::time::Duration;
+
+use codeg_eui_core::{
+    decline_interaction, decline_once, reconcile_snapshot_interactions, InteractionBackend,
+    InteractionKey, LiveBackend, LiveFuture, LiveProjector, PendingInteraction, ReceiveOutcome,
+    SharedModel, INTERACTIVE_PROMPT_NOTICE,
+};
+use codeg_lib::acp::types::PermissionOptionInfo;
+use codeg_lib::acp::{AcpEvent, EventEnvelope, SessionState};
+use codeg_lib::models::AgentType;
+use tokio::sync::RwLock;
+
+type ActionFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
+
+#[derive(Clone, Debug, PartialEq, Eq)]
+enum Action {
+    Permission(String, String),
+    CancelTurn,
+    Question(String),
+    Plan,
+}
+
+#[derive(Clone, Default)]
+struct RecordingBackend {
+    actions: Arc<Mutex<Vec<Action>>>,
+    state: Option<Arc<RwLock<SessionState>>>,
+}
+
+impl RecordingBackend {
+    fn actions(&self) -> Vec<Action> {
+        self.actions.lock().unwrap().clone()
+    }
+
+    fn push(&self, action: Action) -> ActionFuture<'_> {
+        self.actions.lock().unwrap().push(action);
+        Box::pin(async { Ok(()) })
+    }
+
+    fn with_state(state: Arc<RwLock<SessionState>>) -> Self {
+        Self {
+            actions: Arc::new(Mutex::new(Vec::new())),
+            state: Some(state),
+        }
+    }
+}
+
+impl LiveBackend for RecordingBackend {
+    fn get_state<'a>(
+        &'a self,
+        connection_id: &'a str,
+    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>> {
+        let state = self.state.clone();
+        Box::pin(async move {
+            let state = state?;
+            let matches = state.read().await.connection_id == connection_id;
+            matches.then_some(state)
+        })
+    }
+}
+
+impl InteractionBackend for RecordingBackend {
+    fn respond_permission<'a>(
+        &'a self,
+        _connection_id: &'a str,
+        request_id: &'a str,
+        option_id: &'a str,
+    ) -> ActionFuture<'a> {
+        self.push(Action::Permission(
+            request_id.to_string(),
+            option_id.to_string(),
+        ))
+    }
+
+    fn cancel_active_turn<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
+        self.push(Action::CancelTurn)
+    }
+
+    fn cancel_question<'a>(
+        &'a self,
+        _connection_id: &'a str,
+        question_id: &'a str,
+    ) -> ActionFuture<'a> {
+        self.push(Action::Question(question_id.to_string()))
+    }
+
+    fn cancel_plan_approvals_by_parent<'a>(&'a self, _connection_id: &'a str) -> ActionFuture<'a> {
+        self.push(Action::Plan)
+    }
+}
+
+fn option(option_id: &str, name: &str, kind: &str) -> PermissionOptionInfo {
+    PermissionOptionInfo {
+        option_id: option_id.to_string(),
+        name: name.to_string(),
+        kind: kind.to_string(),
+    }
+}
+
+fn session_state(connection_id: &str) -> Arc<RwLock<SessionState>> {
+    Arc::new(RwLock::new(SessionState::new(
+        connection_id.to_string(),
+        AgentType::Codex,
+        None,
+        "eui-test".to_string(),
+        None,
+    )))
+}
+
+async fn emit(state: &Arc<RwLock<SessionState>>, payload: AcpEvent) {
+    let (stream, envelope) = {
+        let mut guard = state.write().await;
+        let _ = guard.apply_event(&payload);
+        guard.event_seq += 1;
+        let envelope = Arc::new(EventEnvelope {
+            seq: guard.event_seq,
+            connection_id: guard.connection_id.clone(),
+            payload,
+        });
+        (guard.event_stream(), envelope)
+    };
+    stream.send(envelope);
+}
+
+#[tokio::test]
+async fn permission_uses_kind_then_name_then_id_or_cancels_turn() {
+    let backend = RecordingBackend::default();
+    decline_interaction(
+        &backend,
+        "c1",
+        PendingInteraction::Permission {
+            request_id: "r1".to_string(),
+            options: vec![
+                option("reject-by-id", "Allow", "allow_once"),
+                option("deny-by-name", "Deny", "allow_once"),
+                option("kind-wins", "Allow", "reject_once"),
+            ],
+        },
+    )
+    .await
+    .unwrap();
+    decline_interaction(
+        &backend,
+        "c1",
+        PendingInteraction::Permission {
+            request_id: "r2".to_string(),
+            options: Vec::new(),
+        },
+    )
+    .await
+    .unwrap();
+
+    assert_eq!(
+        backend.actions(),
+        vec![
+            Action::Permission("r1".to_string(), "kind-wins".to_string()),
+            Action::CancelTurn,
+        ]
+    );
+}
+
+#[tokio::test]
+async fn snapshot_and_live_event_share_one_deduplicated_decline_policy() {
+    let backend = RecordingBackend::default();
+    let mut state = SessionState::new(
+        "c1".to_string(),
+        AgentType::Codex,
+        None,
+        "eui-test".to_string(),
+        None,
+    );
+    let _ = state.apply_event(&AcpEvent::PermissionRequest {
+        request_id: "p1".to_string(),
+        tool_call: serde_json::json!({}),
+        options: vec![option("deny", "Deny", "reject_once")],
+    });
+    let _ = state.apply_event(&AcpEvent::QuestionRequest {
+        question_id: "q1".to_string(),
+        questions: Vec::new(),
+    });
+    let _ = state.apply_event(&AcpEvent::PlanApprovalRequest {
+        approval_id: "a1".to_string(),
+        tool_call_id: "tool-1".to_string(),
+        plan_markdown: "plan".to_string(),
+    });
+    let snapshot = state.to_snapshot();
+    let mut seen = HashSet::<InteractionKey>::new();
+
+    reconcile_snapshot_interactions(&backend, "c1", &snapshot, &mut seen)
+        .await
+        .unwrap();
+    reconcile_snapshot_interactions(&backend, "c1", &snapshot, &mut seen)
+        .await
+        .unwrap();
+    decline_once(
+        &backend,
+        "c1",
+        PendingInteraction::Question {
+            question_id: "q1".to_string(),
+        },
+        &mut seen,
+    )
+    .await
+    .unwrap();
+
+    assert_eq!(
+        backend.actions(),
+        vec![
+            Action::Permission("p1".to_string(), "deny".to_string()),
+            Action::Question("q1".to_string()),
+            Action::Plan,
+        ]
+    );
+}
+
+#[tokio::test]
+async fn snapshot_pending_interactions_decline_before_event_resume_once() {
+    let state = session_state("snapshot-only");
+    {
+        let mut guard = state.write().await;
+        let _ = guard.apply_event(&AcpEvent::PermissionRequest {
+            request_id: "p1".to_string(),
+            tool_call: serde_json::json!({}),
+            options: vec![option("deny", "Deny", "reject_once")],
+        });
+        let _ = guard.apply_event(&AcpEvent::QuestionRequest {
+            question_id: "q1".to_string(),
+            questions: Vec::new(),
+        });
+        let _ = guard.apply_event(&AcpEvent::PlanApprovalRequest {
+            approval_id: "a1".to_string(),
+            tool_call_id: "tool-1".to_string(),
+            plan_markdown: "plan".to_string(),
+        });
+    }
+    let backend = RecordingBackend::with_state(Arc::clone(&state));
+    let projector = LiveProjector::new(Arc::new(backend.clone()), SharedModel::new());
+
+    let mut attachment = projector.attach("snapshot-only", 0).await.unwrap();
+
+    assert_eq!(
+        backend.actions(),
+        vec![
+            Action::Permission("p1".to_string(), "deny".to_string()),
+            Action::Question("q1".to_string()),
+            Action::Plan,
+        ]
+    );
+    assert_eq!(attachment.queued_control_events(), 0);
+    assert_eq!(attachment.snapshot().error_strip, INTERACTIVE_PROMPT_NOTICE);
+
+    attachment.resync().await.unwrap();
+    assert_eq!(backend.actions().len(), 3);
+}
+
+#[tokio::test]
+async fn live_interactions_decline_and_turn_reaches_terminal_marker() {
+    let state = session_state("live-interactions");
+    let backend = RecordingBackend::with_state(Arc::clone(&state));
+    let projector = LiveProjector::new(Arc::new(backend.clone()), SharedModel::new());
+    let mut attachment = projector.attach("live-interactions", 0).await.unwrap();
+
+    emit(
+        &state,
+        AcpEvent::QuestionRequest {
+            question_id: "q-live".to_string(),
+            questions: Vec::new(),
+        },
+    )
+    .await;
+    assert_eq!(
+        tokio::time::timeout(Duration::from_secs(2), attachment.receive_next())
+            .await
+            .unwrap()
+            .unwrap(),
+        ReceiveOutcome::Applied
+    );
+    emit(
+        &state,
+        AcpEvent::PlanApprovalRequest {
+            approval_id: "a-live".to_string(),
+            tool_call_id: "tool-live".to_string(),
+            plan_markdown: "plan".to_string(),
+        },
+    )
+    .await;
+    attachment.receive_next().await.unwrap();
+    emit(
+        &state,
+        AcpEvent::PermissionRequest {
+            request_id: "p-live".to_string(),
+            tool_call: serde_json::json!({}),
+            options: vec![option("deny-live", "Deny", "reject_once")],
+        },
+    )
+    .await;
+    attachment.receive_next().await.unwrap();
+    emit(
+        &state,
+        AcpEvent::TurnComplete {
+            session_id: "session".to_string(),
+            stop_reason: "end_turn".to_string(),
+            agent_type: "codex".to_string(),
+            mark_awaiting_reply: true,
+            termination_source: None,
+            provider_turn_id: None,
+        },
+    )
+    .await;
+    tokio::time::timeout(Duration::from_secs(2), attachment.receive_next())
+        .await
+        .unwrap()
+        .unwrap();
+
+    assert_eq!(
+        backend.actions(),
+        vec![
+            Action::Question("q-live".to_string()),
+            Action::Plan,
+            Action::Permission("p-live".to_string(), "deny-live".to_string()),
+        ]
+    );
+    assert_eq!(attachment.snapshot().error_strip, INTERACTIVE_PROMPT_NOTICE);
+    assert!(attachment.snapshot().t_end_ns > 0);
+    assert!(!attachment.snapshot().stream_active);
+}
diff --git a/src-tauri/codeg-eui-core/tests/live_recovery.rs b/src-tauri/codeg-eui-core/tests/live_recovery.rs
new file mode 100644
index 00000000..99c3fe1c
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/live_recovery.rs
@@ -0,0 +1,532 @@
+use std::sync::atomic::{AtomicUsize, Ordering};
+use std::sync::{Arc, Barrier};
+
+use codeg_eui_core::{
+    snapshot_and_subscribe_observed, InteractionBackend, InteractionFuture, LiveBackend,
+    LiveFuture, LiveProjector, Projection, ReceiveOutcome, SharedModel,
+};
+use codeg_lib::acp::types::PermissionOptionInfo;
+use codeg_lib::acp::{AcpEvent, EventEnvelope, SessionState};
+use codeg_lib::models::AgentType;
+use tokio::sync::RwLock;
+
+fn state(connection_id: &str) -> Arc<RwLock<SessionState>> {
+    Arc::new(RwLock::new(SessionState::new(
+        connection_id.to_string(),
+        AgentType::Codex,
+        None,
+        "eui-test".to_string(),
+        None,
+    )))
+}
+
+async fn emit(state: &Arc<RwLock<SessionState>>, payload: AcpEvent) {
+    let (stream, envelope) = {
+        let mut guard = state.write().await;
+        let _ = guard.apply_event(&payload);
+        guard.event_seq += 1;
+        let envelope = Arc::new(EventEnvelope {
+            seq: guard.event_seq,
+            connection_id: guard.connection_id.clone(),
+            payload,
+        });
+        (guard.event_stream(), envelope)
+    };
+    stream.send(envelope);
+}
+
+#[derive(Clone)]
+struct StateBackend {
+    state: Arc<RwLock<SessionState>>,
+    declines: Arc<AtomicUsize>,
+}
+
+impl InteractionBackend for StateBackend {
+    fn respond_permission<'a>(
+        &'a self,
+        _connection_id: &'a str,
+        _request_id: &'a str,
+        _option_id: &'a str,
+    ) -> InteractionFuture<'a> {
+        self.declines.fetch_add(1, Ordering::SeqCst);
+        Box::pin(async { Ok(()) })
+    }
+
+    fn cancel_active_turn<'a>(&'a self, _connection_id: &'a str) -> InteractionFuture<'a> {
+        Box::pin(async { Ok(()) })
+    }
+
+    fn cancel_question<'a>(
+        &'a self,
+        _connection_id: &'a str,
+        _question_id: &'a str,
+    ) -> InteractionFuture<'a> {
+        Box::pin(async { Ok(()) })
+    }
+
+    fn cancel_plan_approvals_by_parent<'a>(
+        &'a self,
+        _connection_id: &'a str,
+    ) -> InteractionFuture<'a> {
+        Box::pin(async { Ok(()) })
+    }
+}
+
+impl LiveBackend for StateBackend {
+    fn get_state<'a>(
+        &'a self,
+        connection_id: &'a str,
+    ) -> LiveFuture<'a, Option<Arc<RwLock<SessionState>>>> {
+        let state = Arc::clone(&self.state);
+        Box::pin(async move {
+            let matches = state.read().await.connection_id == connection_id;
+            matches.then_some(state)
+        })
+    }
+}
+
+#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
+async fn attach_cannot_miss_event_between_snapshot_and_subscribe() {
+    let state = state("atomic");
+    let entered = Arc::new(Barrier::new(2));
+    let release = Arc::new(Barrier::new(2));
+    let attach_state = Arc::clone(&state);
+    let attach_entered = Arc::clone(&entered);
+    let attach_release = Arc::clone(&release);
+    let attach = tokio::spawn(async move {
+        snapshot_and_subscribe_observed(&attach_state, move || {
+            attach_entered.wait();
+            attach_release.wait();
+        })
+        .await
+    });
+
+    entered.wait();
+    let writer_attempted = Arc::new(tokio::sync::Notify::new());
+    let writer_state = Arc::clone(&state);
+    let writer_signal = Arc::clone(&writer_attempted);
+    let writer = tokio::spawn(async move {
+        writer_signal.notify_one();
+        emit(
+            &writer_state,
+            AcpEvent::ContentDelta {
+                text: "hello".to_string(),
+                parent_tool_use_id: None,
+            },
+        )
+        .await;
+    });
+    writer_attempted.notified().await;
+    release.wait();
+
+    let mut attach = attach.await.unwrap();
+    writer.await.unwrap();
+
+    assert_eq!(attach.snapshot.event_seq, 0);
+    let event = attach.receiver.recv().await.expect("subscribed event");
+    assert_eq!(event.seq, 1);
+}
+
+#[test]
+fn sequence_gap_marks_projection_for_authoritative_resync() {
+    let state = state("gap");
+    let snapshot = state.blocking_read().to_snapshot();
+    let mut projection = Projection::default();
+    projection.replace_from_snapshot(&snapshot, 10);
+
+    let outcome = projection.apply_envelope(
+        &EventEnvelope {
+            seq: 2,
+            connection_id: "gap".to_string(),
+            payload: AcpEvent::ContentDelta {
+                text: "must-not-apply".to_string(),
+                parent_tool_use_id: None,
+            },
+        },
+        20,
+    );
+
+    assert!(outcome.needs_resync());
+    assert!(projection.needs_resync);
+    assert_eq!(projection.event_seq, 0);
+    assert!(projection.live_assistant.is_empty());
+}
+
+#[test]
+fn user_message_starts_a_new_assistant_generation_and_marker_window() {
+    let mut projection = Projection {
+        connection_id: "turns".to_string(),
+        event_seq: 4,
+        live_assistant: "old answer".to_string(),
+        assistant_generation: 8,
+        transcript_generation: 3,
+        t_first_token_ns: 10,
+        t_end_ns: 20,
+        error_strip: "old error".to_string(),
+        ..Projection::default()
+    };
+
+    projection.apply_envelope(
+        &EventEnvelope {
+            seq: 5,
+            connection_id: "turns".to_string(),
+            payload: AcpEvent::UserMessage {
+                message_id: "message-2".to_string(),
+                blocks: Vec::new(),
+            },
+        },
+        30,
+    );
+
+    assert!(projection.live_assistant.is_empty());
+    assert_eq!(projection.t_first_token_ns, 0);
+    assert_eq!(projection.t_end_ns, 0);
+    assert_eq!(projection.assistant_generation, 9);
+    assert_eq!(projection.transcript_generation, 4);
+    assert!(projection.error_strip.is_empty());
+}
+
+#[test]
+fn turn_attempt_rollback_forces_authoritative_recovery() {
+    let mut projection = Projection {
+        connection_id: "rollback".to_string(),
+        live_assistant: "speculative".to_string(),
+        stream_active: true,
+        ..Projection::default()
+    };
+
+    let outcome = projection.apply_envelope(
+        &EventEnvelope {
+            seq: 1,
+            connection_id: "rollback".to_string(),
+            payload: AcpEvent::TurnAttemptRollback { attempt: 2 },
+        },
+        30,
+    );
+
+    assert!(outcome.needs_resync());
+    assert!(projection.needs_resync);
+}
+
+#[test]
+fn active_turn_hard_error_sets_terminal_marker_without_connection_death() {
+    let mut projection = Projection {
+        connection_id: "error".to_string(),
+        stream_active: true,
+        ..Projection::default()
+    };
+
+    projection.apply_envelope(
+        &EventEnvelope {
+            seq: 1,
+            connection_id: "error".to_string(),
+            payload: AcpEvent::Error {
+                message: "turn failed".to_string(),
+                agent_type: "codex".to_string(),
+                code: Some("turn_failed".to_string()),
+                terminal: false,
+            },
+        },
+        44,
+    );
+
+    assert_eq!(projection.t_end_ns, 44);
+    assert!(!projection.stream_active);
+    assert_eq!(projection.error_strip, "turn failed");
+}
+
+#[tokio::test]
+async fn snapshot_replacement_coalesces_text_and_reduces_tool_summaries() {
+    let state = state("snapshot");
+    emit(
+        &state,
+        AcpEvent::ContentDelta {
+            text: "hel".to_string(),
+            parent_tool_use_id: None,
+        },
+    )
+    .await;
+    emit(
+        &state,
+        AcpEvent::ContentDelta {
+            text: "lo".to_string(),
+            parent_tool_use_id: None,
+        },
+    )
+    .await;
+    emit(
+        &state,
+        AcpEvent::ToolCall {
+            tool_call_id: "tool-1".to_string(),
+            title: "Read file".to_string(),
+            kind: "read".to_string(),
+            status: "in_progress".to_string(),
+            content: None,
+            raw_input: None,
+            raw_output: None,
+            locations: None,
+            meta: None,
+            images: None,
+        },
+    )
+    .await;
+
+    let snapshot = state.read().await.to_snapshot();
+    let mut projection = Projection::default();
+    projection.replace_from_snapshot(&snapshot, 50);
+
+    assert_eq!(projection.live_assistant, "hello");
+    assert_eq!(projection.tools.len(), 1);
+    assert_eq!(projection.tools[0].name, "Read file");
+    assert_eq!(projection.tools[0].status, "in_progress");
+    assert_eq!(projection.event_seq, 3);
+    assert_eq!(projection.assistant_generation, 1);
+    assert_eq!(projection.transcript_generation, 1);
+}
+
+#[tokio::test]
+async fn snapshot_hard_error_sets_terminal_marker_after_dropped_event() {
+    let state = state("snapshot-error");
+    emit(
+        &state,
+        AcpEvent::UserMessage {
+            message_id: "failed-message".to_string(),
+            blocks: Vec::new(),
+        },
+    )
+    .await;
+    emit(
+        &state,
+        AcpEvent::ContentDelta {
+            text: "partial".to_string(),
+            parent_tool_use_id: None,
+        },
+    )
+    .await;
+    emit(
+        &state,
+        AcpEvent::Error {
+            message: "hard failure".to_string(),
+            agent_type: "codex".to_string(),
+            code: Some("turn_failed".to_string()),
+            terminal: false,
+        },
+    )
+    .await;
+    let snapshot = state.read().await.to_snapshot();
+    let mut projection = Projection::default();
+
+    projection.replace_from_snapshot(&snapshot, 55);
+
+    assert_eq!(projection.error_strip, "hard failure");
+    assert_eq!(projection.t_end_ns, 55);
+    assert!(!projection.stream_active);
+}
+
+#[tokio::test]
+async fn snapshot_new_user_message_resets_prior_turn_markers() {
+    let state = state("snapshot-turn");
+    emit(
+        &state,
+        AcpEvent::UserMessage {
+            message_id: "old-message".to_string(),
+            blocks: Vec::new(),
+        },
+    )
+    .await;
+    emit(
+        &state,
+        AcpEvent::ContentDelta {
+            text: "old answer".to_string(),
+            parent_tool_use_id: None,
+        },
+    )
+    .await;
+    let mut projection = Projection::default();
+    projection.replace_from_snapshot(&state.read().await.to_snapshot(), 10);
+    projection.t_end_ns = 15;
+
+    emit(
+        &state,
+        AcpEvent::UserMessage {
+            message_id: "new-message".to_string(),
+            blocks: Vec::new(),
+        },
+    )
+    .await;
+    projection.replace_from_snapshot(&state.read().await.to_snapshot(), 20);
+
+    assert!(projection.live_assistant.is_empty());
+    assert_eq!(projection.t_first_token_ns, 0);
+    assert_eq!(projection.t_end_ns, 0);
+}
+
+#[tokio::test]
+async fn sequence_gap_replaces_projection_from_authoritative_snapshot() {
+    let state = state("recovery");
+    let backend: Arc<dyn LiveBackend> = Arc::new(StateBackend {
+        state: Arc::clone(&state),
+        declines: Arc::new(AtomicUsize::new(0)),
+    });
+    let projector = LiveProjector::new(backend, SharedModel::new());
+    let mut attachment = projector.attach("recovery", 0).await.unwrap();
+
+    let (stream, second) = {
+        let mut guard = state.write().await;
+        let _ = guard.apply_event(&AcpEvent::ContentDelta {
+            text: "fi".to_string(),
+            parent_tool_use_id: None,
+        });
+        guard.event_seq = 1;
+        let payload = AcpEvent::ContentDelta {
+            text: "nal".to_string(),
+            parent_tool_use_id: None,
+        };
+        let _ = guard.apply_event(&payload);
+        guard.event_seq = 2;
+        let envelope = Arc::new(EventEnvelope {
+            seq: 2,
+            connection_id: "recovery".to_string(),
+            payload,
+        });
+        (guard.event_stream(), envelope)
+    };
+    stream.send(second);
+
+    assert_eq!(
+        attachment.receive_next().await.unwrap(),
+        ReceiveOutcome::Recovered
+    );
+    assert_eq!(attachment.snapshot().event_seq, 2);
+    assert_eq!(attachment.snapshot().live_assistant, "final");
+    assert!(!attachment.snapshot().needs_resync);
+}
+
+#[tokio::test]
+async fn control_overflow_resyncs_and_declines_snapshot_permission_once() {
+    let state = state("overflow");
+    let declines = Arc::new(AtomicUsize::new(0));
+    let backend: Arc<dyn LiveBackend> = Arc::new(StateBackend {
+        state: Arc::clone(&state),
+        declines: Arc::clone(&declines),
+    });
+    let projector = LiveProjector::with_control_capacity(backend, SharedModel::new(), 1);
+    let mut attachment = projector.attach("overflow", 0).await.unwrap();
+
+    emit(
+        &state,
+        AcpEvent::ContentDelta {
+            text: "partial".to_string(),
+            parent_tool_use_id: None,
+        },
+    )
+    .await;
+    for _ in 0..1_000 {
+        if attachment.queued_control_events() == 1 {
+            break;
+        }
+        tokio::task::yield_now().await;
+    }
+    assert_eq!(attachment.queued_control_events(), 1);
+
+    emit(
+        &state,
+        AcpEvent::PermissionRequest {
+            request_id: "overflow-permission".to_string(),
+            tool_call: serde_json::json!({}),
+            options: vec![PermissionOptionInfo {
+                option_id: "deny".to_string(),
+                name: "Deny".to_string(),
+                kind: "reject_once".to_string(),
+            }],
+        },
+    )
+    .await;
+    for _ in 0..1_000 {
+        if attachment.recovery_pending() {
+            break;
+        }
+        tokio::task::yield_now().await;
+    }
+    assert!(attachment.recovery_pending());
+
+    assert_eq!(
+        attachment.receive_next().await.unwrap(),
+        ReceiveOutcome::Recovered
+    );
+    assert_eq!(attachment.snapshot().event_seq, 2);
+    assert_eq!(declines.load(Ordering::SeqCst), 1);
+
+    emit(
+        &state,
+        AcpEvent::TurnComplete {
+            session_id: "session".to_string(),
+            stop_reason: "end_turn".to_string(),
+            agent_type: "codex".to_string(),
+            mark_awaiting_reply: true,
+            termination_source: None,
+            provider_turn_id: None,
+        },
+    )
+    .await;
+    assert_eq!(
+        attachment.receive_next().await.unwrap(),
+        ReceiveOutcome::Applied
+    );
+    assert!(attachment.snapshot().t_end_ns > 0);
+    assert!(!attachment.snapshot().stream_active);
+
+    attachment.resync().await.unwrap();
+    let authoritative = state.read().await.to_snapshot();
+    let mut expected = Projection::default();
+    expected.replace_from_snapshot(&authoritative, 1);
+    assert_eq!(attachment.snapshot().event_seq, expected.event_seq);
+    assert_eq!(
+        attachment.snapshot().live_assistant,
+        expected.live_assistant
+    );
+    assert_eq!(attachment.snapshot().tools, expected.tools);
+    assert_eq!(attachment.snapshot().stream_active, expected.stream_active);
+    assert_eq!(declines.load(Ordering::SeqCst), 1);
+}
+
+#[tokio::test]
+async fn broadcast_lag_recovers_without_blocking_the_producer() {
+    let state = state("lag");
+    let backend: Arc<dyn LiveBackend> = Arc::new(StateBackend {
+        state: Arc::clone(&state),
+        declines: Arc::new(AtomicUsize::new(0)),
+    });
+    let projector = LiveProjector::with_control_capacity(backend, SharedModel::new(), 5_000);
+    let mut attachment = projector.attach("lag", 0).await.unwrap();
+
+    {
+        let mut guard = state.write().await;
+        let stream = guard.event_stream();
+        for seq in 1..=4_097 {
+            let payload = AcpEvent::ContentDelta {
+                text: if seq == 4_097 {
+                    "final".to_string()
+                } else {
+                    String::new()
+                },
+                parent_tool_use_id: None,
+            };
+            let _ = guard.apply_event(&payload);
+            guard.event_seq = seq;
+            stream.send(Arc::new(EventEnvelope {
+                seq,
+                connection_id: "lag".to_string(),
+                payload,
+            }));
+        }
+    }
+
+    assert_eq!(
+        attachment.receive_next().await.unwrap(),
+        ReceiveOutcome::Recovered
+    );
+    assert_eq!(attachment.snapshot().event_seq, 4_097);
+    assert_eq!(attachment.snapshot().live_assistant, "final");
+    assert!(!attachment.snapshot().needs_resync);
+}

