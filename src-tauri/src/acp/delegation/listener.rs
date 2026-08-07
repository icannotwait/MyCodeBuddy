//! Main-process side of the `codeg-mcp` round-trip: accept UDS / named-pipe
//! connections from companion processes, validate the per-launch token,
//! resolve the parent's current conversation, and hand off to the broker.
//!
//! The listener is intentionally tiny — most of the work (depth checking,
//! spawn lifecycle, timeout, cancellation) happens inside
//! [`DelegationBroker`]. The listener is the boundary between the wire and
//! the broker, plus the place where the per-launch token policy is enforced.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use sea_orm::EntityTrait;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(any(test, feature = "test-utils"))]
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::acp::delegation::broker::{
    DelegationBroker, StatusWait, StatusWaitPreflight, StatusWaitPreflightKind,
};
use crate::acp::delegation::continuation::coordinator::{
    ContinuationError, DelegationContinuationCoordinator, JoinArmOutcome, JoinArmRequest,
};
use crate::acp::delegation::continuation::{
    foreground_mcp_release_fence, ForegroundMcpReleaseOwner,
};
use crate::acp::delegation::lease::CompanionLeaseRegistry;
use crate::acp::delegation::metrics::CompletionRestartOutcome;
use crate::acp::delegation::recovery_policy::{
    decide_delegation_recovery, RecoveryAction, RecoveryRailSnapshot, RequestedRecoveryOperation,
};
use crate::acp::delegation::run_store::{
    recovery_action_payload, recovery_source_from_continue_eligibility,
};
use crate::acp::delegation::transport::{
    read_frame, write_frame, BrokerAskRequest, BrokerCancelRequest, BrokerCancelTaskRequest,
    BrokerCommitFeedbackRequest, BrokerCompleteWorkRequest, BrokerFeedbackRequest,
    BrokerGetWorkflowStateRequest, BrokerMessage, BrokerParentDecisionRequest,
    BrokerPublishWorkflowRequest, BrokerRecoverWorkflowRequest, BrokerRecoveryAuthorizationRequest,
    BrokerReplyDelegationRequest, BrokerRequest, BrokerResponse,
    BrokerRestartLegacyWorkflowRequest, BrokerSessionRequest, BrokerSettleWorkflowRequest,
    BrokerStatusRequest, CancelDelegationReason, CompanionReadyAck, CompanionRole,
};
use crate::acp::delegation::types::{
    correlation_error_message, validate_correlation_id, CorrelationEntryPoint,
    CorrelationFailureKind, DelegationReplyResult, DelegationRequest, DelegationReturnWhen,
    DelegationStatusBatch, DelegationTaskReport, DelegationWakeReason, ParentDecisionResult,
    TaskStatus,
};
use crate::acp::delegation::workflow::{
    accept_complete_work_txn, decide_workflow_recovery, get_workflow_state_core,
    guard_current_final_delivery_core, guard_task_final_delivery_core,
    publish_workflow_manifest_with_selection_core, recover_workflow_core,
    restart_legacy_workflow_if_enforced, select_completion_protocol, settle_workflow_gate_core,
    settle_workflow_gate_v2_core, CompletionProtocolRolloutConfig, FinalDeliveryGuardResult,
    ManifestDocument, PlanReviewError, PublishWorkflowRequest, RecoverWorkflowRequest,
    SettleWorkflowRequest, SettleWorkflowV2Request, WorkflowError, WorkflowRecoveryDisposition,
    WorkflowStoreError,
};
use crate::acp::feedback::{PendingFeedback, SessionFeedbackAccess};
use crate::acp::question::{
    QuestionOption, QuestionOutcome, QuestionSpec, RecoveryQuestionPresentation,
    SessionQuestionAccess,
};
use crate::acp::recovery_authorization::{
    derive_recovery_action_metadata, DelegationAuthorizationIdentity, PreparedAuthorization,
    RecoveryAllowedAction, RecoveryAuthorizationError, RecoveryAuthorizationResult,
    RecoveryAuthorizationService, RecoveryChallenge, RecoverySubjectKind, RECOVERY_APPROVE_LABEL,
    RECOVERY_DECLINE_LABEL,
};
use crate::acp::session_info::{SessionInfo, SessionInfoAccess};
use crate::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;
use crate::db::entities::{delegation_workflow, recovery_authorization};
use crate::models::AgentType;
use crate::web::event_bridge::EventEmitter;
use serde_json::Value;

/// Hard ceiling on a *positive* `get_delegation_status` long-poll, so a single
/// MCP tool call can't block the companion's round-trip unbounded. The child
/// keeps running past this; the LLM simply re-issues the wait. An explicit
/// `wait_ms = 0` opts out of the ceiling and blocks until the task is terminal.
const STATUS_WAIT_MAX_MS: u64 = 60_000;

/// Parent session context for full [`WaitStamp`] registration on Join waits.
#[derive(Debug, Clone)]
pub struct ParentWaitContext {
    pub conversation_id: i32,
    pub connection_incarnation: String,
    pub turn_generation: u64,
    /// ACP tool_call_id of the parked wait tool when known.
    pub parent_tool_use_id: Option<String>,
}

/// Pluggable "what conversation is this parent currently in?" lookup. The
/// production impl wraps `ConnectionManager.get_state`; tests use an
/// in-memory map.
///
/// Kept as a trait so the listener can be unit-tested without spinning up a
/// real `ConnectionManager` or RwLock<SessionState>.
#[async_trait]
pub trait ParentSessionLookup: Send + Sync {
    async fn current_conversation_id(&self, parent_connection_id: &str) -> Option<i32>;

    /// Rich identity for full WaitStamp registration (incarnation+turn+parent).
    /// Default falls back to conversation id only (test stubs).
    async fn parent_wait_context(&self, parent_connection_id: &str) -> Option<ParentWaitContext> {
        let conversation_id = self.current_conversation_id(parent_connection_id).await?;
        Some(ParentWaitContext {
            conversation_id,
            connection_incarnation: String::new(),
            turn_generation: 0,
            parent_tool_use_id: None,
        })
    }

    /// Bind the parent foreground tool lease to
    /// [`CancellationCapability::DelegationWait`] for the exact wait stamp.
    /// Default is a no-op for test stubs (reports missing tool id).
    async fn bind_delegation_wait(
        &self,
        _parent_connection_id: &str,
        expected: &crate::acp::tool_watchdog::WaitStamp,
    ) -> crate::acp::tool_watchdog::BindDelegationWaitResult {
        use crate::acp::tool_watchdog::BindDelegationWaitResult;
        match expected
            .parent_tool_use_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => BindDelegationWaitResult::WaitToolIdMissing,
            Some(_) => BindDelegationWaitResult::WaitToolLeaseMismatch,
        }
    }
}

/// Per-launch token entry. Bound at MCP injection time and revoked on parent
/// connection teardown.
#[derive(Debug, Clone)]
pub struct TokenEntry {
    pub parent_connection_id: String,
    pub working_dir: PathBuf,
    /// Whether this launch advertised `coordination_v1` (Join semantics).
    pub coordination_v1: bool,
    /// Whether this immutable launch opted into durable Join continuation.
    pub delegation_continuation_v1: bool,
    /// Immutable companion role for this launch.
    pub role: CompanionRole,
    /// Whether this launch advertised `workflow_v2` (Root-only mutation tools).
    pub workflow_v2: bool,
    /// Child-only completion protocol capability for this immutable launch.
    pub completion_v2: bool,
    /// Durable task identity stamped by workflow admission, never by the model.
    pub bound_task_id: Option<String>,
}

impl TokenEntry {
    /// Legacy entry without Join capability (tests / pre-coordination launches).
    pub fn legacy(parent_connection_id: &str, working_dir: PathBuf) -> Self {
        Self {
            parent_connection_id: parent_connection_id.to_string(),
            working_dir,
            coordination_v1: false,
            delegation_continuation_v1: false,
            role: CompanionRole::Root,
            workflow_v2: false,
            completion_v2: false,
            bound_task_id: None,
        }
    }
}

#[derive(Default)]
pub struct TokenRegistry {
    inner: RwLock<HashMap<String, TokenEntry>>,
    continuation_coordinator: OnceLock<Weak<DelegationContinuationCoordinator>>,
}

impl TokenRegistry {
    pub fn with_continuation_coordinator(
        coordinator: Arc<DelegationContinuationCoordinator>,
    ) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            continuation_coordinator: OnceLock::from(Arc::downgrade(&coordinator)),
        }
    }

    fn continuation_coordinator(&self) -> Option<Arc<DelegationContinuationCoordinator>> {
        self.continuation_coordinator.get()?.upgrade()
    }

    pub async fn register(&self, token: String, entry: TokenEntry) {
        self.inner.write().await.insert(token, entry);
    }

    pub async fn revoke(&self, token: &str) {
        self.inner.write().await.remove(token);
    }

    pub async fn lookup(&self, token: &str) -> Option<TokenEntry> {
        self.inner.read().await.get(token).cloned()
    }

    /// Drop every token whose `parent_connection_id` matches. Used on parent
    /// connection teardown so a leaked token can't be reused. Returns the
    /// revoked token strings so callers can also revoke ready leases.
    pub async fn revoke_by_parent(&self, parent_connection_id: &str) -> Vec<String> {
        let mut map = self.inner.write().await;
        let mut revoked = Vec::new();
        map.retain(|token, entry| {
            if entry.parent_connection_id == parent_connection_id {
                revoked.push(token.clone());
                false
            } else {
                true
            }
        });
        revoked
    }
}

enum ArmStatus {
    Immediate(DelegationStatusBatch),
    Suspended,
}

enum StatusReleaseDecision {
    WaitCancelled(crate::acp::tool_watchdog::CancelCause),
    Suspended,
}

#[cfg(any(test, feature = "test-utils"))]
#[allow(dead_code, reason = "constructed only by listener test builds")]
pub(crate) struct StatusReleaseDecisionGateHandle {
    pub before_select_entered: oneshot::Receiver<()>,
    pub arm_suspended_ready: oneshot::Receiver<()>,
    pub allow_select: oneshot::Sender<()>,
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Default)]
struct StatusReleaseDecisionGate {
    before_select: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
    arm_ready: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    allow_select: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

#[cfg(any(test, feature = "test-utils"))]
impl StatusReleaseDecisionGate {
    #[allow(dead_code, reason = "called only by listener test builds")]
    async fn install(&self) -> StatusReleaseDecisionGateHandle {
        let (before_tx, before_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (allow_tx, allow_rx) = oneshot::channel();
        *self.before_select.lock().await = Some(before_tx);
        *self
            .arm_ready
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(ready_tx);
        *self.allow_select.lock().await = Some(allow_rx);
        StatusReleaseDecisionGateHandle {
            before_select_entered: before_rx,
            arm_suspended_ready: ready_rx,
            allow_select: allow_tx,
        }
    }

    async fn before_select(&self) {
        if let Some(entered) = self.before_select.lock().await.take() {
            let _ = entered.send(());
        }
        if let Some(allow) = self.allow_select.lock().await.take() {
            let _ = allow.await;
        }
    }

    fn arm_suspended_ready(&self) {
        if let Some(ready) = self
            .arm_ready
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = ready.send(());
        }
    }
}

#[derive(Serialize)]
struct StatusErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct StatusErrorEnvelope {
    error: StatusErrorBody,
}

impl StatusErrorEnvelope {
    fn continuation_arm_failed() -> Self {
        Self {
            error: StatusErrorBody {
                code: "continuation_arm_failed",
                message: "Delegation continuation could not be armed",
            },
        }
    }
}

struct ProcessedStatus {
    batch: DelegationStatusBatch,
    release_owner: Option<ForegroundMcpReleaseOwner>,
}

impl ProcessedStatus {
    fn plain(batch: DelegationStatusBatch) -> Self {
        Self {
            batch,
            release_owner: None,
        }
    }
}

struct CancelWaiterOnDrop(CancellationToken);

impl Drop for CancelWaiterOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub struct DelegationListener {
    pub broker: Arc<DelegationBroker>,
    pub tokens: Arc<TokenRegistry>,
    pub leases: Arc<CompanionLeaseRegistry>,
    pub parent_lookup: Arc<dyn ParentSessionLookup>,
    /// Pulls pending live-feedback notes for the `check_user_feedback` tool.
    /// Shares the same `tokens` registry and parent-connection scoping as the
    /// delegation arms — one companion, one socket, two features.
    pub feedback: Arc<dyn SessionFeedbackAccess>,
    /// Registers / cancels the blocking `ask_user_question` tool's pending
    /// questions. Same `tokens` registry and parent-connection scoping.
    pub questions: Arc<dyn SessionQuestionAccess>,
    /// Resolves a referenced session for the `get_session_info` tool. Unlike the
    /// other arms this is NOT parent-scoped — it looks any non-deleted session up
    /// by its codeg conversation id (still token-gated against an invalid caller).
    pub session_info: Arc<dyn SessionInfoAccess>,
    /// Process-local reliability metrics (wait peer-close, cancel classes).
    pub metrics: Arc<crate::acp::delegation::metrics::DelegationMetrics>,
    /// Host-only request-scoped wait cancel registry (never cancels children).
    pub wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
    /// Shared `EventEmitter` for workflow graph live events (publish/settle).
    pub workflow_emitter: EventEmitter,
    pub completion_protocol_rollout: Arc<CompletionProtocolRolloutConfig>,
    recovery_authorizations: Option<Arc<RecoveryAuthorizationService>>,
    #[cfg(any(test, feature = "test-utils"))]
    status_release_decision_gate: Arc<StatusReleaseDecisionGate>,
}

impl DelegationListener {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        leases: Arc<CompanionLeaseRegistry>,
        parent_lookup: Arc<dyn ParentSessionLookup>,
        feedback: Arc<dyn SessionFeedbackAccess>,
        questions: Arc<dyn SessionQuestionAccess>,
        session_info: Arc<dyn SessionInfoAccess>,
    ) -> Arc<Self> {
        Self::new_with_wait_cancel(
            broker,
            tokens,
            leases,
            parent_lookup,
            feedback,
            questions,
            session_info,
            crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_wait_cancel(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        leases: Arc<CompanionLeaseRegistry>,
        parent_lookup: Arc<dyn ParentSessionLookup>,
        feedback: Arc<dyn SessionFeedbackAccess>,
        questions: Arc<dyn SessionQuestionAccess>,
        session_info: Arc<dyn SessionInfoAccess>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
    ) -> Arc<Self> {
        Self::new_with_workflow_emitter(
            broker,
            tokens,
            leases,
            parent_lookup,
            feedback,
            questions,
            session_info,
            wait_cancel,
            EventEmitter::Noop,
        )
    }

    /// Production constructor with a live [`EventEmitter`] for workflow graph
    /// events. Tests may keep [`Self::new_with_wait_cancel`] (Noop emitter).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_workflow_emitter(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        leases: Arc<CompanionLeaseRegistry>,
        parent_lookup: Arc<dyn ParentSessionLookup>,
        feedback: Arc<dyn SessionFeedbackAccess>,
        questions: Arc<dyn SessionQuestionAccess>,
        session_info: Arc<dyn SessionInfoAccess>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
        workflow_emitter: EventEmitter,
    ) -> Arc<Self> {
        Self::new_with_workflow_runtime(
            broker,
            tokens,
            leases,
            parent_lookup,
            feedback,
            questions,
            session_info,
            wait_cancel,
            workflow_emitter,
            Arc::new(CompletionProtocolRolloutConfig::default()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_workflow_runtime(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        leases: Arc<CompanionLeaseRegistry>,
        parent_lookup: Arc<dyn ParentSessionLookup>,
        feedback: Arc<dyn SessionFeedbackAccess>,
        questions: Arc<dyn SessionQuestionAccess>,
        session_info: Arc<dyn SessionInfoAccess>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
        workflow_emitter: EventEmitter,
        completion_protocol_rollout: Arc<CompletionProtocolRolloutConfig>,
    ) -> Arc<Self> {
        let metrics = broker.metrics();
        let recovery_authorizations = broker
            .run_store()
            .map(|runs| Arc::new(RecoveryAuthorizationService::new(runs.db().conn.clone())));
        Arc::new(Self {
            broker,
            tokens,
            leases,
            parent_lookup,
            feedback,
            questions,
            session_info,
            wait_cancel,
            metrics,
            workflow_emitter,
            completion_protocol_rollout,
            recovery_authorizations,
            #[cfg(any(test, feature = "test-utils"))]
            status_release_decision_gate: Arc::new(StatusReleaseDecisionGate::default()),
        })
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[allow(dead_code, reason = "called only by listener test builds")]
    pub(crate) async fn install_status_release_decision_gate(
        &self,
    ) -> StatusReleaseDecisionGateHandle {
        self.status_release_decision_gate.install().await
    }

    /// Run the accept loop until the socket is unbound. Errors on accept are
    /// logged and the loop continues — a single bad connection can't bring
    /// down the listener.
    #[cfg(unix)]
    pub async fn run(self: Arc<Self>, socket_path: PathBuf) -> std::io::Result<()> {
        let _ = tokio::fs::remove_file(&socket_path).await;
        if let Some(parent) = socket_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        tracing::info!("[delegation] listening on UDS {}", socket_path.display());
        loop {
            match listener.accept().await {
                Ok((mut conn, _)) => {
                    let me = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = me.serve_one(&mut conn).await {
                            tracing::error!("[delegation] connection failed: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("[delegation] accept failed: {e}");
                    // Brief backoff so a persistent accept error doesn't pin a core.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Windows variant: bind a named pipe and follow Tokio's recommended
    /// accept pattern — wait for a connect, immediately create the *next*
    /// server instance, then hand the connected instance off to a worker.
    /// This keeps a pipe instance available at all times, so clients calling
    /// `ClientOptions::open()` between connections don't see `NotFound`.
    #[cfg(windows)]
    pub async fn run(self: Arc<Self>, socket_path: PathBuf) -> std::io::Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let path_str = socket_path.to_string_lossy().to_string();
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&path_str)?;
        tracing::info!("[delegation] listening on named pipe {path_str}");
        loop {
            if let Err(e) = server.connect().await {
                tracing::error!("[delegation] connect failed: {e}");
                // Re-create the instance so the next iteration has a fresh
                // listener; a failed connect leaves the current one unusable.
                server = ServerOptions::new().create(&path_str)?;
                continue;
            }
            let connected = server;
            // Re-bind BEFORE serving the current client, so a client that
            // opens during this turn finds a server instance to connect to.
            server = ServerOptions::new().create(&path_str)?;
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let mut conn = connected;
                if let Err(e) = me.serve_one(&mut conn).await {
                    tracing::error!("[delegation] connection failed: {e}");
                }
            });
        }
    }

    /// Stream-generic per-connection handler. Exposed so unit tests can drive
    /// it over `tokio::io::duplex` instead of a real socket.
    pub async fn serve_one<C>(&self, conn: &mut C) -> std::io::Result<()>
    where
        C: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        let msg: BrokerMessage = read_frame(conn).await?;
        // Ready lease is a long-lived hold: authenticate → mark ready → ack →
        // select peer-EOF vs revoke, then mark closed exactly once.
        if let BrokerMessage::Ready(req) = msg {
            return self.serve_ready_lease(conn, req.token).await;
        }
        let mut foreground_release_owner = None;
        let resp = match msg {
            BrokerMessage::Ready(_) => unreachable!("handled above"),
            BrokerMessage::Call(req) => self.report_response(self.process(req).await).await?,
            BrokerMessage::Status(req) => {
                // A status long-poll — especially `wait_ms = 0` (block until
                // terminal) — can park for the whole lifetime of the child.
                // Race it against peer-close on this one-shot connection so a
                // companion that cancels and drops the request socket doesn't
                // leave this task parked until the task happens to finish. A
                // status query has no side effects (unlike a delegation), so
                // abandoning the wait is safe and there's nothing to cancel
                // broker-side. The companion never writes a second frame on
                // this socket, so the probe read only resolves on EOF/error.
                use crate::acp::delegation::metrics::{
                    DelegationAuditRecord, WaitModeLabel, WaitReturnReason,
                };
                let wait_mode = match req.wait_ms {
                    None => WaitModeLabel::Snapshot,
                    Some(0) => WaitModeLabel::Terminal,
                    Some(_) => WaitModeLabel::Supervised,
                };
                let requested_wait_ms = req.wait_ms.map(|ms| ms.min(STATUS_WAIT_MAX_MS));
                let wait_started = std::time::Instant::now();
                let status_fut = self.process_status(req);
                tokio::pin!(status_fut);
                let mut probe = [0u8; 1];
                let reports = tokio::select! {
                    biased;
                    reports = &mut status_fut => reports,
                    _ = conn.read(&mut probe) => {
                        // Peer closed before broker returned: record once,
                        // no task mutation, abandon wait.
                        let wall = wait_started.elapsed();
                        self.metrics.record_wait(
                            wait_mode,
                            wall,
                            WaitReturnReason::PeerClosed,
                        );
                        DelegationAuditRecord::wait(
                            wait_mode,
                            requested_wait_ms,
                            wall,
                            WaitReturnReason::PeerClosed,
                        )
                        .emit_wait();
                        return Ok(());
                    },
                };
                match reports {
                    Ok(processed) => {
                        let response = self.status_response(processed.batch).await?;
                        foreground_release_owner = processed.release_owner;
                        response
                    }
                    Err(_) => value_response(&StatusErrorEnvelope::continuation_arm_failed())?,
                }
            }
            BrokerMessage::CancelTask(req) => {
                self.report_response(self.process_cancel_task(req).await)
                    .await?
            }
            BrokerMessage::Feedback(req) => {
                // at-least-once delivery: READ pending notes (no mutation),
                // WRITE the response, and COMMIT them delivered ONLY on a
                // successful write. A dropped/failed write skips the commit, so
                // the notes stay pending for the agent's next check.
                match self.feedback_target(&req).await {
                    None => {
                        // Invalid token: return an empty envelope (no leak of
                        // whether any feedback exists), nothing to commit.
                        write_frame(conn, &feedback_response(&[])?).await?;
                    }
                    Some(parent_conn_id) => {
                        let pending = self.feedback.read_pending_feedback(&parent_conn_id).await;
                        // Read-only: the response carries the note ids
                        // (`_commit_ids`); delivery is committed LATER, by the
                        // companion's `CommitFeedback` once it actually returns
                        // the result to the agent. So a cancel that suppresses
                        // the agent-facing response leaves the notes pending.
                        write_frame(conn, &feedback_response(&pending)?).await?;
                    }
                }
                return Ok(());
            }
            BrokerMessage::CommitFeedback(req) => {
                self.process_commit_feedback(req).await;
                // Empty ack so the companion can confirm the listener saw it.
                BrokerResponse {
                    outcome: Value::Null,
                }
            }
            BrokerMessage::Ask(req) => {
                // Register the question (broadcasting the card) and park until
                // the user answers — racing peer-close exactly like `Status`.
                // The companion holds this connection open for the whole wait
                // and never writes a second frame, so the probe read only
                // resolves on EOF/error; a canceled tool call drops the
                // companion's future, closing this socket, which we observe and
                // tear the pending question down. An invalid token, a gone
                // connection, or a connection that already has a pending ask
                // (one-at-a-time) yields a `declined` outcome (the LLM proceeds
                // with its own judgment) rather than hanging.
                let Some(parent_conn_id) = self.ask_target(&req).await else {
                    write_frame(conn, &ask_declined_response()?).await?;
                    return Ok(());
                };
                let Some(reg) = self
                    .questions
                    .register_question(&parent_conn_id, req.questions)
                    .await
                else {
                    write_frame(conn, &ask_declined_response()?).await?;
                    return Ok(());
                };
                let question_id = reg.question_id;
                let mut answer_rx = reg.answer_rx;
                // Close the teardown race: `ask_target` validated the token, but the
                // parent connection may have been revoked + swept
                // (`cancel_questions_by_parent`) in the window before the insert
                // above — the sweep would have missed this just-registered entry,
                // leaving it parked until peer-close. The token is revoked before
                // the sweep, so a re-check that now finds it gone means teardown is
                // underway: cancel immediately so the ask can't linger.
                if self.tokens.lookup(&req.token).await.is_none() {
                    self.questions
                        .cancel_question(&parent_conn_id, &question_id)
                        .await;
                    write_frame(conn, &ask_declined_response()?).await?;
                    return Ok(());
                }
                let mut probe = [0u8; 1];
                let outcome = tokio::select! {
                    biased;
                    ans = &mut answer_rx => ans.ok(),
                    _ = conn.read(&mut probe) => {
                        self.questions
                            .cancel_question(&parent_conn_id, &question_id)
                            .await;
                        return Ok(());
                    }
                };
                let resp = match outcome {
                    Some(o) => ask_response(&o)?,
                    // Sender dropped without sending (connection teardown drain):
                    // surface a declined outcome so the tool returns cleanly.
                    None => ask_declined_response()?,
                };
                write_frame(conn, &resp).await?;
                return Ok(());
            }
            BrokerMessage::SessionInfo(req) => {
                // Read-only resolution (DB + a bounded transcript parse). No
                // peer-close race needed: unlike Status/Ask this never blocks on
                // a long-poll or a human — the bounded parse always completes —
                // and there is nothing to tear down on cancel.
                session_response(self.process_session_info(req).await)?
            }
            BrokerMessage::ParentDecision(req) => {
                // Blocking parent decision: race Broker wait against peer-close
                // on this one-shot socket. Peer close drops ONLY this waiter —
                // the durable attention row stays open for replay; no task or
                // attention mutation on abandon.
                let decision_fut = self.process_parent_decision(req);
                tokio::pin!(decision_fut);
                let mut probe = [0_u8; 1];
                let outcome = tokio::select! {
                    biased;
                    outcome = &mut decision_fut => outcome,
                    _ = conn.read(&mut probe) => return Ok(()),
                };
                write_frame(conn, &value_response(&outcome)?).await?;
                return Ok(());
            }
            BrokerMessage::ReplyDelegation(req) => {
                // Immediate: serialize through the normal final write_frame path.
                value_response(&self.process_reply_delegation(req).await)?
            }
            BrokerMessage::PublishWorkflow(req) => {
                value_response(&self.process_publish_workflow(req).await)?
            }
            BrokerMessage::SettleWorkflow(req) => {
                value_response(&self.process_settle_workflow(req).await)?
            }
            BrokerMessage::CompleteWork(req) => {
                value_response(&self.process_complete_work(req).await)?
            }
            BrokerMessage::GetWorkflowState(req) => {
                value_response(&self.process_get_workflow_state(req).await)?
            }
            BrokerMessage::RestartLegacyWorkflow(req) => {
                value_response(&self.process_restart_legacy_workflow(req).await)?
            }
            BrokerMessage::RequestRecoveryAuthorization(req) => {
                let cancelled = CancellationToken::new();
                let request_fut = self.process_recovery_authorization(req, cancelled.clone());
                tokio::pin!(request_fut);
                let mut probe = [0_u8; 1];
                let outcome = tokio::select! {
                    biased;
                    outcome = &mut request_fut => outcome,
                    _ = conn.read(&mut probe) => {
                        cancelled.cancel();
                        let _ = request_fut.await;
                        return Ok(());
                    }
                };
                value_response(&outcome)?
            }
            BrokerMessage::RecoverWorkflow(req) => {
                value_response(&self.process_recover_workflow(req).await)?
            }
            BrokerMessage::Cancel(cancel) => {
                self.process_cancel(cancel).await;
                // Empty ack — the companion only uses this to detect the
                // listener has at least seen the cancel before dropping.
                BrokerResponse {
                    outcome: Value::Null,
                }
            }
        };
        write_frame(conn, &resp).await?;
        if let Some(owner) = foreground_release_owner.take() {
            owner.frame_flushed();
        }
        Ok(())
    }

    /// Authenticated two-frame ready lease.
    ///
    /// Order: validate/reserve token → write `{"ready":true}` ack → publish
    /// host readiness via [`CompanionLeaseRegistry::mark_ready`]. Readiness is
    /// never published until the ack write succeeds, so a dead companion cannot
    /// make the host `wait_ready` / Connected / `RouteBootstrapOutcome::Ready`.
    ///
    /// On ack write failure the lease is revoked so the waiter fails closed
    /// immediately (not merely availability=false after a false ready). If
    /// revoke races between ack and mark_ready, mark_ready fails and the hold
    /// is not entered.
    async fn serve_ready_lease<C>(&self, conn: &mut C, token: String) -> std::io::Result<()>
    where
        C: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        // 1) Authenticate first — never publish ready for an unknown/revoked token.
        if self.tokens.lookup(&token).await.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "invalid ready-lease token",
            ));
        }
        // 2) Reserve: host must have registered the lease before companion Ready.
        let mut availability = self
            .leases
            .subscribe_availability(&token)
            .await
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "ready lease not registered")
            })?;

        // 3) Write ack only after authentication/reserve. Host readiness is
        //    published only after this write succeeds.
        if let Err(e) = write_frame(conn, &CompanionReadyAck { ready: true }).await {
            // Fail closed: never mark_ready; drop the lease so wait_ready sees
            // Closed immediately (ready_tx dropped) and cannot return Ready.
            self.leases.revoke(&token).await;
            return Err(e);
        }

        // 4) Publish host ready only after durable ack. Revoke race → fail closed.
        //
        // `AlreadyReady` is a successful secondary attach (e.g. CLI exec turns
        // re-spawn the same codeg-mcp after a session-open prewarm already holds
        // the exclusive ready lease). Ack was already written; do **not**
        // mark_closed — that would tear down the primary holder's availability.
        // Secondary instances return immediately without exclusive hold so they
        // can serve MCP stdio tools for the agent turn.
        match self.leases.mark_ready(&token).await {
            Ok(()) => {}
            Err(crate::acp::delegation::lease::ReadyLeaseError::AlreadyReady) => {
                return Ok(());
            }
            Err(e) => {
                self.leases.mark_closed(&token).await;
                return Err(std::io::Error::other(format!("mark_ready: {e}")));
            }
        }

        // Hold open: peer EOF or external revoke (availability → false).
        let mut probe = [0u8; 1];
        tokio::select! {
            biased;
            _ = conn.read(&mut probe) => {}
            _ = async {
                loop {
                    if !*availability.borrow() {
                        break;
                    }
                    if availability.changed().await.is_err() {
                        break;
                    }
                }
            } => {}
        }
        self.leases.mark_closed(&token).await;
        Ok(())
    }

    /// Validate the token, resolve the caller's parent connection/conversation,
    /// and query the status of every requested task id. Legacy requests (no
    /// `return_when`) keep snapshot / supervised / any-terminal waits. Join
    /// requests require `return_when=all_terminal_or_attention` with explicit
    /// `wait_ms=0` and a token that advertised `coordination_v1`. Invalid-token
    /// or capability-denied Join still returns Join-shaped additive fields
    /// without revealing ownership and without parking.
    async fn process_status(
        &self,
        req: BrokerStatusRequest,
    ) -> Result<ProcessedStatus, ContinuationError> {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            let unknown_reports: Vec<_> =
                req.task_ids.iter().map(|id| unknown_report(id)).collect();
            return Ok(ProcessedStatus::plain(match req.return_when {
                None => DelegationStatusBatch::legacy(unknown_reports),
                Some(_) => DelegationStatusBatch::joined(
                    unknown_reports,
                    DelegationWakeReason::Unavailable,
                    Vec::new(),
                ),
            }));
        };
        // Connection-bound capability: a legacy token must not enter Join or
        // consult Broker ownership even if raw socket JSON sends return_when.
        if req.return_when.is_some() && !entry.coordination_v1 {
            return Ok(ProcessedStatus::plain(DelegationStatusBatch::joined(
                req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                DelegationWakeReason::Unavailable,
                Vec::new(),
            )));
        }
        // Identity-less hosts (Cursor) announce this MCP call as a generic
        // "MCP: tool" and never upgrade it on the wire — the companion
        // round-trip landing here is the FIRST moment anything knows the call
        // is `get_delegation_status`. Restore the live card's identity before
        // running the (possibly long-poll-blocked) status query; on ordinary
        // hosts the broker's sticky gate makes this a no-op.
        let mut rename_input = serde_json::Map::new();
        rename_input.insert(
            "task_ids".into(),
            serde_json::Value::Array(
                req.task_ids
                    .iter()
                    .map(|id| serde_json::Value::String(id.clone()))
                    .collect(),
            ),
        );
        if let Some(ms) = req.wait_ms {
            rename_input.insert("wait_ms".into(), serde_json::Value::Number(ms.into()));
        }
        let rewritten_status_tool_id = self
            .broker
            .rewrite_identityless_tool_call(
                &entry.parent_connection_id,
                crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE,
                serde_json::Value::Object(rename_input),
            )
            .await;
        let parent_conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        match req.return_when {
            // Snapshot or positive supervised waits: no indefinite arm registration.
            None if req.wait_ms != Some(0) => {
                Ok(ProcessedStatus::plain(DelegationStatusBatch::legacy(
                    self.broker
                        .get_tasks_status(
                            &entry.parent_connection_id,
                            parent_conversation_id,
                            &req.task_ids,
                            legacy_wait_from(req.wait_ms),
                        )
                        .await,
                )))
            }
            // Indefinite legacy terminal wait (`wait_ms: 0`, no return_when).
            None => {
                self.arm_indefinite_status_wait(
                    &entry,
                    &req,
                    parent_conversation_id,
                    rewritten_status_tool_id,
                    IndefiniteWaitKind::LegacyTerminal,
                )
                .await
            }
            Some(DelegationReturnWhen::AllTerminalOrAttention) if req.wait_ms == Some(0) => {
                if !entry.delegation_continuation_v1 {
                    return self
                        .arm_indefinite_status_wait(
                            &entry,
                            &req,
                            parent_conversation_id,
                            rewritten_status_tool_id,
                            IndefiniteWaitKind::CompatJoin,
                        )
                        .await;
                }
                if req.task_ids.is_empty() {
                    return Ok(ProcessedStatus::plain(
                        self.broker
                            .join_tasks_status(
                                &entry.parent_connection_id,
                                parent_conversation_id,
                                &req.task_ids,
                            )
                            .await,
                    ));
                }
                let Some(parent_conversation_id) = parent_conversation_id else {
                    return Ok(ProcessedStatus::plain(DelegationStatusBatch::joined(
                        req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                        DelegationWakeReason::Unavailable,
                        Vec::new(),
                    )));
                };
                self.arm_indefinite_status_wait(
                    &entry,
                    &req,
                    Some(parent_conversation_id),
                    rewritten_status_tool_id,
                    IndefiniteWaitKind::ContinuationJoin,
                )
                .await
            }
            Some(_) => Ok(ProcessedStatus::plain(DelegationStatusBatch::joined(
                req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                DelegationWakeReason::Unavailable,
                Vec::new(),
            ))),
        }
    }

    /// Canonical arm path for all indefinite status waits (legacy terminal,
    /// compatibility Join, continuation Join). Registers real canonical
    /// `task_ids`, binds exact `DelegationWait` when possible, parks with
    /// cancel-aware `select!`, and transfers ownership on continuation arm.
    async fn arm_indefinite_status_wait(
        &self,
        entry: &TokenEntry,
        req: &BrokerStatusRequest,
        parent_conversation_id: Option<i32>,
        rewritten_status_tool_id: Option<String>,
        kind: IndefiniteWaitKind,
    ) -> Result<ProcessedStatus, ContinuationError> {
        use crate::acp::tool_watchdog::{
            BindDelegationWaitResult, WaitCancelHandle, WaitOwner, WaitStamp,
        };

        let preflight_kind = match kind {
            IndefiniteWaitKind::LegacyTerminal => StatusWaitPreflightKind::LegacyTerminal,
            IndefiniteWaitKind::CompatJoin | IndefiniteWaitKind::ContinuationJoin => {
                StatusWaitPreflightKind::Join
            }
        };
        let preflight = self
            .broker
            .preflight_status_wait(
                &entry.parent_connection_id,
                parent_conversation_id,
                &req.task_ids,
                preflight_kind,
            )
            .await;
        let canonical_task_ids = match preflight {
            StatusWaitPreflight::Ready(batch) => return Ok(ProcessedStatus::plain(batch)),
            StatusWaitPreflight::NeedPark { canonical_task_ids } => canonical_task_ids,
        };

        // Request-associated wait tool id only — never heuristic scan.
        let parent_tool_use_id = resolve_wait_tool_id(req, rewritten_status_tool_id.as_deref());

        let wait_ctx = self
            .parent_lookup
            .parent_wait_context(&entry.parent_connection_id)
            .await;
        let parent_conversation_id =
            match parent_conversation_id.or_else(|| wait_ctx.as_ref().map(|c| c.conversation_id)) {
                Some(id) => id,
                None if matches!(kind, IndefiniteWaitKind::LegacyTerminal) => {
                    // Legacy terminal can park without a conversation id (in-memory).
                    // Use 0 only as stamp placeholder; reports still assemble correctly.
                    0
                }
                None => {
                    return Ok(ProcessedStatus::plain(DelegationStatusBatch::joined(
                        req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                        DelegationWakeReason::Unavailable,
                        Vec::new(),
                    )));
                }
            };

        let wait_id = uuid::Uuid::new_v4().to_string();
        let (cancel_tx, mut cancel_rx) =
            crate::acp::delegation::wait_cancel::new_wait_cancel_channel();
        let wait_stamp = WaitStamp {
            wait_id: wait_id.clone(),
            connection_id: entry.parent_connection_id.clone(),
            connection_incarnation: wait_ctx
                .as_ref()
                .map(|c| c.connection_incarnation.clone())
                .unwrap_or_default(),
            turn_generation: wait_ctx.as_ref().map(|c| c.turn_generation).unwrap_or(0),
            parent_conversation_id,
            parent_tool_use_id,
        };

        if self
            .wait_cancel
            .register(WaitCancelHandle {
                stamp: wait_stamp.clone(),
                owner: WaitOwner::Listener,
                cancel: cancel_tx,
                task_ids: canonical_task_ids.clone(),
            })
            .await
            .is_err()
        {
            // Fail closed: do not park without a live cancel handle.
            emit_wait_arm_reason("wait_register_failed");
            return Ok(ProcessedStatus::plain(match kind {
                IndefiniteWaitKind::LegacyTerminal => DelegationStatusBatch::legacy(
                    canonical_task_ids
                        .iter()
                        .map(|id| unknown_report(id))
                        .collect(),
                ),
                IndefiniteWaitKind::CompatJoin | IndefiniteWaitKind::ContinuationJoin => {
                    DelegationStatusBatch::joined(
                        canonical_task_ids
                            .iter()
                            .map(|id| unknown_report(id))
                            .collect(),
                        DelegationWakeReason::Unavailable,
                        Vec::new(),
                    )
                }
            }));
        }

        // Install immediately after successful register so peer-close that
        // abandons process_status during bind_delegation_wait still Drop-cleans
        // the registry entry (no ownerless wait stamp leak).
        let mut wait_guard = crate::acp::delegation::wait_cancel::WaitCancelGuard::new(
            self.wait_cancel.clone(),
            wait_stamp.clone(),
        );

        // Singleton and multi-task both bind when concrete tool id + lease exist.
        match self
            .parent_lookup
            .bind_delegation_wait(&entry.parent_connection_id, &wait_stamp)
            .await
        {
            BindDelegationWaitResult::Bound => {}
            BindDelegationWaitResult::WaitToolIdMissing => {
                emit_wait_arm_reason("wait_tool_id_missing");
            }
            BindDelegationWaitResult::WaitToolLeaseMismatch
            | BindDelegationWaitResult::WaitStampStale => {
                // Design closed set: WaitStampStale maps to wait_tool_lease_mismatch.
                emit_wait_arm_reason("wait_tool_lease_mismatch");
            }
            BindDelegationWaitResult::BindFailed => {
                emit_wait_arm_reason("wait_bind_failed");
            }
        }

        match kind {
            IndefiniteWaitKind::LegacyTerminal => {
                let park = self.broker.get_tasks_status(
                    &entry.parent_connection_id,
                    if parent_conversation_id == 0 {
                        None
                    } else {
                        Some(parent_conversation_id)
                    },
                    &canonical_task_ids,
                    StatusWait::Terminal,
                );
                tokio::pin!(park);
                tokio::select! {
                    biased;
                    reports = &mut park => {
                        let _ = self.wait_cancel.deregister(&wait_stamp).await;
                        wait_guard.disarm();
                        Ok(ProcessedStatus::plain(DelegationStatusBatch::legacy(reports)))
                    }
                    _ = cancel_rx.changed() => {
                        if crate::acp::delegation::wait_cancel::cancel_flag_set(&cancel_rx) {
                            let cause = crate::acp::delegation::wait_cancel::cancel_cause_of(
                                &cancel_rx,
                            )
                            .unwrap_or(crate::acp::tool_watchdog::CancelCause::AutoTimeout);
                            let _ = self.wait_cancel.deregister(&wait_stamp).await;
                            wait_guard.disarm();
                            Ok(ProcessedStatus::plain(DelegationStatusBatch::legacy(
                                canonical_task_ids
                                    .iter()
                                    .map(|id| wait_cancel_report(id, cause))
                                    .collect(),
                            )))
                        } else {
                            let reports = park.await;
                            let _ = self.wait_cancel.deregister(&wait_stamp).await;
                            wait_guard.disarm();
                            Ok(ProcessedStatus::plain(DelegationStatusBatch::legacy(reports)))
                        }
                    }
                }
            }
            IndefiniteWaitKind::CompatJoin => {
                let park = self.broker.join_tasks_status(
                    &entry.parent_connection_id,
                    Some(parent_conversation_id),
                    &canonical_task_ids,
                );
                tokio::pin!(park);
                tokio::select! {
                    biased;
                    batch = &mut park => {
                        let _ = self.wait_cancel.deregister(&wait_stamp).await;
                        wait_guard.disarm();
                        Ok(ProcessedStatus::plain(batch))
                    }
                    _ = cancel_rx.changed() => {
                        if crate::acp::delegation::wait_cancel::cancel_flag_set(&cancel_rx) {
                            let cause = crate::acp::delegation::wait_cancel::cancel_cause_of(
                                &cancel_rx,
                            )
                            .unwrap_or(crate::acp::tool_watchdog::CancelCause::AutoTimeout);
                            let _ = self.wait_cancel.deregister(&wait_stamp).await;
                            wait_guard.disarm();
                            Ok(ProcessedStatus::plain(DelegationStatusBatch::joined(
                                canonical_task_ids
                                    .iter()
                                    .map(|id| wait_cancel_report(id, cause))
                                    .collect(),
                                DelegationWakeReason::Unavailable,
                                Vec::new(),
                            )))
                        } else {
                            let batch = park.await;
                            let _ = self.wait_cancel.deregister(&wait_stamp).await;
                            wait_guard.disarm();
                            Ok(ProcessedStatus::plain(batch))
                        }
                    }
                }
            }
            IndefiniteWaitKind::ContinuationJoin => {
                let coordinator = self
                    .tokens
                    .continuation_coordinator()
                    .ok_or(ContinuationError::ArmWorkerDropped)?;
                let waiter_closed = CancellationToken::new();
                // Keep a clone for the cancel path: JoinArmRequest takes ownership,
                // and CancelWaiterOnDrop only fires when this future drops.
                let waiter_closed_for_cancel = waiter_closed.clone();
                let _cancel_waiter_on_drop = CancelWaiterOnDrop(waiter_closed.clone());
                let (transfer_tx, transfer_rx) = tokio::sync::oneshot::channel();
                let (release_owner, foreground_release) = foreground_mcp_release_fence();
                let request = JoinArmRequest {
                    parent_connection_id: entry.parent_connection_id.clone(),
                    parent_conversation_id,
                    task_ids: canonical_task_ids.clone(),
                    waiter_closed,
                    transferred_wait_rx: Some(transfer_rx),
                    foreground_release,
                };
                let wait_cancel_reg = self.wait_cancel.clone();
                let wait_stamp_for_arm = wait_stamp.clone();
                let transfer_task_ids = canonical_task_ids.clone();
                let cancel_rx_for_transfer = cancel_rx.clone();
                // Peer-close Drop cancels this token; TransferredWait watches it
                // to deregister without aborting the durable continuation.
                let waiter_closed_for_transfer = waiter_closed_for_cancel.clone();
                // After successful transfer_tx.send, clear this so peer-close
                // Drop cannot deregister the coordinator-owned wait via guard.
                // TransferredWait owns post-transfer registration cleanup.
                let transfer_disarm = wait_guard.drop_armed_flag();
                #[cfg(any(test, feature = "test-utils"))]
                let status_release_decision_gate = Arc::clone(&self.status_release_decision_gate);
                // JoinHandle must stay addressable in select: dropping without
                // abort() detaches the task in Tokio and can still transfer/suspend.
                let mut arm_task = tokio::spawn(async move {
                    match coordinator.begin_arm_from_join(request).await? {
                        JoinArmOutcome::Immediate(batch) => {
                            // Worker does not need wait ownership on Immediate.
                            drop(transfer_tx);
                            Ok::<ArmStatus, ContinuationError>(ArmStatus::Immediate(batch))
                        }
                        JoinArmOutcome::Arming { completion, .. } => {
                            match wait_cancel_reg
                                .transfer_owner(
                                    &wait_stamp_for_arm.wait_id,
                                    &wait_stamp_for_arm,
                                    WaitOwner::ContinuationCoordinator,
                                )
                                .await
                            {
                                Ok(()) => {
                                    // Linearizable handoff: transfer_owner already
                                    // flipped owner to ContinuationCoordinator.
                                    // WaitCancelGuard Drop is owner-aware and will
                                    // not remove coordinator rows. Optional test
                                    // gate parks here so peer-close can race the
                                    // residual window before transfer_tx.send.
                                    #[cfg(any(test, feature = "test-utils"))]
                                    wait_cancel_reg.observe_transfer_handoff_gate().await;

                                    // Abort if the registration vanished before send
                                    // (lost the race / concurrent explicit cleanup).
                                    if wait_cancel_reg
                                        .owner(&wait_stamp_for_arm.wait_id)
                                        .await
                                        != Some(
                                            crate::acp::tool_watchdog::WaitOwner::ContinuationCoordinator,
                                        )
                                    {
                                        emit_wait_arm_reason("wait_transfer_failed");
                                        drop(transfer_tx);
                                        let _ = wait_cancel_reg
                                            .deregister(&wait_stamp_for_arm)
                                            .await;
                                        return Err(ContinuationError::ArmWorkerDropped);
                                    }

                                    let transferred =
                                        crate::acp::delegation::wait_cancel::TransferredWait::new(
                                            wait_stamp_for_arm.clone(),
                                            transfer_task_ids,
                                            cancel_rx_for_transfer,
                                            wait_cancel_reg.clone(),
                                            waiter_closed_for_transfer,
                                        );
                                    if transfer_tx.send(transferred).is_err() {
                                        // Worker gone before handoff — deregister.
                                        let _ =
                                            wait_cancel_reg.deregister(&wait_stamp_for_arm).await;
                                        return Err(ContinuationError::ArmWorkerDropped);
                                    }
                                    // Coordinator owns cleanup: disarm listener
                                    // guard immediately (not only after Suspended)
                                    // so peer-close cannot Drop-deregister.
                                    transfer_disarm
                                        .store(false, std::sync::atomic::Ordering::SeqCst);
                                    completion
                                        .await
                                        .map_err(|_| ContinuationError::ArmWorkerDropped)??;
                                    #[cfg(any(test, feature = "test-utils"))]
                                    status_release_decision_gate.arm_suspended_ready();
                                    Ok::<ArmStatus, ContinuationError>(ArmStatus::Suspended)
                                }
                                Err(_) => {
                                    // Failed transfer is terminal for arming:
                                    // drop tx without send so worker aborts.
                                    emit_wait_arm_reason("wait_transfer_failed");
                                    drop(transfer_tx);
                                    let _ = wait_cancel_reg.deregister(&wait_stamp_for_arm).await;
                                    Err(ContinuationError::ArmWorkerDropped)
                                }
                            }
                        }
                    }
                });
                #[cfg(any(test, feature = "test-utils"))]
                self.status_release_decision_gate.before_select().await;
                let decision = tokio::select! {
                    biased;
                    joined = &mut arm_task => match joined
                        .map_err(|_| ContinuationError::ArmWorkerDropped)??
                    {
                        ArmStatus::Suspended => StatusReleaseDecision::Suspended,
                        ArmStatus::Immediate(batch) => {
                            let _ = self.wait_cancel.deregister(&wait_stamp).await;
                            wait_guard.disarm();
                            return Ok(ProcessedStatus::plain(batch));
                        }
                    },
                    changed = cancel_rx.changed() => {
                        if changed.is_err()
                            || !crate::acp::delegation::wait_cancel::cancel_flag_set(&cancel_rx)
                        {
                            return Err(ContinuationError::ArmWorkerDropped);
                        }
                        StatusReleaseDecision::WaitCancelled(
                            crate::acp::delegation::wait_cancel::cancel_cause_of(&cancel_rx)
                                .unwrap_or(crate::acp::tool_watchdog::CancelCause::AutoTimeout),
                        )
                    }
                };
                match decision {
                    StatusReleaseDecision::WaitCancelled(cause) => {
                        // Signal pre-insert races; abort+join so transfer cannot
                        // complete after cancel (JoinHandle drop would detach).
                        waiter_closed_for_cancel.cancel();
                        arm_task.abort();
                        let _ = arm_task.await;
                        let _ = self.wait_cancel.deregister(&wait_stamp).await;
                        wait_guard.disarm();
                        Ok(ProcessedStatus::plain(DelegationStatusBatch::joined(
                            canonical_task_ids
                                .iter()
                                .map(|task_id| wait_cancel_report(task_id, cause))
                                .collect(),
                            DelegationWakeReason::Unavailable,
                            Vec::new(),
                        )))
                    }
                    StatusReleaseDecision::Suspended => {
                        wait_guard.disarm();
                        Ok(ProcessedStatus {
                            batch: self
                                .continuation_release_batch(
                                    &entry.parent_connection_id,
                                    parent_conversation_id,
                                    &canonical_task_ids,
                                )
                                .await,
                            release_owner: Some(release_owner),
                        })
                    }
                }
            }
        }
    }

    async fn continuation_release_batch(
        &self,
        parent_connection_id: &str,
        parent_conversation_id: i32,
        canonical_task_ids: &[String],
    ) -> DelegationStatusBatch {
        let tasks = self
            .broker
            .get_tasks_status_snapshot(
                parent_connection_id,
                Some(parent_conversation_id),
                canonical_task_ids,
            )
            .await;
        DelegationStatusBatch::joined(tasks, DelegationWakeReason::Unavailable, Vec::new())
    }

    /// Stable non-secret rejection for unauthorized or capability-denied
    /// parent-decision attempts.
    fn decision_unavailable(code: &str, message: &str) -> ParentDecisionResult {
        ParentDecisionResult::Rejected {
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    /// Backs `request_parent_decision`. Token must advertise `coordination_v1`
    /// and role `DelegationChild`. Connection id bound to the token is the
    /// child's ACP connection (see injection).
    async fn process_parent_decision(
        &self,
        request: BrokerParentDecisionRequest,
    ) -> ParentDecisionResult {
        let Some(entry) = self.tokens.lookup(&request.token).await else {
            return Self::decision_unavailable(
                "unauthorized",
                "decision request is not authorized on this connection",
            );
        };
        if !entry.coordination_v1 {
            return Self::decision_unavailable(
                "coordination_unavailable",
                "delegation coordination is unavailable on this connection",
            );
        }
        if entry.role != CompanionRole::DelegationChild {
            return Self::decision_unavailable(
                "not_delegation_child",
                "only a live Codeg delegation child can request a parent decision",
            );
        }
        self.broker
            .request_parent_decision(
                &entry.parent_connection_id,
                &request.child_tool_call_id,
                &request.message,
            )
            .await
    }

    /// Backs `reply_to_delegation`. Any coordination-aware token may attempt
    /// a reply; Broker enforces direct-parent ownership.
    async fn process_reply_delegation(
        &self,
        request: BrokerReplyDelegationRequest,
    ) -> DelegationReplyResult {
        let Some(entry) = self.tokens.lookup(&request.token).await else {
            return DelegationReplyResult::Unauthorized;
        };
        if !entry.coordination_v1 {
            return DelegationReplyResult::Rejected {
                code: "coordination_unavailable".into(),
                message: "delegation coordination is unavailable on this connection".into(),
            };
        }
        let conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        self.broker
            .reply_to_delegation(
                &entry.parent_connection_id,
                conversation_id,
                &request.request_id,
                &request.reply,
            )
            .await
    }

    /// Backs the `cancel_delegation` tool. A `timeout` reason is explicitly
    /// non-canceling; every other reason validates the token, resolves the
    /// caller's parent, and cancels the task.
    async fn process_cancel_task(&self, req: BrokerCancelTaskRequest) -> DelegationTaskReport {
        if req.reason == CancelDelegationReason::Timeout {
            return timeout_cancel_guidance_report(&req.task_id);
        }
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return unknown_report(&req.task_id);
        };
        // Explicit task cancel (distinct from MCP request cancel).
        self.metrics.record_explicit_cancel(req.reason);
        crate::acp::delegation::metrics::DelegationAuditRecord::cancel(
            &entry.parent_connection_id,
            &req.task_id,
            req.reason,
        )
        .emit_cancel();
        // Same identity restoration as `process_status` — see the comment
        // there. `cancel_delegation` results are free-form text, so the
        // completion-time sniff can't cover this tool; the call-time rename
        // is its only identity source on identity-less hosts.
        let mut rename_input = serde_json::Map::new();
        rename_input.insert(
            "task_id".into(),
            serde_json::Value::String(req.task_id.clone()),
        );
        self.broker
            .rewrite_identityless_tool_call(
                &entry.parent_connection_id,
                crate::acp::delegation::CANCEL_TOOL_REWRITE_TITLE,
                serde_json::Value::Object(rename_input),
            )
            .await;
        let parent_conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        self.broker
            .cancel_task_by_id(
                &entry.parent_connection_id,
                parent_conversation_id,
                &req.task_id,
                req.reason.as_str(),
            )
            .await
    }

    /// Validate the token and resolve the `check_user_feedback` target: the
    /// caller's parent connection id. `None` on an invalid token — the LLM can't
    /// usefully distinguish "no notes" from "bad token", and we don't leak which.
    async fn feedback_target(&self, req: &BrokerFeedbackRequest) -> Option<String> {
        let entry = self.tokens.lookup(&req.token).await?;
        Some(entry.parent_connection_id)
    }

    /// Validate the token and resolve the `ask_user_question` target: the
    /// caller's parent connection id. `None` on an invalid token — the LLM gets
    /// a `declined` outcome (proceed with judgment), and we don't leak which.
    async fn ask_target(&self, req: &BrokerAskRequest) -> Option<String> {
        let entry = self.tokens.lookup(&req.token).await?;
        Some(entry.parent_connection_id)
    }

    /// Mark the named feedback notes delivered, after the companion confirms it
    /// returned them to the agent. Token-scoped to the parent connection. Unknown
    /// tokens are dropped (no LLM on the receiving end to react).
    async fn process_commit_feedback(&self, req: BrokerCommitFeedbackRequest) {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return;
        };
        self.feedback
            .commit_feedback_delivered(&entry.parent_connection_id, req.ids)
            .await;
    }

    /// Validate token + dispatch cancel to the broker. Unknown tokens and
    /// parent-mismatched cancels are silently dropped — there's no LLM on
    /// the receiving end of this method to react to errors.
    async fn process_cancel(&self, cancel: BrokerCancelRequest) {
        let Some(_entry) = self.tokens.lookup(&cancel.token).await else {
            return;
        };
        // MCP tools/call cancellation — not an explicit cancel_delegation.
        self.metrics.record_mcp_request_cancel();
        let reason = cancel
            .reason
            .unwrap_or_else(|| "mcp client canceled".into());
        self.broker
            .cancel_by_external_handle(&cancel.external_handle, reason)
            .await;
    }

    /// Validate the token and resolve the `get_session_info` target. An invalid
    /// token yields a `found:false` outcome (the LLM can't usefully distinguish it
    /// from a deleted session, and we don't leak which).
    ///
    /// SCOPE (deliberate, user-confirmed): the lookup is by codeg conversation id
    /// and is intentionally NOT scoped to the caller's parent connection or to the
    /// session ids actually referenced in the prompt — any non-deleted session
    /// resolves. This is sound in codeg's single-tenant trust model: there is no
    /// per-user isolation anywhere (desktop is one local user; server mode shares
    /// one `CODEG_TOKEN` + one data dir across an operator's devices), the user can
    /// already open every session in the UI, and the agent already has full
    /// filesystem access to every agent's raw session files via its own tools — so
    /// reading session metadata by id is strictly less capability than the agent
    /// already holds, not an escalation. The token gate above still prevents an
    /// unrelated process from reaching the broker at all.
    async fn process_session_info(&self, req: BrokerSessionRequest) -> SessionInfo {
        if self.tokens.lookup(&req.token).await.is_none() {
            return SessionInfo::not_found(req.session_id);
        }
        self.session_info
            .resolve(req.session_id, req.max_messages.unwrap_or(0))
            .await
    }

    /// Auth + Root/`workflow_v2` gate for workflow mutation/recovery tools.
    async fn workflow_auth_context(
        &self,
        token: &str,
    ) -> Result<(TokenEntry, i32), WorkflowWireError> {
        let entry = self
            .tokens
            .lookup(token)
            .await
            .ok_or(WorkflowWireError::InvalidToken)?;
        if !entry.workflow_v2 {
            return Err(WorkflowWireError::FeatureDisabled);
        }
        if entry.role != CompanionRole::Root {
            return Err(WorkflowWireError::RootOnly);
        }
        let parent_conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await
            .ok_or(WorkflowWireError::NoActiveConversation)?;
        Ok((entry, parent_conversation_id))
    }

    async fn restart_legacy_if_required(
        &self,
        db: &crate::db::AppDatabase,
        parent_conversation_id: i32,
        rollout_subject: Option<(String, Option<String>)>,
    ) -> Result<
        Option<crate::acp::delegation::workflow::LegacyWorkflowRestartProjection>,
        WorkflowStoreError,
    > {
        match restart_legacy_workflow_if_enforced(
            db,
            parent_conversation_id,
            rollout_subject,
            &self.completion_protocol_rollout,
        )
        .await
        {
            Ok(Some(projection)) => {
                self.metrics
                    .record_completion_restart(if projection.idempotent_replay {
                        CompletionRestartOutcome::Reused
                    } else {
                        CompletionRestartOutcome::Created
                    });
                Ok(Some(projection))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.metrics
                    .record_completion_restart(CompletionRestartOutcome::Failed);
                Err(error)
            }
        }
    }

    async fn process_publish_workflow(&self, req: BrokerPublishWorkflowRequest) -> Value {
        let parent_conversation_id = match self.workflow_auth_context(&req.token).await {
            Ok((_, id)) => id,
            Err(e) => return e.to_value(),
        };
        let Some(runs) = self.broker.run_store() else {
            return WorkflowWireError::StoreUnavailable.to_value();
        };
        let document: ManifestDocument = match serde_json::from_value(req.document) {
            Ok(d) => d,
            Err(e) => {
                return WorkflowWireError::InvalidArguments(format!(
                    "publish_workflow_manifest document: {e}"
                ))
                .to_value();
            }
        };
        let rollout_subject = document
            .nodes
            .iter()
            .find(|node| {
                node.role == Some(crate::acp::delegation::workflow::ManifestNodeRole::Author)
            })
            .or_else(|| document.nodes.iter().find(|node| node.agent_type.is_some()));
        let rollout_subject_owned = rollout_subject.map(|node| {
            (
                node.agent_type.clone().unwrap_or_else(|| "unknown".into()),
                node.profile_id.clone(),
            )
        });
        match self
            .restart_legacy_if_required(runs.db(), parent_conversation_id, rollout_subject_owned)
            .await
        {
            Ok(Some(projection)) => {
                return serde_json::to_value(projection).unwrap_or_else(|error| {
                    WorkflowWireError::Internal(format!("serialize legacy restart result: {error}"))
                        .to_value()
                })
            }
            Ok(None) => {}
            Err(error) => return workflow_store_error_value(error),
        }
        let selection = select_completion_protocol(
            rollout_subject
                .and_then(|node| node.agent_type.as_deref())
                .unwrap_or("unknown"),
            rollout_subject.and_then(|node| node.profile_id.as_deref()),
            &self.completion_protocol_rollout,
        );
        let creation_mode = selection.mode.clone();
        match publish_workflow_manifest_with_selection_core(
            runs.db(),
            &self.workflow_emitter,
            parent_conversation_id,
            PublishWorkflowRequest { document },
            selection,
        )
        .await
        {
            Ok(r) => {
                if r.manifest_revision == 1 && !r.idempotent_replay {
                    self.metrics
                        .record_completion_protocol_creation(creation_mode);
                }
                serde_json::to_value(r).unwrap_or_else(|e| {
                    WorkflowWireError::Internal(format!("serialize publish result: {e}")).to_value()
                })
            }
            Err(e) => workflow_store_error_value(e),
        }
    }

    async fn process_settle_workflow(&self, req: BrokerSettleWorkflowRequest) -> Value {
        let parent_conversation_id = match self.workflow_auth_context(&req.token).await {
            Ok((_, id)) => id,
            Err(e) => return e.to_value(),
        };
        let Some(runs) = self.broker.run_store() else {
            return WorkflowWireError::StoreUnavailable.to_value();
        };
        let header = match delegation_workflow::Entity::find_by_id(&req.workflow_id)
            .one(&runs.db().conn)
            .await
        {
            Ok(Some(header)) => header,
            Ok(None) => {
                return workflow_store_error_value(WorkflowStoreError::NotFound(req.workflow_id))
            }
            Err(error) => {
                return WorkflowWireError::Internal(format!(
                    "load workflow settlement protocol: {error}"
                ))
                .to_value()
            }
        };
        if header.parent_conversation_id != parent_conversation_id {
            return workflow_store_error_value(WorkflowStoreError::CrossParent {
                workflow_id: header.workflow_id,
                expected_parent: parent_conversation_id,
                actual_parent: header.parent_conversation_id,
            });
        }
        match self
            .restart_legacy_if_required(runs.db(), parent_conversation_id, None)
            .await
        {
            Ok(Some(projection)) => {
                return serde_json::to_value(projection).unwrap_or_else(|error| {
                    WorkflowWireError::Internal(format!("serialize legacy restart result: {error}"))
                        .to_value()
                })
            }
            Ok(None) => {}
            Err(error) => return workflow_store_error_value(error),
        }

        let result = if header.completion_protocol_version == 2 {
            if req.manifest_revision.is_some()
                || req.gate_cycle.is_some()
                || req.outcome.is_some()
                || req.evidence.is_some()
            {
                return WorkflowWireError::InvalidArguments(
                    "protocol-v2 settlement rejects legacy manifest, cycle, outcome, and evidence fields"
                        .into(),
                )
                .to_value();
            }
            let expected_review_round = match (req.expected_review_round, req.expected_gate_cycle) {
                (Some(round), Some(cycle)) if round != cycle => {
                    return WorkflowWireError::InvalidArguments(
                        "expected_review_round and expected_gate_cycle disagree".into(),
                    )
                    .to_value()
                }
                (round, cycle) => round.or(cycle),
            };
            let expected_outcome = match req.expected_outcome.as_deref() {
                Some(value) => match parse_gate_settlement_outcome(value) {
                    Ok(outcome) => Some(outcome),
                    Err(message) => return WorkflowWireError::InvalidArguments(message).to_value(),
                },
                None => None,
            };
            settle_workflow_gate_v2_core(
                runs.db(),
                &self.workflow_emitter,
                parent_conversation_id,
                SettleWorkflowV2Request {
                    workflow_id: req.workflow_id,
                    gate_id: req.gate_id,
                    expected_graph_revision: req.expected_graph_revision,
                    expected_review_round,
                    expected_outcome,
                    summary: req.summary,
                    recovery_authorization_id: req.recovery_authorization_id,
                },
            )
            .await
        } else {
            if req.expected_review_round.is_some()
                || req.expected_gate_cycle.is_some()
                || req.expected_outcome.is_some()
            {
                return WorkflowWireError::InvalidArguments(
                    "protocol-v1 settlement requires the legacy request shape".into(),
                )
                .to_value();
            }
            let outcome = match req.outcome.as_deref() {
                Some(value) => match parse_gate_settlement_outcome(value) {
                    Ok(outcome) => outcome,
                    Err(message) => return WorkflowWireError::InvalidArguments(message).to_value(),
                },
                None => {
                    return WorkflowWireError::InvalidArguments(
                        "protocol-v1 settlement requires outcome".into(),
                    )
                    .to_value()
                }
            };
            let evidence = match req.evidence {
                Some(value) => match serde_json::from_value(value) {
                    Ok(evidence) => evidence,
                    Err(error) => {
                        return WorkflowWireError::InvalidArguments(format!(
                            "settle_workflow_gate evidence: {error}"
                        ))
                        .to_value()
                    }
                },
                None => {
                    return WorkflowWireError::InvalidArguments(
                        "protocol-v1 settlement requires evidence".into(),
                    )
                    .to_value()
                }
            };
            let Some(manifest_revision) = req.manifest_revision else {
                return WorkflowWireError::InvalidArguments(
                    "protocol-v1 settlement requires manifest_revision".into(),
                )
                .to_value();
            };
            let Some(gate_cycle) = req.gate_cycle else {
                return WorkflowWireError::InvalidArguments(
                    "protocol-v1 settlement requires gate_cycle".into(),
                )
                .to_value();
            };
            settle_workflow_gate_core(
                runs.db(),
                &self.workflow_emitter,
                parent_conversation_id,
                SettleWorkflowRequest {
                    workflow_id: req.workflow_id,
                    manifest_revision,
                    gate_id: req.gate_id,
                    expected_graph_revision: req.expected_graph_revision,
                    gate_cycle,
                    outcome,
                    evidence,
                    summary: req.summary,
                    recovery_authorization_id: req.recovery_authorization_id,
                },
            )
            .await
        };
        match result {
            Ok(r) => {
                if let Some(action) = r.plan_next_action {
                    self.metrics.record_completion_plan_reducer(
                        action,
                        r.stagnation_count,
                        r.rewrite_used,
                    );
                    self.metrics.record_completion_continuation(
                        crate::acp::delegation::metrics::CompletionContinuationReason::PlanReview,
                    );
                }
                if let Some(observation) = r.plan_metric_observation {
                    self.metrics.record_completion_plan_classification(
                        observation.change,
                        observation.localized_intersection,
                        observation.lineage_reset,
                    );
                    self.metrics
                        .record_completion_sibling_reruns(observation.sibling_reruns);
                }
                serde_json::to_value(r).unwrap_or_else(|e| {
                    WorkflowWireError::Internal(format!("serialize settle result: {e}")).to_value()
                })
            }
            Err(e) => workflow_store_error_value(e),
        }
    }

    async fn process_complete_work(&self, req: BrokerCompleteWorkRequest) -> Value {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return completion_work_error_value(
                "completion_tool_unauthorized",
                "completion tool is not authorized for this live child",
            );
        };
        if !entry.completion_v2 || entry.role != CompanionRole::DelegationChild {
            return completion_work_error_value(
                "completion_tool_unauthorized",
                "completion tool is not authorized for this live child",
            );
        }
        let Some(task_id) = entry.bound_task_id.as_deref() else {
            return completion_work_error_value(
                "completion_tool_unauthorized",
                "completion tool is not authorized for this live child",
            );
        };
        let Some(runs) = self.broker.run_store() else {
            return completion_work_error_value(
                "completion_tool_unauthorized",
                "completion tool is not authorized for this live child",
            );
        };
        match accept_complete_work_txn(
            runs.db(),
            task_id,
            &entry.parent_connection_id,
            &req.child_tool_call_id,
            &req.request,
        )
        .await
        {
            Ok(intent) => {
                if let Ok(Some(context)) = runs.terminal_completion_resolver_context(task_id).await
                {
                    self.metrics.record_completion_tool_accepted(context.role);
                }
                serde_json::to_value(intent).unwrap_or_else(|error| {
                    completion_work_error_value("persistence", &error.to_string())
                })
            }
            Err(error) => completion_work_error_value(error.code(), &error.to_string()),
        }
    }

    async fn process_get_workflow_state(&self, req: BrokerGetWorkflowStateRequest) -> Value {
        let parent_conversation_id = match self.workflow_auth_context(&req.token).await {
            Ok((_, id)) => id,
            Err(e) => return e.to_value(),
        };
        let Some(runs) = self.broker.run_store() else {
            return WorkflowWireError::StoreUnavailable.to_value();
        };
        match guard_current_final_delivery_core(
            runs.db(),
            &self.workflow_emitter,
            parent_conversation_id,
            req.workflow_id.as_deref(),
        )
        .await
        {
            Ok(Some(FinalDeliveryGuardResult::Ready(_))) | Ok(None) => {}
            Ok(Some(FinalDeliveryGuardResult::Rejected(diagnostic)))
            | Ok(Some(FinalDeliveryGuardResult::Reopened { diagnostic, .. })) => {
                return serde_json::json!({
                    "error": {
                        "code": diagnostic.code(),
                        "message": diagnostic.to_string(),
                    }
                });
            }
            Err(error) => return workflow_store_error_value(error),
        }
        match get_workflow_state_core(
            runs.db(),
            parent_conversation_id,
            req.workflow_id.as_deref(),
        )
        .await
        {
            Ok(r) => serde_json::to_value(r).unwrap_or_else(|e| {
                WorkflowWireError::Internal(format!("serialize workflow state: {e}")).to_value()
            }),
            Err(e) => workflow_store_error_value(e),
        }
    }

    async fn process_restart_legacy_workflow(
        &self,
        req: BrokerRestartLegacyWorkflowRequest,
    ) -> Value {
        let parent_conversation_id = match self.workflow_auth_context(&req.token).await {
            Ok((_, id)) => id,
            Err(error) => return error.to_value(),
        };
        if i64::from(parent_conversation_id) != req.source_conversation_id {
            self.metrics
                .record_completion_restart(CompletionRestartOutcome::Rejected);
            return workflow_store_error_value(WorkflowStoreError::CrossParent {
                workflow_id: "legacy_restart".into(),
                expected_parent: parent_conversation_id,
                actual_parent: i32::try_from(req.source_conversation_id).unwrap_or_default(),
            });
        }
        let Some(runs) = self.broker.run_store() else {
            return WorkflowWireError::StoreUnavailable.to_value();
        };
        match restart_legacy_workflow_if_enforced(
            runs.db(),
            parent_conversation_id,
            None,
            &self.completion_protocol_rollout,
        )
        .await
        {
            Ok(Some(projection)) => {
                self.metrics
                    .record_completion_restart(if projection.idempotent_replay {
                        CompletionRestartOutcome::Reused
                    } else {
                        CompletionRestartOutcome::Created
                    });
                serde_json::to_value(projection).unwrap_or_else(|error| {
                    WorkflowWireError::Internal(format!("serialize legacy restart result: {error}"))
                        .to_value()
                })
            }
            Ok(None) => {
                self.metrics
                    .record_completion_restart(CompletionRestartOutcome::Rejected);
                workflow_store_error_value(
                    WorkflowStoreError::LegacyCompletionProtocolRestartInvalid(
                        "legacy restart requires current v2_enforce mode".into(),
                    ),
                )
            }
            Err(error) => {
                self.metrics
                    .record_completion_restart(CompletionRestartOutcome::Failed);
                workflow_store_error_value(error)
            }
        }
    }

    async fn process_recovery_authorization(
        &self,
        req: BrokerRecoveryAuthorizationRequest,
        cancelled: CancellationToken,
    ) -> Value {
        if let Err(message) = validate_correlation_id(&req.correlation_id) {
            return recovery_wire_error("invalid_correlation_id", &message);
        }
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return recovery_wire_error("invalid_token", "invalid token");
        };
        let Some(parent_conversation_id) = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await
        else {
            return recovery_wire_error(
                "no_active_conversation",
                "parent has no active conversation",
            );
        };
        let Some(service) = self.recovery_authorizations.as_ref() else {
            return recovery_wire_error(
                "store_unavailable",
                "recovery authorization store is unavailable",
            );
        };

        let challenge = match req.subject_kind {
            RecoverySubjectKind::DelegationTask => {
                if !entry.coordination_v1 {
                    return recovery_wire_error(
                        "feature_disabled",
                        "delegation recovery is not enabled",
                    );
                }
                if req.proposed_user_reason.is_some() {
                    return recovery_wire_error(
                        "invalid_arguments",
                        "proposed_user_reason is not accepted for delegation recovery",
                    );
                }
                match self
                    .delegation_recovery_challenge(parent_conversation_id, &req.subject_id)
                    .await
                {
                    Ok(challenge) => challenge,
                    Err(error) => return error,
                }
            }
            RecoverySubjectKind::Workflow => {
                if !entry.workflow_v2 {
                    return recovery_wire_error(
                        "feature_disabled",
                        "workflow_v2 is not enabled for this companion",
                    );
                }
                if entry.role != CompanionRole::Root {
                    return recovery_wire_error(
                        "root_only",
                        "workflow recovery authorization is Root-only",
                    );
                }
                match self
                    .workflow_recovery_challenge(
                        parent_conversation_id,
                        &req.subject_id,
                        req.proposed_user_reason.as_deref(),
                    )
                    .await
                {
                    Ok(challenge) => challenge,
                    Err(error) => return error,
                }
            }
        };

        let prepared = match service.prepare(challenge.clone()).await {
            Ok(prepared) => prepared,
            Err(error) => return recovery_authorization_error_value(error),
        };
        match prepared {
            PreparedAuthorization::ExistingApproved(result) => {
                recovery_authorization_result_value(&result, true)
            }
            PreparedAuthorization::Pending {
                row,
                newly_created: false,
            } => match service
                .wait_for_resolution(&row.authorization_id, cancelled)
                .await
            {
                Ok(result) => recovery_authorization_result_value(&result, true),
                Err(error) => recovery_authorization_error_value(error),
            },
            PreparedAuthorization::Pending {
                row,
                newly_created: true,
            } => {
                self.drive_new_recovery_question(
                    service,
                    &entry.parent_connection_id,
                    &challenge,
                    row.authorization_id,
                    cancelled,
                )
                .await
            }
            PreparedAuthorization::NotRequired { .. } => recovery_wire_error(
                "recovery_authorization_not_required",
                "central recovery policy does not require authorization",
            ),
            PreparedAuthorization::HardStop { code } => recovery_wire_error(
                "recovery_authorization_blocked",
                &format!("central recovery policy blocked authorization: {code}"),
            ),
        }
    }

    async fn delegation_recovery_challenge(
        &self,
        parent_conversation_id: i32,
        task_id: &str,
    ) -> Result<RecoveryChallenge, Value> {
        let Some(runs) = self.broker.run_store() else {
            return Err(recovery_wire_error(
                "store_unavailable",
                "delegation run store is unavailable",
            ));
        };
        let target = runs
            .load_by_task_id(task_id)
            .await
            .map_err(|_| {
                recovery_wire_error("recovery_subject_load_failed", "failed to load task")
            })?
            .ok_or_else(|| {
                recovery_wire_error("recovery_subject_not_found", "task was not found")
            })?;
        if target.parent_conversation_id != parent_conversation_id {
            return Err(recovery_wire_error(
                "recovery_subject_not_owned",
                "task is not directly owned by this caller",
            ));
        }
        let eligibility = runs
            .build_continue_eligibility(&target)
            .await
            .map_err(|_| {
                recovery_wire_error(
                    "recovery_subject_load_failed",
                    "failed to derive task recovery state",
                )
            })?;
        let decision = decide_delegation_recovery(
            &recovery_source_from_continue_eligibility(&eligibility),
            &RecoveryRailSnapshot {
                agent_supports_reuse: eligibility.agent_supports_reuse,
                unexpected_continue_budget_available: eligibility
                    .unexpected_continue_budget_available,
                replacement_budget_available: eligibility.replacement_budget_available,
            },
            RequestedRecoveryOperation::Inspect,
        );
        if !decision.requires_authorization() {
            return Err(recovery_wire_error(
                "recovery_authorization_not_required",
                "central recovery policy does not project a confirmable action",
            ));
        }
        let (allowed_action, operation) = match decision.proposed_action() {
            Some(RecoveryAction::Continue { .. }) => (
                RecoveryAllowedAction::Continue,
                RequestedRecoveryOperation::Continue,
            ),
            Some(RecoveryAction::FreshDispatch) => (
                RecoveryAllowedAction::FreshDispatch,
                RequestedRecoveryOperation::FreshDispatch,
            ),
            Some(RecoveryAction::Replace { replacement_reason }) => (
                RecoveryAllowedAction::Replace,
                RequestedRecoveryOperation::Replace { replacement_reason },
            ),
            None => {
                return Err(recovery_wire_error(
                    "recovery_authorization_not_required",
                    "central recovery policy does not project an action",
                ))
            }
        };
        Ok(RecoveryChallenge {
            parent_conversation_id,
            subject_kind: RecoverySubjectKind::DelegationTask,
            subject_id: target.task_id.clone(),
            delegation_identity: Some(DelegationAuthorizationIdentity {
                source_task_id: target.task_id,
                child_conversation_id: Some(target.child_conversation_id),
                lineage_root_task_id: target.lineage_root_task_id,
                work_unit_key: target.work_unit_key,
            }),
            source_state_fingerprint: decision.source_state_fingerprint.clone(),
            allowed_action,
            action_payload: recovery_action_payload(&operation),
            cause_code: serialized_recovery_code(&decision.cause_code),
            risk_class: serialized_recovery_code(&decision.risk_class),
            display_reason: None,
        })
    }

    async fn workflow_recovery_challenge(
        &self,
        parent_conversation_id: i32,
        workflow_id: &str,
        proposed_user_reason: Option<&str>,
    ) -> Result<RecoveryChallenge, Value> {
        let Some(runs) = self.broker.run_store() else {
            return Err(recovery_wire_error(
                "store_unavailable",
                "workflow store is unavailable",
            ));
        };
        if let Some(reason) = proposed_user_reason {
            if reason.trim().is_empty() || reason.len() > 4096 {
                return Err(recovery_wire_error(
                    "invalid_arguments",
                    "proposed_user_reason must be nonblank and at most 4096 UTF-8 bytes",
                ));
            }
        }
        let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&runs.db().conn)
            .await
            .map_err(|_| {
                recovery_wire_error("recovery_subject_load_failed", "failed to load workflow")
            })?
            .ok_or_else(|| {
                recovery_wire_error("recovery_subject_not_found", "workflow was not found")
            })?;
        if header.parent_conversation_id != parent_conversation_id {
            return Err(recovery_wire_error(
                "recovery_subject_not_owned",
                "workflow is not owned by this caller",
            ));
        }
        let snapshot =
            crate::acp::delegation::workflow::store::load_workflow_recovery_snapshot_conn(
                &runs.db().conn,
                &header,
                proposed_user_reason,
            )
            .await
            .map_err(workflow_store_error_value)?;
        let decision = decide_workflow_recovery(&snapshot);
        if !decision.requires_authorization() {
            return Err(recovery_wire_error(
                "recovery_authorization_not_required",
                "central workflow policy does not project a confirmable action",
            ));
        }
        let (allowed_action, display_reason) = match &decision.disposition {
            WorkflowRecoveryDisposition::Recover { .. } => {
                if proposed_user_reason.is_some() {
                    return Err(recovery_wire_error(
                        "invalid_arguments",
                        "proposed_user_reason is not accepted for generic workflow recovery",
                    ));
                }
                (RecoveryAllowedAction::RecoverWorkflow, None)
            }
            WorkflowRecoveryDisposition::ResetPlanLineage => (
                RecoveryAllowedAction::ResetPlanLineage,
                proposed_user_reason.map(str::to_string),
            ),
            _ => {
                return Err(recovery_wire_error(
                    "recovery_authorization_not_required",
                    "central workflow policy does not project an action",
                ))
            }
        };
        Ok(RecoveryChallenge {
            parent_conversation_id,
            subject_kind: RecoverySubjectKind::Workflow,
            subject_id: workflow_id.to_string(),
            delegation_identity: None,
            source_state_fingerprint: decision.source_state_fingerprint.clone(),
            allowed_action,
            action_payload: decision
                .action_payload()
                .expect("authorized workflow decision has action payload"),
            cause_code: decision.cause_code.as_str().to_string(),
            risk_class: decision.risk_class.as_str().to_string(),
            display_reason,
        })
    }

    async fn drive_new_recovery_question(
        &self,
        service: &RecoveryAuthorizationService,
        parent_connection_id: &str,
        challenge: &RecoveryChallenge,
        authorization_id: String,
        cancelled: CancellationToken,
    ) -> Value {
        let Some(metadata) =
            derive_recovery_action_metadata(challenge.allowed_action, &challenge.action_payload)
        else {
            return recovery_wire_error(
                "recovery_authorization_contract_invalid",
                "derived recovery action payload is invalid",
            );
        };
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
            recovery: Some(RecoveryQuestionPresentation {
                subject: challenge.subject_kind.as_str().to_string(),
                action: challenge.allowed_action.as_str().to_string(),
                target: metadata.target_code.to_string(),
                cause: challenge.cause_code.clone(),
                risk: challenge.risk_class.clone(),
                display_reason: challenge.display_reason.clone(),
            }),
        }];
        let Some(registration) = self
            .questions
            .register_question(parent_connection_id, questions)
            .await
        else {
            let _ = service
                .abandon_until_terminal(&authorization_id, None)
                .await;
            return recovery_wire_error(
                "recovery_authorization_blocked",
                "recovery question could not be registered",
            );
        };
        if let Err(error) = service
            .bind_question(&authorization_id, &registration.question_id)
            .await
        {
            self.questions
                .cancel_question(parent_connection_id, &registration.question_id)
                .await;
            let _ = service
                .abandon_until_terminal(&authorization_id, None)
                .await;
            return recovery_authorization_error_value(error);
        }

        let question_id = registration.question_id;
        let outcome = tokio::select! {
            biased;
            _ = cancelled.cancelled() => None,
            outcome = registration.answer_rx => outcome.ok(),
        };
        match outcome {
            Some(outcome) => match service.resolve_question(&authorization_id, outcome).await {
                Ok(result) => recovery_authorization_result_value(&result, false),
                Err(error) => recovery_authorization_error_value(error),
            },
            None => {
                self.questions
                    .cancel_question(parent_connection_id, &question_id)
                    .await;
                match service
                    .abandon_question(&authorization_id, &question_id)
                    .await
                {
                    Ok(()) => match service.get(&authorization_id).await {
                        Ok(row) => recovery_authorization_row_value(&row, false),
                        Err(error) => recovery_authorization_error_value(error),
                    },
                    Err(error) => recovery_authorization_error_value(error),
                }
            }
        }
    }

    async fn process_recover_workflow(&self, req: BrokerRecoverWorkflowRequest) -> Value {
        if let Err(message) = validate_correlation_id(&req.correlation_id) {
            return recovery_wire_error("invalid_correlation_id", &message);
        }
        let parent_conversation_id = match self.workflow_auth_context(&req.token).await {
            Ok((_, id)) => id,
            Err(error) => return error.to_value(),
        };
        let Some(runs) = self.broker.run_store() else {
            return WorkflowWireError::StoreUnavailable.to_value();
        };
        match self
            .restart_legacy_if_required(runs.db(), parent_conversation_id, None)
            .await
        {
            Ok(Some(projection)) => {
                return serde_json::to_value(projection).unwrap_or_else(|error| {
                    WorkflowWireError::Internal(format!("serialize legacy restart result: {error}"))
                        .to_value()
                })
            }
            Ok(None) => {}
            Err(error) => return workflow_store_error_value(error),
        }
        match recover_workflow_core(
            runs.db(),
            &self.workflow_emitter,
            parent_conversation_id,
            RecoverWorkflowRequest {
                workflow_id: req.workflow_id,
                recovery_authorization_id: req.recovery_authorization_id,
                expected_manifest_revision: req.expected_manifest_revision,
                correlation_id: req.correlation_id,
            },
        )
        .await
        {
            Ok(result) => serde_json::to_value(result).unwrap_or_else(|error| {
                WorkflowWireError::Internal(format!("serialize recover workflow result: {error}"))
                    .to_value()
            }),
            Err(error) => workflow_store_error_value(error),
        }
    }

    async fn process(&self, req: BrokerRequest) -> DelegationTaskReport {
        // 1. Token + parent_connection_id consistency check. Treat both as
        //    "canceled" since the LLM can't usefully react to either —
        //    the parent has either been torn down or is impersonating.
        let entry = match self.tokens.lookup(&req.token).await {
            Some(e) => e,
            None => return cancel("invalid token"),
        };
        if entry.parent_connection_id != req.parent_connection_id {
            return cancel("token does not match parent connection");
        }

        // 2. Resolve the parent's current conversation. Without one the
        //    broker can't link the child row to the parent.
        let parent_conversation_id = match self
            .parent_lookup
            .current_conversation_id(&req.parent_connection_id)
            .await
        {
            Some(id) => id,
            None => return cancel("parent has no active conversation"),
        };

        if entry.role == CompanionRole::Root && entry.workflow_v2 {
            if let Some(runs) = self.broker.run_store() {
                match self
                    .restart_legacy_if_required(runs.db(), parent_conversation_id, None)
                    .await
                {
                    Ok(Some(projection)) => {
                        return report_failed(
                            "legacy_completion_protocol_restart_required",
                            &format!(
                            "legacy workflow is read-only; continue in successor conversation {}",
                            projection.successor_conversation_id
                        ),
                        )
                    }
                    Ok(None) => {}
                    Err(error) => return report_failed(error.code(), &error.to_string()),
                }
            }
        }

        let work_unit_key = match parse_work_unit_key(&req.input) {
            Ok(key) => key,
            Err(message) => return report_failed("invalid_work_unit_key", &message),
        };

        // continue_delegation: tagged by companion or shape (task_id + task,
        // no agent_type). Must run before agent_type is required.
        let is_continue = req.input.get("_codeg_tool").and_then(|v| v.as_str())
            == Some("continue_delegation")
            || (req.input.get("task_id").is_some()
                && req.input.get("agent_type").is_none()
                && req.input.get("task").is_some());

        // Correlation resolution order (design): non-empty host `_meta.tool_use_id`
        // is authoritative and does not require argument-based correlation.
        // Only when the host id is empty do we validate/require correlation_id
        // grammar (malformed / explicit null → fail closed here).
        // Whitespace-only ids are absent, but every nonblank host id is an
        // opaque identity and must retain its original bytes.
        let parent_tool_use_id = if req.parent_tool_use_id.trim().is_empty() {
            String::new()
        } else {
            req.parent_tool_use_id
        };
        let host_tool_id_present = !parent_tool_use_id.is_empty();
        let correlation_id = if host_tool_id_present {
            // Best-effort forward of a valid correlation_id; ignore malformed.
            parse_correlation_id(&req.input).ok().flatten()
        } else {
            match parse_correlation_id(&req.input) {
                Ok(id) => id,
                Err(_) => {
                    let entry = if is_continue {
                        CorrelationEntryPoint::ContinueDelegation
                    } else {
                        CorrelationEntryPoint::DelegateToAgent
                    };
                    let message = correlation_error_message(CorrelationFailureKind::Missing, entry);
                    return report_failed(CorrelationFailureKind::Missing.wire_code(), &message);
                }
            }
        };

        let recovery_authorization_id = match parse_recovery_authorization_id(&req.input) {
            Ok(id) => id,
            Err(message) => return report_failed("invalid_recovery_authorization_id", &message),
        };

        if is_continue {
            let target_task_id = match req.input.get("task_id").and_then(|v| v.as_str()) {
                Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                _ => {
                    return report_failed("not_found", "missing or empty task_id");
                }
            };
            let continue_task = match req.input.get("task").and_then(|v| v.as_str()) {
                Some(s) if !s.trim().is_empty() => s.to_string(),
                _ => {
                    return report_failed("invalid_working_dir", "missing or empty task");
                }
            };
            let continue_req = crate::acp::delegation::types::ContinueDelegationRequest {
                parent_connection_id: req.parent_connection_id,
                parent_conversation_id,
                parent_tool_use_id,
                target_task_id,
                task: continue_task,
                work_unit_key,
                external_handle: req.external_handle,
                correlation_id,
                recovery_authorization_id,
            };
            return self.broker.continue_delegation(continue_req).await;
        }

        // 3. Parse the delegate_to_agent arguments. Schema validation lives
        //    on the LLM side; we only enforce what the broker can't.
        let agent_type = match req.input.get("agent_type").and_then(|v| v.as_str()) {
            Some(raw) => match parse_agent_type(raw) {
                Some(t) => t,
                None => return invalid_agent_type(raw),
            },
            None => return invalid_agent_type(""),
        };
        let task = match req.input.get("task").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => {
                return report_failed("invalid_working_dir", "missing or empty task");
            }
        };
        let profile_id = match req.input.get("profile_id") {
            None => None,
            Some(value) => match value.as_str().map(str::trim) {
                Some(id) if uuid::Uuid::parse_str(id).is_ok() => Some(id.to_string()),
                _ => {
                    return report_failed(
                        "invalid_delegation_profile",
                        "profile_id must be a valid UUID",
                    );
                }
            },
        };
        // The `working_dir` the LLM explicitly passed (before defaulting),
        // used by the broker's correlation key. `None` when omitted —
        // symmetric with the ACP `raw_input`, which also omits it then.
        let requested_working_dir = req
            .input
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let working_dir = requested_working_dir
            .clone()
            .or_else(|| Some(entry.working_dir.to_string_lossy().to_string()));

        let (replaces_task_id, replacement_reason) = match parse_replacement_inputs(&req.input) {
            Ok(pair) => pair,
            Err(message) => return report_failed("invalid_replacement", &message),
        };

        let delegation_req = DelegationRequest {
            parent_connection_id: req.parent_connection_id,
            parent_conversation_id,
            parent_tool_use_id,
            agent_type,
            profile_id,
            task,
            working_dir,
            requested_working_dir,
            external_handle: req.external_handle,
            work_unit_key,
            replaces_task_id,
            replacement_reason,
            correlation_id,
            recovery_authorization_id,
        };
        self.broker.start_delegation(delegation_req).await
    }
}

/// Parse optional `correlation_id` from tool input.
///
/// - absent → `Ok(None)` (caller may still bind via non-empty host tool id)
/// - present string that validates → `Ok(Some)`
/// - present null / empty / malformed / over-length / non-string → `Err`
///   (maps to wire `delegation_correlation_missing` when host id is empty)
pub(crate) fn parse_correlation_id(input: &Value) -> Result<Option<String>, String> {
    match input.get("correlation_id") {
        None => Ok(None),
        Some(Value::Null) => Err("correlation_id must not be null".into()),
        Some(Value::String(raw)) => match validate_correlation_id(raw) {
            Ok(()) => Ok(Some(raw.clone())),
            Err(message) => Err(message),
        },
        Some(_) => Err("correlation_id must be a string".into()),
    }
}

/// Parse the optional recovery receipt used by an exact continue or
/// replacement replay. The receipt is opaque and must remain nonblank; the
/// admission layer validates its subject, action, and one-time consumption.
pub(crate) fn parse_recovery_authorization_id(input: &Value) -> Result<Option<String>, String> {
    match input.get("recovery_authorization_id") {
        None => Ok(None),
        Some(Value::String(raw)) if !raw.trim().is_empty() => Ok(Some(raw.clone())),
        Some(Value::String(_)) => Err("recovery_authorization_id must not be blank".into()),
        Some(Value::Null) => Err("recovery_authorization_id must not be null".into()),
        Some(_) => Err("recovery_authorization_id must be a string".into()),
    }
}

/// Max Unicode scalars for optional `work_unit_key` (design / Skill contract).
const WORK_UNIT_KEY_MAX_CHARS: usize = 200;

/// Parse optional `work_unit_key` from `delegate_to_agent` tool input.
///
/// - absent / null / blank → `None` (ad-hoc one-shot)
/// - non-string → error
/// - trimmed length > 200 Unicode scalars → error
/// - otherwise → `Some(trimmed)`
pub(crate) fn parse_work_unit_key(input: &Value) -> Result<Option<String>, String> {
    match input.get("work_unit_key") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > WORK_UNIT_KEY_MAX_CHARS {
                return Err(format!(
                    "work_unit_key must be at most {WORK_UNIT_KEY_MAX_CHARS} characters"
                ));
            }
            Ok(Some(trimmed.to_string()))
        }
        Some(_) => Err("work_unit_key must be a string".into()),
    }
}

/// Parse optional replacement linkage. Both present → Ok(Some, Some);
/// both absent → Ok(None, None); one present without the other → Err.
pub(crate) fn parse_replacement_inputs(
    input: &Value,
) -> Result<(Option<String>, Option<String>), String> {
    let replaces = match input.get("replaces_task_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Some(_) => return Err("replaces_task_id must be a string".into()),
    };
    let reason = match input.get("replacement_reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                match t {
                    "unresumable"
                    | "budget_exhausted_continue"
                    | "not_supported"
                    | "admission_failed"
                    | "admission_unknown" => Some(t.to_string()),
                    other => {
                        return Err(format!("invalid replacement_reason: {other}"));
                    }
                }
            }
        }
        Some(_) => return Err("replacement_reason must be a string".into()),
    };
    match (replaces, reason) {
        (None, None) => Ok((None, None)),
        (Some(r), Some(reason)) => Ok((Some(r), Some(reason))),
        (Some(_), None) => Err("replacement_reason required with replaces_task_id".into()),
        (None, Some(_)) => Err("replaces_task_id required with replacement_reason".into()),
    }
}

/// Serialize a [`DelegationTaskReport`] into a [`BrokerResponse`] for the wire.
/// Used by the `Call` / `CancelTask` arms, which each resolve to one report.
/// Listener-side auth / wiring errors for workflow MCP tools.
enum WorkflowWireError {
    InvalidToken,
    FeatureDisabled,
    RootOnly,
    NoActiveConversation,
    StoreUnavailable,
    InvalidArguments(String),
    Internal(String),
}

impl WorkflowWireError {
    fn to_value(&self) -> Value {
        let (code, message) = match self {
            Self::InvalidToken => ("invalid_token", "invalid token".to_string()),
            Self::FeatureDisabled => (
                "feature_disabled",
                "workflow_v2 is not enabled for this companion".to_string(),
            ),
            Self::RootOnly => (
                "root_only",
                "workflow tools are Root-only; children cannot mutate or load workflow state"
                    .to_string(),
            ),
            Self::NoActiveConversation => (
                "no_active_conversation",
                "parent has no active conversation".to_string(),
            ),
            Self::StoreUnavailable => (
                "store_unavailable",
                "workflow store is not available on this process".to_string(),
            ),
            Self::InvalidArguments(msg) => ("invalid_arguments", msg.clone()),
            Self::Internal(msg) => ("internal", msg.clone()),
        };
        serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        })
    }
}

fn serialized_recovery_code<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn recovery_wire_error(code: &str, message: &str) -> Value {
    serde_json::json!({
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn recovery_authorization_error_value(error: RecoveryAuthorizationError) -> Value {
    recovery_wire_error(error.code(), &error.to_string())
}

fn recovery_authorization_result_value(
    result: &RecoveryAuthorizationResult,
    reused: bool,
) -> Value {
    let Some(action) = RecoveryAllowedAction::parse(&result.allowed_action) else {
        return recovery_wire_error(
            "recovery_authorization_contract_invalid",
            "persisted recovery action is invalid",
        );
    };
    let Some(metadata) = derive_recovery_action_metadata(action, &result.action_payload) else {
        return recovery_wire_error(
            "recovery_authorization_contract_invalid",
            "persisted recovery action payload is invalid",
        );
    };
    let mut value = serde_json::json!({
        "status": serialized_recovery_code(&result.status),
        "recovery_authorization_id": result.authorization_id,
        "reused": reused,
        "subject_kind": result.subject_kind,
        "subject_id": result.subject_id,
        "allowed_action": result.allowed_action,
        "cause_code": result.cause_code,
        "expires_at": result.expires_at,
    });
    let object = value
        .as_object_mut()
        .expect("recovery authorization result is an object");
    match action {
        RecoveryAllowedAction::Continue
        | RecoveryAllowedAction::FreshDispatch
        | RecoveryAllowedAction::Replace => {
            object.insert(
                "replacement_reason".into(),
                metadata
                    .replacement_reason
                    .map_or(Value::Null, |reason| Value::String(reason.into())),
            );
        }
        RecoveryAllowedAction::RecoverWorkflow => {
            object.insert(
                "target_state".into(),
                Value::String(
                    metadata
                        .target_state
                        .expect("workflow recovery metadata has target state")
                        .into(),
                ),
            );
        }
        RecoveryAllowedAction::ResetPlanLineage => {
            let Some(reason) = result.display_reason.as_ref() else {
                return recovery_wire_error(
                    "recovery_authorization_contract_invalid",
                    "Plan lineage reset authorization has no display reason",
                );
            };
            object.insert("display_reason".into(), Value::String(reason.clone()));
        }
    }
    value
}

fn recovery_authorization_row_value(row: &recovery_authorization::Model, reused: bool) -> Value {
    let result = RecoveryAuthorizationResult::from(row);
    recovery_authorization_result_value(&result, reused)
}

fn parse_gate_settlement_outcome(raw: &str) -> Result<GateSettlementOutcome, String> {
    match raw {
        "approved" => Ok(GateSettlementOutcome::Approved),
        "changes_requested" => Ok(GateSettlementOutcome::ChangesRequested),
        "blocked" => Ok(GateSettlementOutcome::Blocked),
        other => Err(format!(
            "outcome must be approved|changes_requested|blocked, got {other}"
        )),
    }
}

fn workflow_store_error_value(err: WorkflowStoreError) -> Value {
    let code = match &err {
        WorkflowStoreError::Validation(WorkflowError::RiskAssessmentInvalid(_)) => {
            "risk_assessment_invalid"
        }
        WorkflowStoreError::Validation(WorkflowError::TaskRouteMismatch(_)) => {
            "task_route_mismatch"
        }
        WorkflowStoreError::Validation(_) => "validation",
        WorkflowStoreError::PlanReview(PlanReviewError::RequiredReviewerSetMismatch { .. }) => {
            "reviewer_set_mismatch"
        }
        WorkflowStoreError::PlanReview(_) => "plan_review",
        WorkflowStoreError::NotFound(_) => "not_found",
        WorkflowStoreError::CrossParent { .. } => "cross_parent",
        WorkflowStoreError::StaleManifestRevision { .. } => "stale_manifest_revision",
        WorkflowStoreError::StaleGraphRevision { .. } => "stale_graph_revision",
        WorkflowStoreError::PublicationTokenMismatch { .. } => "publication_token_mismatch",
        WorkflowStoreError::PublicationTokenConflict { .. } => "publication_token_conflict",
        WorkflowStoreError::AdmittedNodeIdentityMutation { .. } => {
            "admitted_node_identity_mutation"
        }
        WorkflowStoreError::CohortFrozen { .. } => "cohort_frozen",
        WorkflowStoreError::ReviewedTaskStale(_) => "reviewed_task_stale",
        WorkflowStoreError::ArtifactDigestMismatch(_) => "artifact_digest_mismatch",
        WorkflowStoreError::GateNotReady(_) => "gate_not_ready",
        WorkflowStoreError::V2CallerEvidenceRejected => "invalid_arguments",
        WorkflowStoreError::CompletionDecisionRequired => "completion_decision_required",
        WorkflowStoreError::CompletionDecisionSuperseded => "completion_decision_superseded",
        WorkflowStoreError::CompletionArtifactUnavailable => "completion_artifact_unavailable",
        WorkflowStoreError::GateCycleConflict(_) => "gate_cycle_conflict",
        WorkflowStoreError::ExecutionGateSettleRejected(_) => "execution_gate_settle_rejected",
        WorkflowStoreError::ApprovalWithOpenFindings { .. } => "approval_with_open_findings",
        WorkflowStoreError::ApprovalRejectedFailedReviewer { .. } => {
            "approval_rejected_failed_reviewer"
        }
        WorkflowStoreError::SummaryTooLarge => "summary_too_large",
        WorkflowStoreError::NegativeFindingCounts { .. } => "negative_finding_counts",
        WorkflowStoreError::ParentNotFound(_) => "parent_not_found",
        WorkflowStoreError::LegacyCompletionProtocolRestartRequired(_) => {
            "legacy_completion_protocol_restart_required"
        }
        WorkflowStoreError::LegacyCompletionProtocolRestartInvalid(_) => {
            "legacy_completion_protocol_restart_invalid"
        }
        WorkflowStoreError::Busy(_) => "busy",
        WorkflowStoreError::WorkflowRecoveryNotAvailable => "workflow_recovery_not_available",
        WorkflowStoreError::WorkflowRecoveryConflict => "workflow_recovery_conflict",
        WorkflowStoreError::RecoveryAuthorizationRequired { .. } => {
            "recovery_authorization_required"
        }
        WorkflowStoreError::RecoveryAuthorizationStale => "recovery_authorization_stale",
        WorkflowStoreError::RecoveryAuthorizationRejected { code } => code,
        WorkflowStoreError::Persistence(_) => "persistence",
    };
    serde_json::json!({
        "error": {
            "code": code,
            "message": err.to_string(),
        }
    })
}

fn completion_work_error_value(code: &str, message: &str) -> Value {
    serde_json::json!({
        "error": {
            "code": code,
            "message": message,
        }
    })
}

#[cfg(test)]
pub(crate) fn workflow_store_error_code_for_test(err: WorkflowStoreError) -> String {
    workflow_store_error_value(err)["error"]["code"]
        .as_str()
        .expect("workflow errors have string codes")
        .to_string()
}

impl DelegationListener {
    async fn report_response(
        &self,
        report: DelegationTaskReport,
    ) -> std::io::Result<BrokerResponse> {
        let mut outcome = serde_json::to_value(&report).map_err(invalid_json)?;
        self.enrich_completion(&mut outcome, report.task_id.as_deref())
            .await;
        Ok(BrokerResponse { outcome })
    }

    async fn status_response(
        &self,
        batch: DelegationStatusBatch,
    ) -> std::io::Result<BrokerResponse> {
        let task_ids = batch
            .tasks
            .iter()
            .map(|report| report.task_id.clone())
            .collect::<Vec<_>>();
        let mut outcome = serde_json::to_value(batch).map_err(invalid_json)?;
        if let Some(tasks) = outcome.get_mut("tasks").and_then(Value::as_array_mut) {
            for (task, task_id) in tasks.iter_mut().zip(task_ids.iter()) {
                self.enrich_completion(task, task_id.as_deref()).await;
            }
        }
        Ok(BrokerResponse { outcome })
    }

    async fn enrich_completion(&self, value: &mut Value, task_id: Option<&str>) {
        let (Some(task_id), Some(runs)) = (task_id, self.broker.run_store()) else {
            return;
        };
        let final_guard =
            guard_task_final_delivery_core(runs.db(), &self.workflow_emitter, task_id).await;
        match final_guard {
            Ok(Some(FinalDeliveryGuardResult::Reopened { diagnostic, .. }))
            | Ok(Some(FinalDeliveryGuardResult::Rejected(diagnostic))) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("status".into(), Value::String("failed".into()));
                    object.insert("error_code".into(), Value::String(diagnostic.code().into()));
                    object.insert("message".into(), Value::String(diagnostic.to_string()));
                    object.remove("text");
                    object.remove("completion");
                }
                return;
            }
            Ok(Some(FinalDeliveryGuardResult::Ready(_))) | Ok(None) | Err(_) => {}
        }
        match crate::acp::delegation::workflow::load_completion_projection(&runs.db().conn, task_id)
            .await
        {
            Ok(Some(completion)) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "completion".into(),
                        serde_json::to_value(completion).expect("completion projection serializes"),
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("status".into(), Value::String("failed".into()));
                    object.insert("error_code".into(), Value::String(error.code().to_string()));
                    object.insert("message".into(), Value::String(error.to_string()));
                    object.remove("text");
                    object.remove("completion");
                }
            }
        }
    }
}

fn invalid_json(error: serde_json::Error) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("encode broker response: {error}"),
    )
}

/// Serialize an arbitrary serde value as the broker outcome envelope.
fn value_response<T: serde::Serialize>(outcome: &T) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

fn legacy_wait_from(wait_ms: Option<u64>) -> StatusWait {
    match wait_ms {
        None => StatusWait::Snapshot,
        Some(0) => StatusWait::Terminal,
        Some(ms) => {
            StatusWait::Supervised(std::time::Duration::from_millis(ms.min(STATUS_WAIT_MAX_MS)))
        }
    }
}

/// Which indefinite status path is arming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndefiniteWaitKind {
    LegacyTerminal,
    CompatJoin,
    ContinuationJoin,
}

/// Authoritative wait tool id: request-carried `_meta` first, else identity-less
/// rewrite id. Never invents or scans `active_tool_calls`.
///
/// Nonblank ids keep original host/rewrite bytes (opaque identity for bind and
/// lease-key renewal). Trim is used only to reject blank / whitespace-only.
fn resolve_wait_tool_id(
    req: &BrokerStatusRequest,
    rewritten_status_tool_id: Option<&str>,
) -> Option<String> {
    if !req.parent_tool_use_id.trim().is_empty() {
        return Some(req.parent_tool_use_id.clone());
    }
    rewritten_status_tool_id
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

/// Structured debug reason for wait arm / bind failures (closed label set).
fn emit_wait_arm_reason(reason: &'static str) {
    tracing::debug!(reason, "[delegation] wait arm correlation note");
}

/// Serialize a [`DelegationStatusBatch`] for the `Status` arm. Legacy batches
/// omit Join fields; Join batches include `wake_reason` and
/// `attention_requests`.
/// Serialize the pending feedback notes into a
/// `{ "count": N, "feedback": [..], "_commit_ids": [..] }` envelope for the
/// `Feedback` arm. Only the lean `text` + `created_at` reach the agent; the
/// `_commit_ids` are internal — the companion echoes them back in a
/// `CommitFeedback` once it delivers the result, and `render_feedback_result`
/// strips them from the agent-facing output. `count == 0` is "no new feedback".
fn feedback_response(items: &[PendingFeedback]) -> std::io::Result<BrokerResponse> {
    let notes: Vec<Value> = items
        .iter()
        .map(|p| serde_json::json!({ "text": p.text, "created_at": p.created_at }))
        .collect();
    let ids: Vec<&str> = items.iter().map(|p| p.id.as_str()).collect();
    Ok(BrokerResponse {
        outcome: serde_json::json!({
            "count": notes.len(),
            "feedback": notes,
            "_commit_ids": ids,
        }),
    })
}

/// Serialize a resolved [`QuestionOutcome`] into a [`BrokerResponse`] for the
/// `Ask` arm — the `{ answers, declined }` envelope the companion renders.
fn ask_response(outcome: &QuestionOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a resolved [`SessionInfo`] into a [`BrokerResponse`] for the
/// `SessionInfo` arm — the companion renders it into the `get_session_info`
/// tool result.
fn session_response(info: SessionInfo) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&info).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// The `declined` outcome — used when the token is invalid, the connection is
/// gone, or the answer one-shot was dropped without a response. The LLM reads it
/// as "the user didn't answer; proceed with your own judgment".
fn ask_declined_response() -> std::io::Result<BrokerResponse> {
    ask_response(&QuestionOutcome {
        answers: Vec::new(),
        declined: true,
    })
}

/// A `Canceled` report for a setup-side rejection the LLM can't react to (bad
/// token, parent gone). Mirrors the old `cancel(..)` DelegationOutcome.
fn report_canceled(message: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: None,
        continued_from_task_id: None,
        reused_session: None,
        status: TaskStatus::Canceled,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: Some("canceled".into()),
        message: Some(message.into()),
        duration_ms: None,
        observation: None,
        last_agent_activity_at: None,
        stalled_since: None,
        recovery: None,
    }
}

/// A `Failed` report carrying a wire-stable `error_code` for a bad argument.
fn report_failed(error_code: &str, message: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: None,
        continued_from_task_id: None,
        reused_session: None,
        status: TaskStatus::Failed,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: Some(error_code.into()),
        message: Some(message.into()),
        duration_ms: None,
        observation: None,
        last_agent_activity_at: None,
        stalled_since: None,
        recovery: None,
    }
}

/// An `Unknown` report — used when a status/cancel request fails the token
/// check (we don't leak whether the task exists).
fn unknown_report(task_id: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        continued_from_task_id: None,
        reused_session: None,
        status: TaskStatus::Unknown,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: None,
        message: Some("unknown task id".into()),
        duration_ms: None,
        observation: None,
        last_agent_activity_at: None,
        stalled_since: None,
        recovery: None,
    }
}

/// Host wait-cancel settlement for a multi-task Join. Does **not** cancel
/// child tasks; only the wait request is terminated. Error code follows
/// [`CancelCause`] so UserStop emits `user_cancelled`.
fn wait_cancel_report(
    task_id: &str,
    cause: crate::acp::tool_watchdog::CancelCause,
) -> DelegationTaskReport {
    let error_code = crate::acp::tool_watchdog::error_code_for_cause(cause);
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        continued_from_task_id: None,
        reused_session: None,
        status: TaskStatus::Canceled,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: Some(error_code.to_string()),
        message: Some("wait cancelled by host tool watchdog".into()),
        duration_ms: None,
        observation: None,
        last_agent_activity_at: None,
        stalled_since: None,
        recovery: None,
    }
}

fn timeout_cancel_guidance_report(task_id: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        continued_from_task_id: None,
        reused_session: None,
        status: TaskStatus::Running,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: None,
        message: Some(crate::acp::delegation::types::TIMEOUT_CANCEL_GUIDANCE.into()),
        duration_ms: None,
        observation: None,
        last_agent_activity_at: None,
        stalled_since: None,
        recovery: None,
    }
}

fn cancel(message: &str) -> DelegationTaskReport {
    report_canceled(message)
}

fn invalid_agent_type(raw: &str) -> DelegationTaskReport {
    if raw.is_empty() {
        report_failed("invalid_agent_type", "missing agent_type")
    } else {
        report_failed("invalid_agent_type", &format!("invalid agent_type: {raw}"))
    }
}

fn parse_agent_type(raw: &str) -> Option<AgentType> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

/// Default socket path for the running process, scoped to PID so multiple
/// codeg instances on the same machine don't collide.
///
/// Unix: a `.sock` file inside `temp_dir`.
/// Windows: a named pipe address `\\.\pipe\codeg-delegation-<pid>`. Windows
/// named pipes live in their own kernel namespace and ignore `temp_dir`; the
/// argument is kept for signature parity across platforms.
#[cfg(unix)]
pub fn default_socket_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join(format!("codeg-delegation-{}.sock", std::process::id()))
}

#[cfg(windows)]
pub fn default_socket_path(_temp_dir: &Path) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\codeg-delegation-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::connection::SuspensionAck;
    use crate::acp::delegation::attention::{
        mock::MemoryDelegationAttentionStore, AttentionOpenResult, AttentionRecord,
        AttentionRequestSummary, AttentionResolutionCode, AttentionResolveResult,
        AttentionStoreError, DelegationAttentionStore, NewAttentionRequest,
    };
    use crate::acp::delegation::broker::{
        ConversationDepthLookup, DelegationConfig, DelegationMatchKey,
    };
    use crate::acp::delegation::continuation::coordinator::{
        ContinuationError, ContinuationPromptRequest, DelegationContinuationCoordinator,
        ParentContinuationPort, ParentTurnSnapshot, PromptAdmissionResult, SuspendRequest,
        SystemContinuationClock,
    };
    use crate::acp::delegation::continuation::store::{
        ContStoreError, ContinuationPatch, ContinuationRecord, ContinuationStore,
        InMemoryContinuationStore, NewContinuation,
    };
    use crate::acp::delegation::continuation::types::{
        ContinuationFailureCode, ContinuationState, ContinuationWaitingProjection,
        ContinuationWakeReason,
    };
    use crate::acp::delegation::spawner::{
        accepted, mock::MockSpawner, ConnectionSpawner, SpawnerError,
    };
    use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationSuccess};
    use crate::acp::delegation::workflow::publish_workflow_manifest_core;
    use crate::acp::tool_watchdog::{CancelCause, WaitCancelResult, WaitStamp};
    use chrono::Utc;
    use serde_json::json;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::io::{duplex, AsyncRead, AsyncWrite, ReadBuf};

    struct AlwaysRootLookup;
    #[async_trait]
    impl ConversationDepthLookup for AlwaysRootLookup {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    struct StaticParentLookup(Option<i32>);
    #[async_trait]
    impl ParentSessionLookup for StaticParentLookup {
        async fn current_conversation_id(&self, _parent_connection_id: &str) -> Option<i32> {
            self.0
        }
    }

    /// Gates `bind_delegation_wait` so tests can peer-close after register but
    /// before bind returns (WaitCancelGuard must already be armed).
    struct BindGatedParentLookup {
        conversation_id: Option<i32>,
        entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl BindGatedParentLookup {
        fn new(
            conversation_id: Option<i32>,
        ) -> (
            Arc<Self>,
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Sender<()>,
        ) {
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            (
                Arc::new(Self {
                    conversation_id,
                    entered: std::sync::Mutex::new(Some(entered_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                }),
                entered_rx,
                release_tx,
            )
        }
    }

    #[async_trait]
    impl ParentSessionLookup for BindGatedParentLookup {
        async fn current_conversation_id(&self, _parent_connection_id: &str) -> Option<i32> {
            self.conversation_id
        }

        async fn bind_delegation_wait(
            &self,
            _parent_connection_id: &str,
            expected: &crate::acp::tool_watchdog::WaitStamp,
        ) -> crate::acp::tool_watchdog::BindDelegationWaitResult {
            use crate::acp::tool_watchdog::BindDelegationWaitResult;
            if let Some(tx) = self
                .entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
            }
            match expected
                .parent_tool_use_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                None => BindDelegationWaitResult::WaitToolIdMissing,
                Some(_) => BindDelegationWaitResult::Bound,
            }
        }
    }

    /// In-memory feedback stub. `read_pending_feedback` returns the seeded notes
    /// WITHOUT draining (read-only, matching production), recording the conn id;
    /// `commit_feedback_delivered` records the (conn_id, ids) it was committed
    /// with so tests can assert delivery happens only after a successful write.
    /// Default is empty (the delegation tests don't exercise feedback).
    #[derive(Default)]
    struct StubFeedback {
        items: tokio::sync::Mutex<Vec<PendingFeedback>>,
        read_conn: tokio::sync::Mutex<Option<String>>,
        committed: tokio::sync::Mutex<Vec<(String, Vec<String>)>>,
    }
    #[async_trait]
    impl SessionFeedbackAccess for StubFeedback {
        async fn read_pending_feedback(&self, parent_connection_id: &str) -> Vec<PendingFeedback> {
            *self.read_conn.lock().await = Some(parent_connection_id.to_string());
            self.items.lock().await.clone()
        }
        async fn commit_feedback_delivered(&self, parent_connection_id: &str, ids: Vec<String>) {
            self.committed
                .lock()
                .await
                .push((parent_connection_id.to_string(), ids));
        }
    }

    /// In-memory question stub. `register_question` mints a sequential id,
    /// stashes the answer sender (so a test can resolve it via `answer`), and
    /// records the (parent_conn, questions); `cancel_question` removes the
    /// sender and records the canceled id. Lets the listener's `Ask` arm be
    /// driven without a real `ConnectionManager`.
    #[derive(Default)]
    struct StubQuestion {
        pending: tokio::sync::Mutex<HashMap<String, oneshot::Sender<QuestionOutcome>>>,
        registered: tokio::sync::Mutex<Vec<(String, Vec<crate::acp::question::QuestionSpec>)>>,
        canceled: tokio::sync::Mutex<Vec<String>>,
    }
    #[async_trait]
    impl SessionQuestionAccess for StubQuestion {
        async fn register_question(
            &self,
            parent_connection_id: &str,
            questions: Vec<crate::acp::question::QuestionSpec>,
        ) -> Option<crate::acp::question::RegisteredQuestion> {
            let question_id = format!("q-{}", self.registered.lock().await.len() + 1);
            let (tx, rx) = oneshot::channel();
            self.pending.lock().await.insert(question_id.clone(), tx);
            self.registered
                .lock()
                .await
                .push((parent_connection_id.to_string(), questions));
            Some(crate::acp::question::RegisteredQuestion {
                question_id,
                answer_rx: rx,
            })
        }
        async fn cancel_question(&self, _parent_connection_id: &str, question_id: &str) {
            self.pending.lock().await.remove(question_id);
            self.canceled.lock().await.push(question_id.to_string());
        }
        async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {
            // Not exercised by the listener unit tests (the teardown sweep lives
            // in connection.rs); drop all parked senders to satisfy the trait.
            self.pending.lock().await.clear();
        }
    }
    impl StubQuestion {
        async fn answer(&self, question_id: &str, outcome: QuestionOutcome) {
            if let Some(tx) = self.pending.lock().await.remove(question_id) {
                let _ = tx.send(outcome);
            }
        }
    }

    /// In-memory session-info stub. Records every `(session_id, max_messages)` it
    /// was asked to resolve and returns a seeded outcome — `found` sessions echo
    /// their id, unknown ids return `not_found`. Default knows about no sessions.
    #[derive(Default)]
    struct StubSessionInfo {
        known: std::collections::HashSet<i32>,
        calls: tokio::sync::Mutex<Vec<(i32, u32)>>,
    }
    #[async_trait]
    impl SessionInfoAccess for StubSessionInfo {
        async fn resolve(&self, session_id: i32, max_messages: u32) -> SessionInfo {
            self.calls.lock().await.push((session_id, max_messages));
            if self.known.contains(&session_id) {
                SessionInfo {
                    found: true,
                    session_id,
                    title: Some(format!("session {session_id}")),
                    ..Default::default()
                }
            } else {
                SessionInfo::not_found(session_id)
            }
        }
    }

    use tokio::sync::oneshot;

    async fn make_broker(mock: Arc<MockSpawner>) -> Arc<DelegationBroker> {
        let broker = Arc::new(DelegationBroker::new(
            mock as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        // Production default is `enabled: false`; listener tests that don't
        // explicitly set their own config need the switch flipped on so
        // `handle_request` parks pending entries instead of returning
        // `Canceled { reason: "delegation disabled" }` straight away.
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        broker
    }

    async fn make_broker_with_attention(
        mock: Arc<MockSpawner>,
        attention: Arc<dyn DelegationAttentionStore>,
    ) -> Arc<DelegationBroker> {
        let broker = Arc::new(
            DelegationBroker::new(
                mock as Arc<dyn ConnectionSpawner>,
                Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_attention_store(attention),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        broker
    }

    struct JoinEntryAttentionStore {
        inner: MemoryDelegationAttentionStore,
        list_calls: tokio::sync::mpsc::UnboundedSender<()>,
    }

    impl JoinEntryAttentionStore {
        fn new() -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<()>) {
            let (list_calls, receiver) = tokio::sync::mpsc::unbounded_channel();
            (
                Arc::new(Self {
                    inner: MemoryDelegationAttentionStore::new(),
                    list_calls,
                }),
                receiver,
            )
        }
    }

    #[async_trait]
    impl DelegationAttentionStore for JoinEntryAttentionStore {
        async fn open_or_recover(
            &self,
            request: NewAttentionRequest,
        ) -> Result<AttentionOpenResult, AttentionStoreError> {
            self.inner.open_or_recover(request).await
        }

        async fn list_open_for_tasks(
            &self,
            parent_conversation_id: i32,
            task_ids: &[String],
        ) -> Result<Vec<AttentionRequestSummary>, AttentionStoreError> {
            let _ = self.list_calls.send(());
            self.inner
                .list_open_for_tasks(parent_conversation_id, task_ids)
                .await
        }

        async fn wait_snapshot(
            &self,
            request_id: &str,
        ) -> Result<AttentionRecord, AttentionStoreError> {
            self.inner.wait_snapshot(request_id).await
        }

        async fn reply(
            &self,
            parent_conversation_id: i32,
            request_id: &str,
            reply: &str,
            at: chrono::DateTime<Utc>,
        ) -> Result<AttentionResolveResult, AttentionStoreError> {
            self.inner
                .reply(parent_conversation_id, request_id, reply, at)
                .await
        }

        async fn resolve_task(
            &self,
            task_id: &str,
            code: AttentionResolutionCode,
            at: chrono::DateTime<Utc>,
        ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
            self.inner.resolve_task(task_id, code, at).await
        }

        async fn reconcile_open(
            &self,
            at: chrono::DateTime<Utc>,
        ) -> Result<Vec<AttentionRecord>, AttentionStoreError> {
            self.inner.reconcile_open(at).await
        }
    }

    struct ContinuationTestPort {
        snapshot_calls: AtomicUsize,
        snapshot_entered: std::sync::Mutex<Option<oneshot::Sender<()>>>,
        snapshot_release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        suspend_entered: std::sync::Mutex<Option<oneshot::Sender<SuspendRequest>>>,
        suspend_release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        admissions: std::sync::Mutex<Vec<(u64, ContinuationWakeReason)>>,
        fail_snapshot: bool,
    }

    impl ContinuationTestPort {
        fn ready() -> Arc<Self> {
            Arc::new(Self {
                snapshot_calls: AtomicUsize::new(0),
                snapshot_entered: std::sync::Mutex::new(None),
                snapshot_release: tokio::sync::Mutex::new(None),
                suspend_entered: std::sync::Mutex::new(None),
                suspend_release: tokio::sync::Mutex::new(None),
                admissions: std::sync::Mutex::new(Vec::new()),
                fail_snapshot: false,
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                fail_snapshot: true,
                ..Self::ready_value()
            })
        }

        fn snapshot_gated() -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    snapshot_entered: std::sync::Mutex::new(Some(entered_tx)),
                    snapshot_release: tokio::sync::Mutex::new(Some(release_rx)),
                    ..Self::ready_value()
                }),
                entered_rx,
                release_tx,
            )
        }

        fn suspend_gated() -> (
            Arc<Self>,
            oneshot::Receiver<SuspendRequest>,
            oneshot::Sender<()>,
        ) {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            (
                Arc::new(Self {
                    suspend_entered: std::sync::Mutex::new(Some(entered_tx)),
                    suspend_release: tokio::sync::Mutex::new(Some(release_rx)),
                    ..Self::ready_value()
                }),
                entered_rx,
                release_tx,
            )
        }

        fn ready_value() -> Self {
            Self {
                snapshot_calls: AtomicUsize::new(0),
                snapshot_entered: std::sync::Mutex::new(None),
                snapshot_release: tokio::sync::Mutex::new(None),
                suspend_entered: std::sync::Mutex::new(None),
                suspend_release: tokio::sync::Mutex::new(None),
                admissions: std::sync::Mutex::new(Vec::new()),
                fail_snapshot: false,
            }
        }
    }

    #[async_trait]
    impl ParentContinuationPort for ContinuationTestPort {
        async fn snapshot_parent(
            &self,
            connection_id: &str,
        ) -> Result<ParentTurnSnapshot, ContinuationError> {
            self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = self
                .snapshot_entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = entered.send(());
            }
            if let Some(release) = self.snapshot_release.lock().await.take() {
                let _ = release.await;
            }
            if self.fail_snapshot {
                return Err(ContinuationError::ParentUnavailable);
            }
            Ok(ParentTurnSnapshot {
                connection_id: connection_id.to_string(),
                conversation_id: 1,
                session_id: "session-1".into(),
                turn_generation: 1,
                turn_in_flight: true,
            })
        }

        async fn suspend_parent(
            &self,
            request: SuspendRequest,
        ) -> Result<SuspensionAck, ContinuationError> {
            if let Some(entered) = self
                .suspend_entered
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = entered.send(SuspendRequest {
                    continuation_id: request.continuation_id.clone(),
                    parent_connection_id: request.parent_connection_id.clone(),
                    parent_conversation_id: request.parent_conversation_id,
                    parent_session_id: request.parent_session_id.clone(),
                    parent_turn_generation: request.parent_turn_generation,
                });
            }
            if let Some(release) = self.suspend_release.lock().await.take() {
                let _ = release.await;
            }
            Ok(SuspensionAck {
                continuation_id: request.continuation_id,
                parent_turn_generation: request.parent_turn_generation,
            })
        }

        async fn admit_continuation(
            &self,
            request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
            self.admissions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((
                    request.continuation_generation,
                    request.origin.wake_reason(),
                ));
            Ok(PromptAdmissionResult::Admitted)
        }

        async fn publish_waiting(
            &self,
            _connection_id: &str,
            _waiting: Option<ContinuationWaitingProjection>,
        ) -> Result<(), ContinuationError> {
            Ok(())
        }

        async fn publish_failure(
            &self,
            _connection_id: &str,
            _code: ContinuationFailureCode,
        ) -> Result<(), ContinuationError> {
            Ok(())
        }
    }

    struct ReleaseObservedStore {
        inner: InMemoryContinuationStore,
        ownership_wins: AtomicUsize,
    }

    impl ReleaseObservedStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                inner: InMemoryContinuationStore::default(),
                ownership_wins: AtomicUsize::new(0),
            })
        }

        fn ownership_wins(&self) -> usize {
            self.ownership_wins.load(Ordering::SeqCst)
        }

        fn record_selected_transition(
            &self,
            expected: ContinuationState,
            target: ContinuationState,
            won: bool,
        ) {
            if won
                && expected != target
                && matches!(
                    target,
                    ContinuationState::WakePending
                        | ContinuationState::Cancelled
                        | ContinuationState::Failed
                )
            {
                self.ownership_wins.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[async_trait]
    impl ContinuationStore for ReleaseObservedStore {
        async fn insert_arming(
            &self,
            new: NewContinuation,
        ) -> Result<ContinuationRecord, ContStoreError> {
            self.inner.insert_arming(new).await
        }

        async fn load(
            &self,
            continuation_id: &str,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            self.inner.load(continuation_id).await
        }

        async fn load_active_for_conversation(
            &self,
            conversation_id: i32,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            self.inner
                .load_active_for_conversation(conversation_id)
                .await
        }

        async fn list_non_terminal(&self) -> Result<Vec<ContinuationRecord>, ContStoreError> {
            self.inner.list_non_terminal().await
        }

        async fn cas_transition(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: ContinuationState,
            patch: ContinuationPatch,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            let target = patch.state;
            let result = self
                .inner
                .cas_transition(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    patch,
                )
                .await?;
            self.record_selected_transition(expected_state, target, result.is_some());
            Ok(result)
        }

        async fn cas_fail_and_cancel_parent(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: ContinuationState,
            failure_code: ContinuationFailureCode,
            finished_at: chrono::DateTime<Utc>,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            let result = self
                .inner
                .cas_fail_and_cancel_parent(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    failure_code,
                    finished_at,
                )
                .await?;
            self.record_selected_transition(
                expected_state,
                ContinuationState::Failed,
                result.is_some(),
            );
            Ok(result)
        }

        async fn cas_claim_cleanup(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: ContinuationState,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            self.inner
                .cas_claim_cleanup(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                )
                .await
        }

        async fn matches_admitted_marker(
            &self,
            conversation_id: i32,
            marker: &str,
        ) -> Result<bool, ContStoreError> {
            self.inner
                .matches_admitted_marker(conversation_id, marker)
                .await
        }

        async fn load_latest_failure_for_conversation(
            &self,
            conversation_id: i32,
        ) -> Result<Option<ContinuationRecord>, ContStoreError> {
            self.inner
                .load_latest_failure_for_conversation(conversation_id)
                .await
        }
    }

    fn continuation_registry_with_store(
        broker: Arc<DelegationBroker>,
        store: Arc<dyn ContinuationStore>,
        port: Arc<ContinuationTestPort>,
    ) -> (Arc<TokenRegistry>, Arc<DelegationContinuationCoordinator>) {
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store,
            broker,
            Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
            port,
            Arc::new(SystemContinuationClock::new()),
        ));
        let tokens = Arc::new(TokenRegistry::with_continuation_coordinator(
            coordinator.clone(),
        ));
        (tokens, coordinator)
    }

    fn continuation_registry(
        broker: Arc<DelegationBroker>,
        store: Arc<InMemoryContinuationStore>,
        port: Arc<ContinuationTestPort>,
    ) -> (Arc<TokenRegistry>, Arc<DelegationContinuationCoordinator>) {
        continuation_registry_with_store(broker, store as Arc<dyn ContinuationStore>, port)
    }

    fn continuation_token_entry(enabled: bool) -> TokenEntry {
        TokenEntry {
            parent_connection_id: "parent-conn".into(),
            working_dir: PathBuf::from("/tmp"),
            coordination_v1: true,
            delegation_continuation_v1: enabled,
            role: CompanionRole::Root,
            workflow_v2: false,
            completion_v2: false,
            bound_task_id: None,
        }
    }

    struct ContinuationReleaseHarness {
        broker: Arc<DelegationBroker>,
        spawner: Arc<MockSpawner>,
        store: Arc<ReleaseObservedStore>,
        coordinator: Arc<DelegationContinuationCoordinator>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
        task_id: String,
        stamp: WaitStamp,
        status_task:
            Option<tokio::task::JoinHandle<Result<DelegationStatusBatch, ContinuationError>>>,
        response_count: Arc<AtomicUsize>,
        suspend_release: Option<oneshot::Sender<()>>,
        arm_suspended_ready: Option<oneshot::Receiver<()>>,
        allow_decision: Option<oneshot::Sender<()>>,
        snapshot_before: Option<oneshot::Receiver<()>>,
        snapshot_allow_read: Option<oneshot::Sender<()>>,
        snapshot_after: Option<oneshot::Receiver<()>>,
        snapshot_allow_assemble: Option<oneshot::Sender<()>>,
    }

    impl ContinuationReleaseHarness {
        async fn new(label: &str, gate_snapshot: bool) -> Self {
            let spawner = Arc::new(MockSpawner::new());
            let broker = make_broker(spawner.clone()).await;
            let task_id = broker.seed_live_task_for_test("parent-conn", label).await;
            let store = ReleaseObservedStore::new();
            let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
            let (tokens, coordinator) = continuation_registry_with_store(
                broker.clone(),
                store.clone() as Arc<dyn ContinuationStore>,
                port,
            );
            tokens
                .register("tok".into(), continuation_token_entry(true))
                .await;
            let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
            let listener = make_listener_with_wait_cancel(
                broker.clone(),
                tokens,
                Some(1),
                wait_cancel.clone(),
            );
            let decision = listener.install_status_release_decision_gate().await;
            let snapshot = if gate_snapshot {
                Some(broker.install_status_snapshot_gate().await)
            } else {
                None
            };
            let response_count = Arc::new(AtomicUsize::new(0));
            let parent_tool_use_id = format!("wait-{label}");
            let status_task = tokio::spawn({
                let listener = listener.clone();
                let task_id = task_id.clone();
                let response_count = response_count.clone();
                async move {
                    let result = listener
                        .process_status(BrokerStatusRequest {
                            token: "tok".into(),
                            task_ids: vec![task_id],
                            wait_ms: Some(0),
                            return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                            parent_tool_use_id,
                        })
                        .await
                        .map(|processed| processed.batch);
                    if result.is_ok() {
                        response_count.fetch_add(1, Ordering::SeqCst);
                    }
                    result
                }
            });
            decision
                .before_select_entered
                .await
                .expect("status reaches the gated release decision");
            suspend_entered
                .await
                .expect("durable arm reaches the suspension port");
            let stamp = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(stamp) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                        break stamp;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("exact wait stamp is registered");
            let (snapshot_before, snapshot_allow_read, snapshot_after, snapshot_allow_assemble) =
                match snapshot {
                    Some(gate) => (
                        Some(gate.before_read_entered),
                        Some(gate.allow_read),
                        Some(gate.after_classification_entered),
                        Some(gate.allow_assemble),
                    ),
                    None => (None, None, None, None),
                };
            Self {
                broker,
                spawner,
                store,
                coordinator,
                wait_cancel,
                task_id,
                stamp,
                status_task: Some(status_task),
                response_count,
                suspend_release: Some(suspend_release),
                arm_suspended_ready: Some(decision.arm_suspended_ready),
                allow_decision: Some(decision.allow_select),
                snapshot_before,
                snapshot_allow_read,
                snapshot_after,
                snapshot_allow_assemble,
            }
        }

        fn release_suspension(&mut self) {
            self.suspend_release.take().unwrap().send(()).unwrap();
        }

        async fn await_arm_suspended(&mut self) {
            self.arm_suspended_ready
                .take()
                .unwrap()
                .await
                .expect("ArmStatus::Suspended is ready");
        }

        fn release_decision(&mut self) {
            self.allow_decision.take().unwrap().send(()).unwrap();
        }

        async fn cancel_exact(&self) {
            assert_eq!(
                self.wait_cancel
                    .cancel(&self.stamp, CancelCause::AutoTimeout)
                    .await,
                WaitCancelResult::Cancelled
            );
        }

        async fn await_snapshot_before(&mut self) {
            self.snapshot_before.take().unwrap().await.unwrap();
        }

        fn allow_snapshot_read(&mut self) {
            self.snapshot_allow_read.take().unwrap().send(()).unwrap();
        }

        async fn await_snapshot_after(&mut self) {
            self.snapshot_after.take().unwrap().await.unwrap();
        }

        fn allow_snapshot_assemble(&mut self) {
            self.snapshot_allow_assemble
                .take()
                .unwrap()
                .send(())
                .unwrap();
        }

        async fn finish_status(&mut self) -> DelegationStatusBatch {
            tokio::time::timeout(Duration::from_secs(2), self.status_task.take().unwrap())
                .await
                .expect("one foreground response")
                .expect("status task joins")
                .expect("status result")
        }

        async fn await_waiting(&self) -> ContinuationRecord {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(row) = self.store.load_active_for_conversation(1).await.unwrap() {
                        if row.state == ContinuationState::Waiting {
                            break row;
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("released suspension commits the durable Waiting owner")
        }

        async fn assert_common(&self, expected_child_cancels: usize) {
            tokio::time::timeout(Duration::from_secs(2), async {
                while self.wait_cancel.contains(&self.stamp.wait_id).await
                    || self.store.ownership_wins() != 1
                    || self.spawner.cancels.lock().await.len() != expected_child_cancels
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("release invariants settle deterministically");
            assert_eq!(self.response_count.load(Ordering::SeqCst), 1);
            assert_eq!(self.store.ownership_wins(), 1);
            assert_eq!(
                self.spawner.cancels.lock().await.len(),
                expected_child_cancels
            );
            assert_eq!(
                self.spawner.disconnects.lock().await.len(),
                1,
                "only the explicit terminal/Stop lifecycle tears down the child"
            );
            assert_eq!(
                self.wait_cancel
                    .cancel(&self.stamp, CancelCause::AutoTimeout)
                    .await,
                WaitCancelResult::NotFound,
                "the exact WaitStamp is cleaned, not merely a same-id replacement"
            );
        }
    }

    #[derive(Clone, Copy)]
    enum TestFlushMode {
        Pass,
        GateOk,
        GateErr,
    }

    struct FlushGateStream<S> {
        inner: S,
        mode: TestFlushMode,
        entered: Option<oneshot::Sender<()>>,
        release: Option<oneshot::Receiver<()>>,
        successful_flushes: Arc<AtomicUsize>,
        counted: bool,
    }

    impl<S: AsyncRead + Unpin> AsyncRead for FlushGateStream<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for FlushGateStream<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            if !matches!(self.mode, TestFlushMode::Pass) {
                if let Some(entered) = self.entered.take() {
                    let _ = entered.send(());
                }
                if let Some(release) = self.release.as_mut() {
                    match Pin::new(release).poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(_) => self.release = None,
                    }
                }
                if matches!(self.mode, TestFlushMode::GateErr) {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "injected status response flush failure",
                    )));
                }
            }
            match Pin::new(&mut self.inner).poll_flush(cx) {
                Poll::Ready(Ok(())) => {
                    if !self.counted {
                        self.successful_flushes.fetch_add(1, Ordering::SeqCst);
                        self.counted = true;
                    }
                    Poll::Ready(Ok(()))
                }
                other => other,
            }
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    struct ServeOneContinuationHarness {
        broker: Arc<DelegationBroker>,
        spawner: Arc<MockSpawner>,
        store: Arc<ReleaseObservedStore>,
        port: Arc<ContinuationTestPort>,
        coordinator: Arc<DelegationContinuationCoordinator>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
        task_id: String,
        client: Option<tokio::io::DuplexStream>,
        server_task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
        suspend_entered: Option<oneshot::Receiver<SuspendRequest>>,
        suspend_release: Option<oneshot::Sender<()>>,
        flush_entered: Option<oneshot::Receiver<()>>,
        flush_release: Option<oneshot::Sender<()>>,
        successful_flushes: Arc<AtomicUsize>,
        stamp: Option<WaitStamp>,
        continuation_id: Option<String>,
        generation: Option<u64>,
    }

    impl ServeOneContinuationHarness {
        async fn new(
            label: &str,
            mode: TestFlushMode,
            attention: Option<Arc<dyn DelegationAttentionStore>>,
        ) -> Self {
            let spawner = Arc::new(MockSpawner::new());
            let broker = match attention {
                Some(store) => make_broker_with_attention(spawner.clone(), store).await,
                None => make_broker(spawner.clone()).await,
            };
            let task_id = broker.seed_live_task_for_test("parent-conn", label).await;
            let store = ReleaseObservedStore::new();
            let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
            let (tokens, coordinator) = continuation_registry_with_store(
                broker.clone(),
                store.clone() as Arc<dyn ContinuationStore>,
                port.clone(),
            );
            tokens
                .register("tok".into(), continuation_token_entry(true))
                .await;
            let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
            let listener = make_listener_with_wait_cancel(
                broker.clone(),
                tokens,
                Some(1),
                wait_cancel.clone(),
            );
            let (client, server) = tokio::io::duplex(8 * 1024);
            let successful_flushes = Arc::new(AtomicUsize::new(0));
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            let mut server = FlushGateStream {
                inner: server,
                mode,
                entered: (!matches!(mode, TestFlushMode::Pass)).then_some(entered_tx),
                release: (!matches!(mode, TestFlushMode::Pass)).then_some(release_rx),
                successful_flushes: successful_flushes.clone(),
                counted: false,
            };
            let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });
            Self {
                broker,
                spawner,
                store,
                port,
                coordinator,
                wait_cancel,
                task_id,
                client: Some(client),
                server_task: Some(server_task),
                suspend_entered: Some(suspend_entered),
                suspend_release: Some(suspend_release),
                flush_entered: (!matches!(mode, TestFlushMode::Pass)).then_some(entered_rx),
                flush_release: (!matches!(mode, TestFlushMode::Pass)).then_some(release_tx),
                successful_flushes,
                stamp: None,
                continuation_id: None,
                generation: None,
            }
        }

        async fn send_join_status(&mut self) {
            write_frame(
                self.client.as_mut().unwrap(),
                &BrokerMessage::Status(BrokerStatusRequest {
                    token: "tok".into(),
                    task_ids: vec![self.task_id.clone()],
                    wait_ms: Some(0),
                    return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                    parent_tool_use_id: "foreground-release-test".into(),
                }),
            )
            .await
            .unwrap();
            self.suspend_entered
                .take()
                .unwrap()
                .await
                .expect("suspension reached");
            self.stamp = Some(
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let Some(stamp) =
                            self.wait_cancel.live_wait_stamps().await.into_iter().next()
                        {
                            break stamp;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("exact wait stamp"),
            );
            let row = self
                .store
                .load_active_for_conversation(1)
                .await
                .unwrap()
                .unwrap();
            self.continuation_id = Some(row.continuation_id);
            self.generation = Some(row.generation);
        }

        fn release_suspension(&mut self) {
            self.suspend_release.take().unwrap().send(()).unwrap();
        }

        async fn await_flush(&mut self) {
            self.flush_entered.take().unwrap().await.unwrap();
        }

        fn release_flush(&mut self) {
            self.flush_release.take().unwrap().send(()).unwrap();
        }

        async fn read_one_response(&mut self) -> BrokerResponse {
            read_frame(self.client.as_mut().unwrap()).await.unwrap()
        }

        async fn join_server(&mut self) -> std::io::Result<()> {
            self.server_task.take().unwrap().await.unwrap()
        }

        async fn await_one_admission(&self) -> (u64, ContinuationWakeReason) {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(record) = self
                        .port
                        .admissions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .first()
                        .copied()
                    {
                        break record;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("one same-generation admission")
        }

        async fn await_wake_pending(&self) -> ContinuationRecord {
            let continuation_id = self.continuation_id.as_deref().unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let row = self.store.load(continuation_id).await.unwrap().unwrap();
                    if row.state == ContinuationState::WakePending {
                        break row;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("one wake CAS wins while the frame is gated")
        }

        async fn assert_common(
            &self,
            expected_flushes: usize,
            expected_cancels: usize,
            expected_disconnects: usize,
        ) {
            let stamp = self.stamp.as_ref().unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while self.wait_cancel.contains(&stamp.wait_id).await
                    || self.store.ownership_wins() != 1
                    || self.spawner.cancels.lock().await.len() != expected_cancels
                    || self.spawner.disconnects.lock().await.len() != expected_disconnects
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("serve_one lifecycle settles");
            assert_eq!(
                self.successful_flushes.load(Ordering::SeqCst),
                expected_flushes
            );
            assert_eq!(self.store.ownership_wins(), 1);
            assert_eq!(self.spawner.cancels.lock().await.len(), expected_cancels);
            assert_eq!(
                self.spawner.disconnects.lock().await.len(),
                expected_disconnects
            );
            assert_eq!(
                self.wait_cancel
                    .cancel(stamp, CancelCause::AutoTimeout)
                    .await,
                WaitCancelResult::NotFound
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn foreground_release_blocks_terminal_admission_until_frame_flush() {
        let mut fx =
            ServeOneContinuationHarness::new("frame-terminal", TestFlushMode::GateOk, None).await;
        fx.send_join_status().await;
        fx.release_suspension();
        fx.await_flush().await;
        complete_running_task(&fx.broker, &fx.task_id).await;
        let row = fx.await_wake_pending().await;
        assert_eq!(row.state, ContinuationState::WakePending);
        assert!(fx.port.admissions.lock().unwrap().is_empty());
        assert_eq!(fx.spawner.disconnects.lock().await.len(), 1);
        fx.release_flush();
        let _response = fx.read_one_response().await;
        fx.join_server().await.expect("one response frame flushed");
        let (generation, reason) = fx.await_one_admission().await;
        assert_eq!(generation, fx.generation.unwrap());
        assert_eq!(reason, ContinuationWakeReason::AllTerminal);
        fx.assert_common(1, 0, 1).await;
    }

    #[tokio::test(start_paused = true)]
    async fn foreground_release_blocks_attention_admission_until_frame_flush() {
        let attention = Arc::new(MemoryDelegationAttentionStore::new());
        let mut fx = ServeOneContinuationHarness::new(
            "frame-attention",
            TestFlushMode::GateOk,
            Some(attention.clone() as Arc<dyn DelegationAttentionStore>),
        )
        .await;
        fx.send_join_status().await;
        fx.release_suspension();
        fx.await_flush().await;
        attention
            .open_or_recover(NewAttentionRequest {
                task_id: fx.task_id.clone(),
                parent_conversation_id: 1,
                child_conversation_id: 99,
                child_tool_call_id: "child-tool".into(),
                message: "Need a decision".into(),
                created_at: Utc::now(),
            })
            .await
            .unwrap();
        fx.broker.notify_attention_changed_for_test();
        let row = fx.await_wake_pending().await;
        assert_eq!(
            row.wake_reason,
            Some(ContinuationWakeReason::AttentionRequired)
        );
        assert!(fx.port.admissions.lock().unwrap().is_empty());
        fx.release_flush();
        let _response = fx.read_one_response().await;
        fx.join_server().await.expect("one response frame flushed");
        let (generation, reason) = fx.await_one_admission().await;
        assert_eq!(generation, fx.generation.unwrap());
        assert_eq!(reason, ContinuationWakeReason::AttentionRequired);
        fx.assert_common(1, 0, 0).await;
        complete_running_task(&fx.broker, &fx.task_id).await;
    }

    #[tokio::test]
    async fn foreground_release_peer_eof_read_branch_opens_same_generation() {
        let mut fx = ServeOneContinuationHarness::new("peer-eof", TestFlushMode::Pass, None).await;
        fx.send_join_status().await;
        drop(fx.client.take());
        fx.join_server()
            .await
            .expect("EOF/read branch returns Ok(())");
        fx.release_suspension();
        assert_eq!(fx.spawner.disconnects.lock().await.len(), 0);
        complete_running_task(&fx.broker, &fx.task_id).await;
        let (generation, reason) = fx.await_one_admission().await;
        assert_eq!(generation, fx.generation.unwrap());
        assert_eq!(reason, ContinuationWakeReason::AllTerminal);
        fx.assert_common(0, 0, 1).await;
    }

    #[tokio::test]
    async fn foreground_release_response_write_failure_opens_same_generation() {
        let mut fx =
            ServeOneContinuationHarness::new("peer-write-fail", TestFlushMode::GateErr, None).await;
        fx.send_join_status().await;
        fx.release_suspension();
        fx.await_flush().await;
        complete_running_task(&fx.broker, &fx.task_id).await;
        fx.await_wake_pending().await;
        assert!(fx.port.admissions.lock().unwrap().is_empty());
        assert_eq!(fx.spawner.disconnects.lock().await.len(), 1);
        fx.release_flush();
        let error = fx.join_server().await.expect_err("injected flush fails");
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        let (generation, reason) = fx.await_one_admission().await;
        assert_eq!(generation, fx.generation.unwrap());
        assert_eq!(reason, ContinuationWakeReason::AllTerminal);
        fx.assert_common(0, 0, 1).await;
    }

    #[tokio::test]
    async fn foreground_release_wait_remains_user_stop_cancelable() {
        let mut fx =
            ServeOneContinuationHarness::new("stop-while-fenced", TestFlushMode::GateOk, None)
                .await;
        fx.send_join_status().await;
        fx.release_suspension();
        fx.await_flush().await;
        assert_eq!(
            fx.coordinator
                .handle_parent_stop("parent-conn", 1)
                .await
                .unwrap(),
            1
        );
        fx.release_flush();
        let _response = fx.read_one_response().await;
        fx.join_server().await.expect("one response frame flushed");
        assert!(fx.port.admissions.lock().unwrap().is_empty());
        let row = fx
            .store
            .load(fx.continuation_id.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, ContinuationState::Cancelled);
        fx.assert_common(1, 1, 1).await;
    }

    #[tokio::test]
    async fn continuation_registry_weak_owner_drops_and_is_instance_scoped() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (tokens, coordinator) =
            continuation_registry(broker.clone(), store, ContinuationTestPort::ready());
        let weak = Arc::downgrade(&coordinator);
        assert!(Arc::ptr_eq(
            &tokens
                .continuation_coordinator()
                .expect("registry resolves its live AppState coordinator"),
            &coordinator,
        ));

        drop(coordinator);

        assert!(
            weak.upgrade().is_none(),
            "the registry must not keep the AppState coordinator alive"
        );
        assert!(tokens.continuation_coordinator().is_none());

        let second_store = Arc::new(InMemoryContinuationStore::default());
        let (second_tokens, second_coordinator) =
            continuation_registry(broker, second_store, ContinuationTestPort::ready());
        assert!(tokens.continuation_coordinator().is_none());
        assert!(Arc::ptr_eq(
            &second_tokens
                .continuation_coordinator()
                .expect("a separate registry resolves only its own coordinator"),
            &second_coordinator,
        ));
    }

    fn make_listener(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        parent_conversation: Option<i32>,
    ) -> Arc<DelegationListener> {
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(StaticParentLookup(parent_conversation)),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
        )
    }

    fn make_listener_with_wait_cancel(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        parent_conversation: Option<i32>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
    ) -> Arc<DelegationListener> {
        DelegationListener::new_with_wait_cancel(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(StaticParentLookup(parent_conversation)),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            wait_cancel,
        )
    }

    fn make_listener_with_lookup_and_wait_cancel(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        parent_lookup: Arc<dyn ParentSessionLookup>,
        wait_cancel: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
    ) -> Arc<DelegationListener> {
        DelegationListener::new_with_wait_cancel(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            parent_lookup,
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            wait_cancel,
        )
    }

    /// Build a listener whose feedback access is the given stub, so feedback
    /// tests can seed notes and assert the drain. Delegation pieces are minimal.
    fn make_feedback_listener(
        tokens: Arc<TokenRegistry>,
        feedback: Arc<StubFeedback>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(StaticParentLookup(Some(1))),
            feedback,
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
        )
    }

    /// Build a listener whose question access is the given stub, so ask tests
    /// can register/answer questions and assert the round-trip. Delegation and
    /// feedback pieces are minimal.
    fn make_question_listener(
        tokens: Arc<TokenRegistry>,
        questions: Arc<StubQuestion>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            questions,
            Arc::new(StubSessionInfo::default()),
        )
    }

    /// Build a listener whose session-info access is the given stub, so
    /// `get_session_info` tests can seed known sessions and assert the round-trip.
    fn make_session_listener(
        tokens: Arc<TokenRegistry>,
        session_info: Arc<StubSessionInfo>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            session_info,
        )
    }

    async fn make_request(input: serde_json::Value) -> BrokerRequest {
        make_request_with_host_id(input, "pt-1").await
    }

    async fn make_request_with_host_id(
        input: serde_json::Value,
        parent_tool_use_id: &str,
    ) -> BrokerRequest {
        BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "parent-conn".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            external_handle: None,
            input,
        }
    }

    #[tokio::test]
    async fn invalid_token_rejected() {
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            Arc::new(TokenRegistry::default()),
            Some(1),
        );
        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert_eq!(report.error_code.as_deref(), Some("canceled"));
        assert!(report.message.unwrap().contains("invalid token"));
    }

    #[tokio::test]
    async fn token_parent_mismatch_rejected() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("other-parent", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert!(report.message.unwrap().contains("does not match"));
    }

    #[tokio::test]
    async fn missing_parent_conversation_rejected() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        // parent_conversation = None: parent has no live conversation.
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            None,
        );
        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert!(report.message.unwrap().contains("no active conversation"));
    }

    #[tokio::test]
    async fn invalid_agent_type_rejected() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let report = listener
            .process(make_request(json!({"agent_type": "garbage", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("invalid_agent_type"));
    }

    #[test]
    fn cohort_frozen_workflow_error_uses_exact_wire_code() {
        let outcome = workflow_store_error_value(WorkflowStoreError::CohortFrozen {
            node_id: "Task 1".into(),
        });

        assert_eq!(outcome["error"]["code"], "cohort_frozen");
    }

    // -- Task 1: correlation_id parse/forward ------------------------------

    #[test]
    fn parse_correlation_id_absent_is_none() {
        assert_eq!(parse_correlation_id(&json!({})).unwrap(), None);
    }

    #[test]
    fn parse_correlation_id_null_is_err() {
        assert!(
            parse_correlation_id(&json!({"correlation_id": null})).is_err(),
            "explicit JSON null is malformed, not omission"
        );
    }

    #[test]
    fn parse_correlation_id_accepts_valid() {
        assert_eq!(
            parse_correlation_id(&json!({"correlation_id": "abc-123"})).unwrap(),
            Some("abc-123".into())
        );
    }

    #[test]
    fn parse_correlation_id_rejects_invalid_formats() {
        assert!(parse_correlation_id(&json!({"correlation_id": ""})).is_err());
        assert!(parse_correlation_id(&json!({"correlation_id": "has space"})).is_err());
        assert!(parse_correlation_id(&json!({"correlation_id": ".dot"})).is_err());
        assert!(parse_correlation_id(&json!({"correlation_id": "x".repeat(129)})).is_err());
        assert!(parse_correlation_id(&json!({"correlation_id": 1})).is_err());
    }

    #[tokio::test]
    async fn process_rejects_malformed_correlation_id_on_delegate() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        // Empty host tool id: argument correlation is the only key path.
        let report = listener
            .process(
                make_request_with_host_id(
                    json!({
                        "agent_type": "codex",
                        "task": "x",
                        "correlation_id": ".bad"
                    }),
                    "",
                )
                .await,
            )
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
        let msg = report.message.as_deref().unwrap_or("").to_ascii_lowercase();
        assert!(
            msg.contains("delegate_to_agent"),
            "delegate entry message: {msg}"
        );
        assert!(!msg.contains("continue_delegation"), "{msg}");
    }

    #[tokio::test]
    async fn process_rejects_malformed_correlation_id_on_continue() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        // Empty host tool id: argument correlation is the only key path.
        let report = listener
            .process(
                make_request_with_host_id(
                    json!({
                        "_codeg_tool": "continue_delegation",
                        "task_id": "task-1",
                        "task": "revise",
                        "correlation_id": "bad id"
                    }),
                    "",
                )
                .await,
            )
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
        let msg = report.message.as_deref().unwrap_or("").to_ascii_lowercase();
        assert!(
            msg.contains("continue_delegation"),
            "continue entry message: {msg}"
        );
        assert!(
            msg.contains("get_delegation_status") || msg.contains("latest terminal"),
            "continue retry guidance: {msg}"
        );
    }

    #[tokio::test]
    async fn process_host_id_precedes_malformed_correlation_id() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        // Non-empty host tool id is authoritative: bad correlation_id must not
        // fail correlation validation. Use an invalid agent_type so the call
        // fails later for a different, deterministic reason.
        let report = listener
            .process(
                make_request(json!({
                    "agent_type": "garbage",
                    "task": "x",
                    "correlation_id": ".bad"
                }))
                .await,
            )
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("invalid_agent_type"));
        assert_ne!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
    }

    /// Nonblank host ids are opaque: surrounding whitespace is part of the
    /// identity and must survive listener-to-broker forwarding unchanged.
    #[tokio::test]
    async fn process_preserves_nonblank_host_tool_id_whitespace() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-padded-host-id".into())).await;
        mock.queue_send(Ok(accepted(100, Utc::now()))).await;
        let broker = make_broker(mock).await;
        let host_tool_id = "  explicit-card-with-padding  ";
        broker
            .register_pending_tool_call("parent-conn", host_tool_id.into())
            .await;

        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let report = listener
            .process(
                make_request_with_host_id(
                    json!({
                        "agent_type": "codex",
                        "task": "do x",
                        "correlation_id": ".bad"
                    }),
                    host_tool_id,
                )
                .await,
            )
            .await;

        assert_eq!(report.status, TaskStatus::Running);
        assert_ne!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
        assert!(
            broker.take_pending_tool_call("parent-conn").await.is_none(),
            "the exact padded host id must be consumed, not trimmed"
        );
    }

    #[tokio::test]
    async fn process_rejects_null_correlation_id_without_host_id() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let report = listener
            .process(
                make_request_with_host_id(
                    json!({
                        "agent_type": "codex",
                        "task": "x",
                        "correlation_id": null
                    }),
                    "",
                )
                .await,
            )
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
    }

    /// P1 regression: whitespace-only `_meta.tool_use_id` must be treated as
    /// absent (same as empty). Previously the listener gated on trim but
    /// forwarded raw whitespace into the broker, which took the explicit-id
    /// path and never claimed by `correlation_id`.
    #[tokio::test]
    async fn process_whitespace_only_host_tool_id_uses_correlation_path() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-ws-host".into())).await;
        mock.queue_send(Ok(accepted(99, Utc::now()))).await;
        let broker = make_broker(mock).await;
        let match_key = DelegationMatchKey::Delegate {
            correlation_id: "corr-ws-host".into(),
            agent_type: AgentType::Codex,
            task: "do x".into(),
            working_dir: None,
        };
        broker
            .register_pending_tool_call_with_key(
                "parent-conn",
                "delegate-card-ws-host".into(),
                Some(match_key),
            )
            .await;

        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(broker, tokens, Some(1));
        let report = listener
            .process(
                make_request_with_host_id(
                    json!({
                        "agent_type": "codex",
                        "task": "do x",
                        "correlation_id": "corr-ws-host"
                    }),
                    "   ",
                )
                .await,
            )
            .await;

        assert!(
            report.error_code.is_none(),
            "whitespace host + valid corr must claim ACP card: err={:?} msg={:?}",
            report.error_code,
            report.message
        );
        assert_eq!(report.status, TaskStatus::Running);
        assert_eq!(report.child_conversation_id, Some(99));
        assert_ne!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
        assert_ne!(
            report.error_code.as_deref(),
            Some("delegation_correlation_timeout")
        );
        assert_ne!(
            report.error_code.as_deref(),
            Some("missing_parent_tool_use_id")
        );
    }

    /// Whitespace-only host id is absent: malformed correlation must fail closed
    /// at the listener (not skip validation as if a real host id were present).
    #[tokio::test]
    async fn process_whitespace_only_host_tool_id_rejects_malformed_correlation() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let report = listener
            .process(
                make_request_with_host_id(
                    json!({
                        "agent_type": "codex",
                        "task": "x",
                        "correlation_id": ".bad"
                    }),
                    " \t ",
                )
                .await,
            )
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(
            report.error_code.as_deref(),
            Some("delegation_correlation_missing")
        );
    }

    // -- Task 4 Codex I2: work_unit_key parse/forward -----------------------

    #[test]
    fn parse_work_unit_key_absent_or_blank_is_none() {
        assert_eq!(parse_work_unit_key(&json!({})).unwrap(), None);
        assert_eq!(
            parse_work_unit_key(&json!({"work_unit_key": null})).unwrap(),
            None
        );
        assert_eq!(
            parse_work_unit_key(&json!({"work_unit_key": "  "})).unwrap(),
            None
        );
    }

    #[test]
    fn parse_work_unit_key_trims_and_accepts() {
        assert_eq!(
            parse_work_unit_key(&json!({"work_unit_key": "  unit-a  "})).unwrap(),
            Some("unit-a".into())
        );
    }

    #[test]
    fn parse_work_unit_key_rejects_non_string_and_overlong() {
        assert!(parse_work_unit_key(&json!({"work_unit_key": 1})).is_err());
        let long: String = "x".repeat(201);
        let err = parse_work_unit_key(&json!({"work_unit_key": long})).unwrap_err();
        assert!(err.contains("200"), "{err}");
    }

    #[test]
    fn parse_recovery_authorization_id_accepts_opaque_receipt_and_rejects_invalid_values() {
        assert_eq!(
            parse_recovery_authorization_id(&json!({
                "recovery_authorization_id": "receipt-opaque"
            }))
            .unwrap(),
            Some("receipt-opaque".into())
        );
        assert_eq!(parse_recovery_authorization_id(&json!({})).unwrap(), None);
        for value in [
            json!({"recovery_authorization_id": "  "}),
            json!({"recovery_authorization_id": null}),
            json!({"recovery_authorization_id": 7}),
        ] {
            assert!(parse_recovery_authorization_id(&value).is_err());
        }
    }

    #[tokio::test]
    async fn process_rejects_overlong_work_unit_key() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let long: String = "k".repeat(201);
        let report = listener
            .process(
                make_request(json!({
                    "agent_type": "codex",
                    "task": "x",
                    "work_unit_key": long
                }))
                .await,
            )
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("invalid_work_unit_key"));
    }

    /// Full async round-trip through the listener: `delegate_to_agent` returns a
    /// Running ack, the lifecycle resolves the child via `complete_call`, and a
    /// follow-up `get_delegation_status` collects the Completed result.
    #[tokio::test]
    async fn happy_path_ack_then_status_collects_result() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(accepted(42, Utc::now()))).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;

        // 1. delegate_to_agent → Running ack carrying the child conversation id.
        let listener = make_listener(broker.clone(), tokens.clone(), Some(1));
        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::Call(BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "parent-conn".into(),
            parent_tool_use_id: "pt-1".into(),
            external_handle: None,
            input: json!({"agent_type": "codex", "task": "do x"}),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let ack: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(ack.outcome["status"], "running");
        assert_eq!(ack.outcome["child_conversation_id"], 42);
        let task_id = ack.outcome["task_id"].as_str().unwrap().to_string();

        // 2. The lifecycle resolves the child on TurnComplete.
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "result-text".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;

        // 3. get_delegation_status → Completed with the result text.
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id.clone()],
            wait_ms: Some(1_000),
            return_when: None,
            parent_tool_use_id: String::new(),
        });
        write_frame(&mut client, &status).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        // The Status arm returns a `{ tasks: [..] }` envelope; a single id is
        // the first (only) entry.
        assert_eq!(resp.outcome["tasks"][0]["status"], "completed");
        assert_eq!(resp.outcome["tasks"][0]["text"], "result-text");
        assert_eq!(resp.outcome["tasks"][0]["child_conversation_id"], 42);
    }

    /// Start a running task directly and return `(broker, tokens, task_id)`.
    /// Shared setup for the `wait_ms` mapping tests below.
    async fn running_task_fixture() -> (Arc<DelegationBroker>, Arc<TokenRegistry>, String) {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(accepted(7, Utc::now()))).await;
        let broker = make_broker(mock).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let ack = broker
            .start_delegation(DelegationRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "pt-1".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "do x".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await;
        let task_id = ack.task_id.clone().expect("running task carries an id");
        (broker, tokens, task_id)
    }

    async fn complete_running_task(broker: &DelegationBroker, task_id: &str) {
        broker
            .complete_call(
                task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
    }

    #[tokio::test]
    async fn continuation_causeless_release_returns_running_snapshot() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener_with_wait_cancel(
            broker,
            tokens,
            Some(1),
            crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared(),
        );

        let batch = listener
            .continuation_release_batch("parent-conn", 1, std::slice::from_ref(&task_id))
            .await;

        assert_eq!(batch.tasks[0].status, TaskStatus::Running);
        assert_eq!(batch.tasks[0].error_code, None);
        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
    }

    #[tokio::test]
    async fn continuation_causeless_release_returns_completed_snapshot() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        complete_running_task(&broker, &task_id).await;
        let listener = make_listener_with_wait_cancel(
            broker,
            tokens,
            Some(1),
            crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared(),
        );

        let batch = listener
            .continuation_release_batch("parent-conn", 1, std::slice::from_ref(&task_id))
            .await;

        assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
        assert_ne!(
            batch.tasks[0].error_code.as_deref(),
            Some("tool_stalled_timeout")
        );
    }

    #[tokio::test]
    async fn continuation_release_wait_cancel_before_decision_wins_once() {
        let mut fx = ContinuationReleaseHarness::new("cancel-first", false).await;
        fx.cancel_exact().await;
        fx.release_decision();
        let batch = fx.finish_status().await;
        assert_eq!(batch.tasks.len(), 1);
        assert_eq!(batch.tasks[0].status, TaskStatus::Canceled);
        fx.release_suspension();
        let waiting = fx.await_waiting().await;
        assert_eq!(waiting.state, ContinuationState::Waiting);
        assert_eq!(fx.store.ownership_wins(), 0);
        assert_eq!(fx.broker.pending_count().await, 1);
        complete_running_task(&fx.broker, &fx.task_id).await;
        fx.assert_common(0).await;
        assert_eq!(fx.broker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn continuation_release_suspended_wins_simultaneous_wait_cancel() {
        let mut fx = ContinuationReleaseHarness::new("simultaneous", false).await;
        fx.release_suspension();
        fx.await_arm_suspended().await;
        fx.cancel_exact().await;
        fx.release_decision();
        let batch = fx.finish_status().await;
        assert_eq!(batch.tasks[0].status, TaskStatus::Running);
        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
        complete_running_task(&fx.broker, &fx.task_id).await;
        fx.assert_common(0).await;
    }

    #[tokio::test]
    async fn continuation_release_post_ack_cancel_is_cleanup_only() {
        let mut fx = ContinuationReleaseHarness::new("post-ack", true).await;
        fx.release_suspension();
        fx.await_arm_suspended().await;
        fx.release_decision();
        fx.await_snapshot_before().await;
        fx.cancel_exact().await;
        fx.allow_snapshot_read();
        fx.await_snapshot_after().await;
        fx.allow_snapshot_assemble();
        let batch = fx.finish_status().await;
        assert_eq!(batch.tasks[0].status, TaskStatus::Running);
        let row = fx
            .store
            .load_active_for_conversation(1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, ContinuationState::Waiting);
        complete_running_task(&fx.broker, &fx.task_id).await;
        fx.assert_common(0).await;
    }

    #[tokio::test]
    async fn continuation_release_user_stop_keeps_one_observational_response() {
        let mut fx = ContinuationReleaseHarness::new("user-stop", true).await;
        fx.release_suspension();
        fx.await_arm_suspended().await;
        fx.release_decision();
        fx.await_snapshot_before().await;
        fx.allow_snapshot_read();
        fx.await_snapshot_after().await;
        assert_eq!(
            fx.coordinator
                .handle_parent_stop("parent-conn", 1)
                .await
                .unwrap(),
            1
        );
        fx.allow_snapshot_assemble();
        let batch = fx.finish_status().await;
        assert_eq!(batch.tasks.len(), 1);
        assert_eq!(batch.tasks[0].status, TaskStatus::Running);
        fx.assert_common(1).await;
        assert_eq!(fx.broker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn continuation_release_terminal_before_snapshot_is_terminal() {
        let mut fx = ContinuationReleaseHarness::new("terminal-before", true).await;
        fx.release_suspension();
        fx.await_arm_suspended().await;
        fx.release_decision();
        fx.await_snapshot_before().await;
        complete_running_task(&fx.broker, &fx.task_id).await;
        fx.allow_snapshot_read();
        fx.await_snapshot_after().await;
        fx.allow_snapshot_assemble();
        let batch = fx.finish_status().await;
        assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
        fx.assert_common(0).await;
    }

    #[tokio::test]
    async fn continuation_release_terminal_after_snapshot_keeps_running_observation() {
        let mut fx = ContinuationReleaseHarness::new("terminal-after", true).await;
        fx.release_suspension();
        fx.await_arm_suspended().await;
        fx.release_decision();
        fx.await_snapshot_before().await;
        fx.allow_snapshot_read();
        fx.await_snapshot_after().await;
        complete_running_task(&fx.broker, &fx.task_id).await;
        fx.allow_snapshot_assemble();
        let batch = fx.finish_status().await;
        assert_eq!(batch.tasks[0].status, TaskStatus::Running);
        fx.assert_common(0).await;
    }

    #[tokio::test]
    async fn continuation_capability_off_keeps_existing_parked_join() {
        let (attention, mut join_evaluations) = JoinEntryAttentionStore::new();
        let broker = make_broker_with_attention(
            Arc::new(MockSpawner::new()),
            attention as Arc<dyn DelegationAttentionStore>,
        )
        .await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "continuation-off-running")
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register("tok".into(), continuation_token_entry(false))
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let request_task_id = task_id.clone();

        let join = tokio::spawn(async move {
            listener
                .process_status(BrokerStatusRequest {
                    token: "tok".into(),
                    task_ids: vec![request_task_id],
                    wait_ms: Some(0),
                    return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                    parent_tool_use_id: String::new(),
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), join_evaluations.recv())
            .await
            .expect("the old Join must enter its first real Broker evaluation")
            .expect("the evaluation observer must remain installed");
        broker.notify_result_for_test(&task_id);
        tokio::time::timeout(Duration::from_secs(1), join_evaluations.recv())
            .await
            .expect("an unrelated wake must drive a second parked Join evaluation")
            .expect("the evaluation observer must remain installed");
        assert!(
            !join.is_finished(),
            "capability-off Join must retain the existing parked listener behavior"
        );

        complete_running_task(&broker, "continuation-off-running").await;
        let batch = join.await.unwrap().unwrap().batch;
        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::AllTerminal));
        assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
    }

    /// Task 10 matrix sequence 1: capability-off canonical Join retains the
    /// existing parked call at the real listener/broker boundary (no arm row).
    #[tokio::test]
    async fn delegation_continuation_e2e_capability_off_retains_parked_join() {
        let (attention, mut join_evaluations) = JoinEntryAttentionStore::new();
        let broker = make_broker_with_attention(
            Arc::new(MockSpawner::new()),
            attention as Arc<dyn DelegationAttentionStore>,
        )
        .await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "e2e-cap-off")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let port = ContinuationTestPort::ready();
        let (tokens, coordinator) =
            continuation_registry(broker.clone(), store.clone(), port.clone());
        tokens
            .register("tok".into(), continuation_token_entry(false))
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let request_task_id = task_id.clone();
        let join = tokio::spawn(async move {
            listener
                .process_status(BrokerStatusRequest {
                    token: "tok".into(),
                    task_ids: vec![request_task_id],
                    wait_ms: Some(0),
                    return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                    parent_tool_use_id: String::new(),
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), join_evaluations.recv())
            .await
            .expect("capability-off Join must enter the parked Broker evaluation")
            .expect("evaluation observer installed");
        assert!(
            !join.is_finished(),
            "capability-off Join remains parked while children run"
        );
        assert!(
            store.list_non_terminal().await.unwrap().is_empty(),
            "capability-off must not insert a continuation row"
        );
        assert_eq!(port.snapshot_calls.load(Ordering::SeqCst), 0);
        assert_eq!(coordinator.worker_count(), 0);

        broker.notify_result_for_test(&task_id);
        tokio::time::timeout(Duration::from_secs(1), join_evaluations.recv())
            .await
            .expect("parked Join re-evaluates on wake without arming")
            .expect("evaluation observer installed");
        assert!(!join.is_finished());

        complete_running_task(&broker, "e2e-cap-off").await;
        let batch = join.await.unwrap().unwrap().batch;
        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::AllTerminal));
        assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
        assert!(store.list_non_terminal().await.unwrap().is_empty());
        assert_eq!(coordinator.worker_count(), 0);
    }

    /// Task 10 matrix sequence 3 (listener transport): peer-close during arm
    /// drops only the waiter; the owned worker and children continue.
    #[tokio::test]
    async fn delegation_continuation_e2e_peer_close_listener_keeps_children_running() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        broker
            .seed_live_task_for_test("parent-conn", "e2e-peer-listener")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
        let (tokens, _coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["e2e-peer-listener".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .unwrap();
        let suspend = suspend_entered
            .await
            .expect("suspend proves row ownership before peer-close");
        assert_eq!(suspend.parent_connection_id, "parent-conn");
        assert_eq!(store.list_non_terminal().await.unwrap().len(), 1);

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("peer EOF must release serve_one")
            .unwrap()
            .unwrap();
        assert_eq!(
            store.list_non_terminal().await.unwrap().len(),
            1,
            "peer-close must not abort the armed continuation"
        );
        assert_eq!(broker.pending_count().await, 1, "children remain Running");

        suspend_release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let rows = store.list_non_terminal().await.unwrap();
                if rows
                    .first()
                    .is_some_and(|row| row.state == ContinuationState::Waiting)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned worker reaches Waiting after peer-close");

        complete_running_task(&broker, "e2e-peer-listener").await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !store.list_non_terminal().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned worker finishes after child terminal");
    }

    #[tokio::test]
    async fn continuation_capability_empty_join_returns_unavailable_without_row() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let port = ContinuationTestPort::ready();
        let (tokens, _coordinator) =
            continuation_registry(broker.clone(), store.clone(), port.clone());
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker, tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: Vec::new(),
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .unwrap();
        let response: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap().unwrap();

        assert_eq!(response.outcome["wake_reason"], "unavailable");
        assert_eq!(response.outcome["tasks"], json!([]));
        assert!(store.list_non_terminal().await.unwrap().is_empty());
        assert_eq!(
            port.snapshot_calls.load(Ordering::SeqCst),
            0,
            "empty Join must not construct or dispatch a JoinArmRequest"
        );
    }

    #[tokio::test]
    async fn continuation_capability_unbound_parent_returns_unavailable_without_row() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        broker
            .seed_live_task_for_test("parent-conn", "unbound-running")
            .await;
        broker
            .seed_live_task_for_test("other-parent", "foreign-running")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let port = ContinuationTestPort::ready();
        let (tokens, _coordinator) =
            continuation_registry(broker.clone(), store.clone(), port.clone());
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker.clone(), tokens.clone(), None);

        let batch = listener
            .process_status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["unbound-running".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            })
            .await
            .unwrap()
            .batch;

        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
        assert_eq!(batch.tasks[0].status, TaskStatus::Unknown);

        let bound_listener = make_listener(broker, tokens, Some(1));
        let invalid_batch = bound_listener
            .process_status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["missing".into(), "foreign-running".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            })
            .await
            .unwrap()
            .batch;
        assert_eq!(
            invalid_batch.wake_reason,
            Some(DelegationWakeReason::Unavailable)
        );
        assert!(invalid_batch
            .tasks
            .iter()
            .all(|task| task.status == TaskStatus::Unknown));
        assert!(store.list_non_terminal().await.unwrap().is_empty());
        assert_eq!(
            port.snapshot_calls.load(Ordering::SeqCst),
            0,
            "an unbound parent must not construct or dispatch a JoinArmRequest"
        );
    }

    #[tokio::test]
    async fn continuation_peer_close_before_insert_creates_no_row() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        broker
            .seed_live_task_for_test("parent-conn", "pre-insert-running")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (port, snapshot_entered, snapshot_release) = ContinuationTestPort::snapshot_gated();
        let (tokens, coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker, tokens, Some(1));
        let baseline_coordinator_owners = Arc::strong_count(&coordinator);
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["pre-insert-running".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .unwrap();
        snapshot_entered
            .await
            .expect("snapshot gate establishes the pre-insert boundary");
        assert!(Arc::strong_count(&coordinator) > baseline_coordinator_owners);

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("peer EOF must drop the status waiter")
            .unwrap()
            .unwrap();
        snapshot_release.send(()).unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while Arc::strong_count(&coordinator) != baseline_coordinator_owners {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached pre-insert arm task must observe waiter cancellation and exit");
        assert!(store.list_non_terminal().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn continuation_peer_close_during_suspend_does_not_abort_arm_worker() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        broker
            .seed_live_task_for_test("parent-conn", "post-insert-running")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
        let (tokens, _coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["post-insert-running".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .unwrap();
        let suspend = suspend_entered
            .await
            .expect("suspend entry proves the row and worker own the arm");
        assert_eq!(suspend.parent_connection_id, "parent-conn");
        assert_eq!(store.list_non_terminal().await.unwrap().len(), 1);

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("peer EOF must release serve_one")
            .unwrap()
            .unwrap();
        assert_eq!(store.list_non_terminal().await.unwrap().len(), 1);

        suspend_release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let rows = store.list_non_terminal().await.unwrap();
                if rows
                    .first()
                    .is_some_and(|row| row.state == ContinuationState::Waiting)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached arm task must allow the owned worker to publish Waiting");

        complete_running_task(&broker, "post-insert-running").await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !store.list_non_terminal().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned worker must finish after the child becomes terminal");
        assert!(store.list_non_terminal().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn continuation_arm_failure_returns_explicit_tool_error() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        broker
            .seed_live_task_for_test("parent-conn", "arm-failure-running")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (tokens, _coordinator) = continuation_registry(
            broker.clone(),
            store.clone(),
            ContinuationTestPort::failing(),
        );
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker, tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["arm-failure-running".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .unwrap();
        let response: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap().unwrap();

        assert_eq!(
            response.outcome,
            json!({
                "error": {
                    "code": "continuation_arm_failed",
                    "message": "Delegation continuation could not be armed"
                }
            })
        );
        let rendered = crate::acp::delegation::companion::render_status_result(&response.outcome);
        assert_eq!(rendered["isError"], true);
        assert_eq!(
            rendered["content"][0]["text"],
            "Delegation continuation could not be armed"
        );
        assert_eq!(rendered["structuredContent"], response.outcome);
        assert!(store.list_non_terminal().await.unwrap().is_empty());
    }

    /// A legacy token (`coordination_v1=false`) must not enter Join or reveal
    /// whether a requested running task exists, even if raw socket JSON sends
    /// `return_when=all_terminal_or_attention`.
    #[tokio::test]
    async fn legacy_token_cannot_enter_join_or_reveal_a_running_task() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker, tokens, Some(1));
        let batch = tokio::time::timeout(
            Duration::from_secs(1),
            listener.process_status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec![task_id],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .expect("legacy-token Join rejection must not park")
        .unwrap()
        .batch;
        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
        assert_eq!(batch.tasks[0].status, TaskStatus::Unknown);
        assert!(batch.attention_requests.unwrap().is_empty());
    }

    /// Omitted `wait_ms` (the safe default) maps to an immediate snapshot: the
    /// status of a still-running task returns `running` right away rather than
    /// blocking.
    #[tokio::test]
    async fn status_omitted_wait_returns_immediately() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker, tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id],
            wait_ms: None,
            return_when: None,
            parent_tool_use_id: String::new(),
        });
        write_frame(&mut client, &status).await.unwrap();
        // No completion ever happens — an immediate poll must still return.
        let resp: BrokerResponse = tokio::time::timeout(Duration::from_secs(2), async {
            read_frame::<_, BrokerResponse>(&mut client).await.unwrap()
        })
        .await
        .expect("omitted wait_ms must return immediately");
        server_task.await.unwrap().unwrap();
        assert_eq!(resp.outcome["tasks"][0]["status"], "running");
    }

    /// An explicit `wait_ms = 0` maps to an unbounded wait: the call blocks
    /// while the task is running and only resolves once it reaches a terminal
    /// state, returning the completed report through the wire.
    #[tokio::test]
    async fn status_explicit_zero_blocks_until_terminal() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id.clone()],
            wait_ms: Some(0),
            return_when: None,
            parent_tool_use_id: String::new(),
        });
        write_frame(&mut client, &status).await.unwrap();

        // While the task runs, the wait must NOT resolve.
        let early = tokio::time::timeout(Duration::from_millis(50), async {
            read_frame::<_, BrokerResponse>(&mut client).await
        })
        .await;
        assert!(
            early.is_err(),
            "wait_ms=0 must block while the task is still running"
        );

        // Resolving the task wakes the parked wait, which returns completed.
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap().unwrap();
        assert_eq!(resp.outcome["tasks"][0]["status"], "completed");
        assert_eq!(resp.outcome["tasks"][0]["text"], "done");
    }

    /// A `wait_ms = 0` status call that the companion cancels (dropping the
    /// request socket) must not leave `serve_one` parked until the task is
    /// terminal. The peer-close race abandons the wait while leaving the task
    /// itself untouched — there's no broker-side side effect from a status
    /// query.
    async fn assert_status_peer_close_leaves_children_running() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id],
            wait_ms: Some(0),
            return_when: None,
            parent_tool_use_id: String::new(),
        });
        write_frame(&mut client, &status).await.unwrap();
        // Companion cancels: drop the request socket without completing the task.
        drop(client);

        // serve_one must observe the peer-close and return promptly instead of
        // hanging until the (never-completing) task is terminal.
        let result = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("serve_one must return after the peer closes");
        result.unwrap().unwrap();

        // The task itself was not touched by the abandoned status query.
        assert_eq!(broker.pending_count().await, 1);
        assert_eq!(
            broker
                .metrics()
                .snapshot()
                .wait_return_reasons
                .get("peer_closed"),
            Some(&1),
            "metrics prove serve_one observed peer EOF while the status wait was active"
        );
    }

    #[tokio::test]
    async fn infinite_status_wait_abandoned_when_peer_closes() {
        assert_status_peer_close_leaves_children_running().await;
    }

    /// Legacy indefinite wait registers canonical task_ids before park, and
    /// request-carried tool id is stamped on the wait.
    #[tokio::test]
    async fn legacy_indefinite_registers_canonical_task_ids_and_tool_id() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let listener =
            make_listener_with_wait_cancel(broker.clone(), tokens, Some(1), wait_cancel.clone());

        let status_fut = {
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: None,
                        parent_tool_use_id: "wait-tool-B".into(),
                    })
                    .await
            }
        };
        let status_task = tokio::spawn(status_fut);

        // Wait until registration is live with real task ids.
        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let stamps = wait_cancel.live_wait_stamps().await;
                if let Some(s) = stamps.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("wait must register before park");

        assert_eq!(
            stamp.parent_tool_use_id.as_deref(),
            Some("wait-tool-B"),
            "request-carried wait tool id must be registered"
        );
        assert!(wait_cancel.contains(&stamp.wait_id).await);
        assert_eq!(
            wait_cancel.live_task_ids(&stamp.wait_id).await.as_deref(),
            Some(std::slice::from_ref(&task_id)),
            "canonical task_ids must be stored (not empty)"
        );

        // Wait-only cancel: child stays Running; zero Broker cancel.
        let pending_before = broker.pending_count().await;
        assert_eq!(
            wait_cancel
                .cancel(&stamp, crate::acp::tool_watchdog::CancelCause::AutoTimeout)
                .await,
            crate::acp::tool_watchdog::WaitCancelResult::Cancelled
        );
        let batch = tokio::time::timeout(Duration::from_secs(2), status_task)
            .await
            .expect("cancel must complete wait")
            .expect("join")
            .expect("status ok")
            .batch;
        assert_eq!(batch.tasks.len(), 1);
        assert_eq!(
            batch.tasks[0].error_code.as_deref(),
            Some("tool_stalled_timeout")
        );
        assert_eq!(
            broker.pending_count().await,
            pending_before,
            "wait cancel must not Broker-cancel children"
        );
    }

    /// Incident 1570 production field path (listener layer):
    /// `BrokerStatusRequest.parent_tool_use_id` (as filled by companion `_meta`)
    /// is stamped on the parked wait, surfaces via exact-match progress targets
    /// for activity attribution, and wait-only timeout leaves the child Running
    /// so it can still complete afterward.
    ///
    /// Companion layer: `companion::tests::incident_1570_*`.
    /// Controlled-clock renew/timeout: attribution `conversation_1570_*`.
    #[tokio::test]
    async fn incident_1570_status_parent_tool_use_id_exact_match_and_child_survives_wait_timeout() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let listener =
            make_listener_with_wait_cancel(broker.clone(), tokens, Some(1), wait_cancel.clone());

        // Production field value after companion plumbing (Task 2).
        let wait_tool_b = "wait-B";
        let status_task = tokio::spawn({
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: None,
                        parent_tool_use_id: wait_tool_b.into(),
                    })
                    .await
            }
        });

        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(s) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("wait must register before park");

        assert_eq!(
            stamp.parent_tool_use_id.as_deref(),
            Some(wait_tool_b),
            "listener must stamp request-carried wait tool B (production field)"
        );
        assert_eq!(
            wait_cancel.live_task_ids(&stamp.wait_id).await.as_deref(),
            Some(std::slice::from_ref(&task_id)),
            "canonical task membership for activity exact-match"
        );

        // Activity path: exact-match targets use the production wait tool id B.
        // StaticParentLookup defaults incarnation="" / turn_generation=0.
        let targets = wait_cancel
            .exact_match_progress_targets(
                &task_id,
                &stamp.connection_id,
                &stamp.connection_incarnation,
                stamp.turn_generation,
            )
            .await;
        assert_eq!(
            targets.len(),
            1,
            "live wait must be an exact-match progress target: {targets:?}"
        );
        assert_eq!(targets[0].wait_id, stamp.wait_id);
        assert_eq!(
            targets[0].wait_tool_call_id, wait_tool_b,
            "progress target tool id must be B from the status request field"
        );

        // Wait-only timeout: no Broker child cancel; pending count unchanged.
        let pending_before = broker.pending_count().await;
        assert_eq!(
            wait_cancel
                .cancel(&stamp, crate::acp::tool_watchdog::CancelCause::AutoTimeout)
                .await,
            crate::acp::tool_watchdog::WaitCancelResult::Cancelled
        );
        let batch = tokio::time::timeout(Duration::from_secs(2), status_task)
            .await
            .expect("wait cancel must complete process_status")
            .expect("join")
            .expect("status ok")
            .batch;
        assert_eq!(batch.tasks.len(), 1);
        assert_eq!(
            batch.tasks[0].error_code.as_deref(),
            Some("tool_stalled_timeout")
        );
        assert_eq!(
            broker.pending_count().await,
            pending_before,
            "wait-only timeout must not Broker-cancel the child"
        );
        assert!(
            wait_cancel
                .exact_match_progress_targets(
                    &task_id,
                    &stamp.connection_id,
                    &stamp.connection_incarnation,
                    stamp.turn_generation,
                )
                .await
                .is_empty(),
            "settled wait must drop from exact-match targets"
        );

        // Child can still complete afterward (design 1570 shape).
        complete_running_task(&broker, &task_id).await;
        assert_eq!(
            broker.pending_count().await,
            0,
            "child must be able to complete after wait-only timeout"
        );
    }

    /// Peer-close (drop process_status) during bind must not leak the wait
    /// registration. WaitCancelGuard is installed immediately after register,
    /// so abandoning the future mid-bind Drop-deregisters.
    #[tokio::test]
    async fn peer_close_during_bind_deregisters_wait_registration() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let (lookup, bind_entered, bind_release) = BindGatedParentLookup::new(Some(1));
        let listener = make_listener_with_lookup_and_wait_cancel(
            broker.clone(),
            tokens,
            lookup,
            wait_cancel.clone(),
        );

        let status_task = tokio::spawn({
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: None,
                        parent_tool_use_id: "wait-tool-bind-gate".into(),
                    })
                    .await
            }
        });

        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(s) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("wait must register before bind gate");

        tokio::time::timeout(Duration::from_secs(2), bind_entered)
            .await
            .expect("process_status must reach bind_delegation_wait")
            .expect("bind entered");

        assert!(
            wait_cancel.contains(&stamp.wait_id).await,
            "registration must still be live while bind is gated"
        );

        // Peer-close abandons process_status while bind is in flight.
        status_task.abort();
        let _ = status_task.await;
        // Guard Drop spawns async deregister; do not release bind (future is gone).
        drop(bind_release);

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !wait_cancel.contains(&stamp.wait_id).await {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("WaitCancelGuard must deregister after peer-close during bind");

        assert_eq!(
            wait_cancel
                .cancel(&stamp, crate::acp::tool_watchdog::CancelCause::AutoTimeout)
                .await,
            crate::acp::tool_watchdog::WaitCancelResult::NotFound,
            "abandoned bind must leave no ownerless wait registration"
        );
        assert_eq!(
            broker.pending_count().await,
            1,
            "peer-close during bind must not Broker-cancel children"
        );
    }

    /// Wait cancel after transfer while suspend is already in flight must end
    /// the MCP wait, clean the registry, leave children running, and still
    /// commit durable Waiting (control already sent — not pre-suspension Failed).
    #[tokio::test]
    async fn continuation_wait_cancel_after_suspend_control_preserves_waiting() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "cancel-vs-transfer-running")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        // Gate suspend so cancel races after Arming+transfer with control sent
        // but ACK still Pending.
        let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
        let (tokens, coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let listener =
            make_listener_with_wait_cancel(broker.clone(), tokens, Some(1), wait_cancel.clone());

        let status_task = tokio::spawn({
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                        parent_tool_use_id: "wait-tool-cancel-xfer".into(),
                    })
                    .await
            }
        });

        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(s) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("continuation join must register wait before arm handoff");

        let suspend = tokio::time::timeout(Duration::from_secs(2), suspend_entered)
            .await
            .expect("worker must reach suspend after Arming+transfer")
            .expect("suspend gate");
        assert_eq!(suspend.parent_connection_id, "parent-conn");
        assert_eq!(
            store.list_non_terminal().await.unwrap().len(),
            1,
            "Arming row must exist when cancel races the arm_task"
        );

        let pending_before = broker.pending_count().await;
        assert_eq!(
            wait_cancel
                .cancel(&stamp, crate::acp::tool_watchdog::CancelCause::AutoTimeout)
                .await,
            crate::acp::tool_watchdog::WaitCancelResult::Cancelled
        );

        let batch = tokio::time::timeout(Duration::from_secs(2), status_task)
            .await
            .expect("cancel after Arming must complete the wait without hanging")
            .expect("join status task")
            .expect("status ok")
            .batch;
        assert_eq!(
            batch.tasks[0].error_code.as_deref(),
            Some("tool_stalled_timeout")
        );

        // Release suspend: worker must commit Waiting (not pre-suspension Failed).
        let _ = suspend_release.send(());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = store.list_non_terminal().await.unwrap();
                if rows.iter().any(|row| {
                    row.state == ContinuationState::Waiting && row.suspended_at.is_some()
                }) {
                    break;
                }
                if rows
                    .iter()
                    .any(|row| row.state == ContinuationState::Failed)
                {
                    panic!("cancel after suspend control must not Failed-terminalize: {rows:?}");
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("post-control-sent cancel must reach durable Waiting");

        let non_terminal = store.list_non_terminal().await.unwrap();
        assert!(
            non_terminal.iter().any(|row| {
                row.state == ContinuationState::Waiting && row.suspended_at.is_some()
            }),
            "cancel after suspend control must leave resumable Waiting: {non_terminal:?}"
        );
        assert!(
            !wait_cancel.contains(&stamp.wait_id).await,
            "registry must be clean after cancel (MCP wait ends)"
        );
        assert_eq!(
            broker.pending_count().await,
            pending_before,
            "wait cancel must not Broker-cancel children"
        );
        assert!(
            store
                .load_active_for_conversation(1)
                .await
                .unwrap()
                .is_some(),
            "active Waiting continuation must remain for parent resume"
        );
        assert_eq!(
            coordinator.worker_count(),
            1,
            "Waiting worker must stay owned after post-control-sent wait-cancel"
        );
        assert_eq!(coordinator.cancel_workers_for_parent("parent-conn"), 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while coordinator.worker_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker must drain after parent cancel");
    }

    /// Peer-close in the residual window after `transfer_owner` and before
    /// `transfer_tx.send`: durable continuation must still suspend, and once
    /// `TransferredWait` is delivered it must deregister the wait registration
    /// (via waiter_closed) without Failed-orphaning the worker.
    #[tokio::test]
    async fn peer_close_between_transfer_owner_and_send_deregisters_keeps_continuation() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "peer-close-pre-send")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
        let (tokens, coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let (handoff_entered, handoff_release) = wait_cancel.install_transfer_handoff_gate().await;
        let listener =
            make_listener_with_wait_cancel(broker.clone(), tokens, Some(1), wait_cancel.clone());

        let status_task = tokio::spawn({
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                        parent_tool_use_id: "wait-tool-peer-pre-send".into(),
                    })
                    .await
            }
        });

        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(s) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("wait must register");

        tokio::time::timeout(Duration::from_secs(2), handoff_entered)
            .await
            .expect("arm must reach transfer_owner→send handoff gate")
            .expect("handoff entered");
        assert_eq!(
            wait_cancel.owner(&stamp.wait_id).await,
            Some(crate::acp::tool_watchdog::WaitOwner::ContinuationCoordinator),
            "transfer_owner must complete before handoff gate"
        );

        // Peer-close while still inside the residual transfer→send window
        // (drop_armed may still be true; send not yet completed).
        status_task.abort();
        let _ = status_task.await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;

        // Release handoff so transfer_tx.send can proceed and TransferredWait
        // can observe already-cancelled waiter_closed.
        let _ = handoff_release.send(());

        let suspend = tokio::time::timeout(Duration::from_secs(2), suspend_entered)
            .await
            .expect("worker must still suspend after pre-send peer-close")
            .expect("suspend gate");
        assert_eq!(suspend.parent_connection_id, "parent-conn");

        tokio::time::timeout(Duration::from_secs(2), async {
            while wait_cancel.contains(&stamp.wait_id).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("TransferredWait must deregister after peer-close + handoff send");

        let _ = suspend_release.send(());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = store.list_non_terminal().await.unwrap();
                if rows.iter().any(|row| {
                    row.state == ContinuationState::Waiting && row.suspended_at.is_some()
                }) {
                    break;
                }
                if rows
                    .iter()
                    .any(|row| row.state == ContinuationState::Failed)
                {
                    panic!(
                        "pre-send peer-close must not Failed-orphan after transfer_owner: {rows:?}"
                    );
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached worker must publish Waiting after pre-send peer-close");

        assert_eq!(broker.pending_count().await, 1);
        assert_eq!(coordinator.cancel_workers_for_parent("parent-conn"), 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while coordinator.worker_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker must drain after parent cancel");
    }

    /// Peer-close after successful transfer (before ACK) must deregister the
    /// wait registration via TransferredWait/waiter_closed while the durable
    /// continuation continues to Waiting.
    #[tokio::test]
    async fn peer_close_after_transfer_before_ack_deregisters_keeps_continuation() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "peer-close-after-xfer")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
        let (tokens, coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let listener =
            make_listener_with_wait_cancel(broker.clone(), tokens, Some(1), wait_cancel.clone());

        let status_task = tokio::spawn({
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                        parent_tool_use_id: "wait-tool-peer-xfer".into(),
                    })
                    .await
            }
        });

        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(s) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("wait must register");

        let suspend = tokio::time::timeout(Duration::from_secs(2), suspend_entered)
            .await
            .expect("suspend after transfer")
            .expect("suspend gate");
        assert_eq!(suspend.parent_connection_id, "parent-conn");
        assert_eq!(
            wait_cancel.owner(&stamp.wait_id).await,
            Some(crate::acp::tool_watchdog::WaitOwner::ContinuationCoordinator),
            "transfer must complete before suspend entry"
        );

        // Peer-close abandons process_status after transfer / before ACK.
        status_task.abort();
        let _ = status_task.await;

        tokio::time::timeout(Duration::from_secs(2), async {
            while wait_cancel.contains(&stamp.wait_id).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("peer-close after transfer must deregister wait registration");

        let _ = suspend_release.send(());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = store.list_non_terminal().await.unwrap();
                if rows.iter().any(|row| {
                    row.state == ContinuationState::Waiting && row.suspended_at.is_some()
                }) {
                    break;
                }
                if rows
                    .iter()
                    .any(|row| row.state == ContinuationState::Failed)
                {
                    panic!("peer-close after transfer must not Failed-orphan: {rows:?}");
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached worker must publish Waiting after peer-close");

        assert_eq!(
            broker.pending_count().await,
            1,
            "peer-close must not Broker-cancel children"
        );
        assert_eq!(coordinator.cancel_workers_for_parent("parent-conn"), 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while coordinator.worker_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker must drain");
    }

    /// Compatibility Join also registers before park and cancel completes wait only.
    #[tokio::test]
    async fn compat_join_registers_and_cancel_leaves_children() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "compat-join-running")
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register("tok".into(), continuation_token_entry(false))
            .await;
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();
        let listener =
            make_listener_with_wait_cancel(broker.clone(), tokens, Some(1), wait_cancel.clone());

        let status_task = tokio::spawn({
            let listener = listener.clone();
            let task_id = task_id.clone();
            async move {
                listener
                    .process_status(BrokerStatusRequest {
                        token: "tok".into(),
                        task_ids: vec![task_id],
                        wait_ms: Some(0),
                        return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                        parent_tool_use_id: "wait-tool-compat".into(),
                    })
                    .await
            }
        });

        let stamp = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(s) = wait_cancel.live_wait_stamps().await.into_iter().next() {
                    break s;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("compat join must register before park");
        assert!(wait_cancel.contains(&stamp.wait_id).await);
        assert_eq!(
            wait_cancel.live_task_ids(&stamp.wait_id).await,
            Some(vec![task_id.clone()])
        );

        let pending_before = broker.pending_count().await;
        assert_eq!(
            wait_cancel
                .cancel(&stamp, crate::acp::tool_watchdog::CancelCause::AutoTimeout)
                .await,
            crate::acp::tool_watchdog::WaitCancelResult::Cancelled
        );
        let batch = tokio::time::timeout(Duration::from_secs(2), status_task)
            .await
            .expect("cancel completes")
            .unwrap()
            .unwrap()
            .batch;
        assert_eq!(
            batch.tasks[0].error_code.as_deref(),
            Some("tool_stalled_timeout")
        );
        assert_eq!(broker.pending_count().await, pending_before);
    }

    /// resolve_wait_tool_id prefers request id over rewrite; blank keeps rewrite.
    /// Rewrite/fallback ids preserve original bytes (trim only rejects blank).
    #[test]
    fn resolve_wait_tool_id_request_over_rewrite() {
        let req = BrokerStatusRequest {
            token: "t".into(),
            task_ids: vec![],
            wait_ms: Some(0),
            return_when: None,
            parent_tool_use_id: "host-wait".into(),
        };
        assert_eq!(
            resolve_wait_tool_id(&req, Some("rewrite-id")).as_deref(),
            Some("host-wait")
        );
        let blank = BrokerStatusRequest {
            parent_tool_use_id: String::new(),
            ..req.clone()
        };
        assert_eq!(
            resolve_wait_tool_id(&blank, Some("rewrite-id")).as_deref(),
            Some("rewrite-id")
        );
        assert_eq!(resolve_wait_tool_id(&blank, None), None);

        // Padded rewrite/fallback: keep original bytes for lease-key alignment.
        let padded_rewrite = "  rewrite-status-padded  ";
        assert_eq!(
            resolve_wait_tool_id(&blank, Some(padded_rewrite)).as_deref(),
            Some(padded_rewrite),
            "rewrite id must not be trimmed; bind/renew use raw lease keys"
        );
        assert_ne!(
            resolve_wait_tool_id(&blank, Some(padded_rewrite))
                .as_deref()
                .unwrap(),
            padded_rewrite.trim(),
        );
        // Whitespace-only rewrite is blank → reject.
        assert_eq!(resolve_wait_tool_id(&blank, Some("   ")), None);
        // Whitespace-only request falls through; padded rewrite still preserved.
        let ws_only_req = BrokerStatusRequest {
            parent_tool_use_id: "   ".into(),
            ..req.clone()
        };
        assert_eq!(
            resolve_wait_tool_id(&ws_only_req, Some(padded_rewrite)).as_deref(),
            Some(padded_rewrite)
        );
        // Nonblank padded request still preferred over rewrite.
        let padded_host = BrokerStatusRequest {
            parent_tool_use_id: "  host-wait  ".into(),
            ..req
        };
        assert_eq!(
            resolve_wait_tool_id(&padded_host, Some(padded_rewrite)).as_deref(),
            Some("  host-wait  ")
        );
    }

    #[tokio::test]
    async fn continuation_status_peer_close_leaves_children_running() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "status-peer-close-running")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let (port, suspend_entered, suspend_release) = ContinuationTestPort::suspend_gated();
        let (tokens, _coordinator) = continuation_registry(broker.clone(), store.clone(), port);
        tokens
            .register("tok".into(), continuation_token_entry(true))
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec![task_id.clone()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                parent_tool_use_id: String::new(),
            }),
        )
        .await
        .unwrap();
        suspend_entered
            .await
            .expect("canonical Join must cross the durable row/worker boundary");

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("peer EOF must release the canonical status request")
            .unwrap()
            .unwrap();
        assert_eq!(broker.pending_count().await, 1);
        assert_eq!(store.list_non_terminal().await.unwrap().len(), 1);
        assert_eq!(
            broker
                .metrics()
                .snapshot()
                .wait_return_reasons
                .get("peer_closed"),
            Some(&1),
        );

        suspend_release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let rows = store.list_non_terminal().await.unwrap();
                if rows
                    .first()
                    .is_some_and(|row| row.state == ContinuationState::Waiting)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the independently owned worker must survive peer close");
        assert_eq!(broker.pending_count().await, 1);

        complete_running_task(&broker, &task_id).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !store.list_non_terminal().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the independent child terminal action must finish continuation");
        assert_eq!(broker.pending_count().await, 0);
    }

    /// Batch status over the listener: two tasks, one completed and one still
    /// running, return as a `{ tasks: [..] }` envelope with both reports in
    /// request order.
    #[tokio::test]
    async fn batch_status_over_listener_multi_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-1".into())).await;
        mock.queue_send(Ok(accepted(1, Utc::now()))).await;
        mock.queue_spawn(Ok("child-2".into())).await;
        mock.queue_send(Ok(accepted(2, Utc::now()))).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let start = |tool_use: &'static str| {
            let broker = broker.clone();
            async move {
                broker
                    .start_delegation(DelegationRequest {
                        parent_connection_id: "parent-conn".into(),
                        parent_conversation_id: 1,
                        parent_tool_use_id: tool_use.into(),
                        agent_type: AgentType::Codex,
                        profile_id: None,
                        task: "do x".into(),
                        working_dir: None,
                        requested_working_dir: None,
                        external_handle: None,
                        work_unit_key: None,
                        replaces_task_id: None,
                        replacement_reason: None,
                        correlation_id: None,
                        recovery_authorization_id: None,
                    })
                    .await
                    .task_id
                    .unwrap()
            }
        };
        let t1 = start("pt-1").await;
        let t2 = start("pt-2").await;
        broker
            .complete_call(
                &t1,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "first".into(),
                    child_conversation_id: 1,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 3,
                    token_usage: None,
                }),
            )
            .await;

        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![t1.clone(), t2.clone()],
            wait_ms: None,
            return_when: None,
            parent_tool_use_id: String::new(),
        });
        write_frame(&mut client, &status).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        let tasks = resp.outcome["tasks"].as_array().expect("tasks array");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(tasks[0]["task_id"], t1.as_str());
        assert_eq!(tasks[1]["status"], "running");
        assert_eq!(tasks[1]["task_id"], t2.as_str());
    }

    /// An invalid token over a batch status reports `Unknown` for EACH requested
    /// id (preserving order) rather than collapsing to a single report — so the
    /// companion can still render one row per task.
    #[tokio::test]
    async fn batch_status_invalid_token_returns_unknown_per_id() {
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            Arc::new(TokenRegistry::default()),
            Some(1),
        );
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "bad-token".into(),
            task_ids: vec!["a".into(), "b".into()],
            wait_ms: None,
            return_when: None,
            parent_tool_use_id: String::new(),
        });
        write_frame(&mut client, &status).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        let tasks = resp.outcome["tasks"].as_array().expect("tasks array");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["status"], "unknown");
        assert_eq!(tasks[0]["task_id"], "a");
        assert_eq!(tasks[1]["status"], "unknown");
        assert_eq!(tasks[1]["task_id"], "b");
    }

    /// `cancel_delegation` over the listener: a running task is canceled by id
    /// and reports `canceled`.
    #[tokio::test]
    async fn cancel_task_by_id_over_listener() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(accepted(7, Utc::now()))).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        // Start a task directly so we hold its id.
        let ack = broker
            .start_delegation(DelegationRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "pt-1".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "do x".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await;
        let task_id = ack.task_id.clone().unwrap();

        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let cancel = BrokerMessage::CancelTask(BrokerCancelTaskRequest {
            token: "tok".into(),
            task_id: task_id.clone(),
            reason: CancelDelegationReason::UserCancel,
        });
        write_frame(&mut client, &cancel).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["status"], "canceled");
        assert_eq!(broker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn cancel_task_timeout_reason_returns_guidance_without_canceling() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(accepted(7, Utc::now()))).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let ack = broker
            .start_delegation(DelegationRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "pt-1".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "do x".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await;
        let task_id = ack.task_id.clone().unwrap();

        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let cancel = BrokerMessage::CancelTask(BrokerCancelTaskRequest {
            token: "tok".into(),
            task_id,
            reason: CancelDelegationReason::Timeout,
        });
        write_frame(&mut client, &cancel).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["status"], "running");
        assert_eq!(
            resp.outcome["message"],
            crate::acp::delegation::types::TIMEOUT_CANCEL_GUIDANCE
        );
        assert_eq!(broker.pending_count().await, 1);
    }

    /// Explicit cancel counters are **authenticated request-attempt** metrics
    /// (after token validation, Timeout excluded), not "successful running→
    /// settling transition only". Successful terminal cancels are already
    /// counted separately via `record_terminal(Canceled)`. MCP tools/call
    /// cancel is a distinct counter at the same attempt boundary. An asymmetric
    /// "success-only explicit vs attempt MCP" contract would contradict the
    /// brief's separation of the two cancel surfaces and the terminal counter.
    #[tokio::test]
    async fn explicit_cancel_metrics_are_authenticated_request_attempts() {
        let metrics = Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default());
        let mock = Arc::new(MockSpawner::new());
        let broker = Arc::new(
            DelegationBroker::new(
                mock as Arc<dyn ConnectionSpawner>,
                Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_metrics(metrics.clone()),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;

        // 1) Unknown task + valid token still counts as explicit cancel request.
        let listener = make_listener(broker.clone(), tokens.clone(), Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(
            &mut client,
            &BrokerMessage::CancelTask(BrokerCancelTaskRequest {
                token: "tok".into(),
                task_id: "never-existed".into(),
                reason: CancelDelegationReason::UserCancel,
            }),
        )
        .await
        .unwrap();
        let _: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(
            metrics.snapshot().explicit_user_cancel_count,
            1,
            "authenticated cancel_delegation attempt counts even when task is unknown"
        );

        // 2) Timeout is non-canceling and must not increment.
        let listener = make_listener(broker.clone(), tokens.clone(), Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(
            &mut client,
            &BrokerMessage::CancelTask(BrokerCancelTaskRequest {
                token: "tok".into(),
                task_id: "never-existed".into(),
                reason: CancelDelegationReason::Timeout,
            }),
        )
        .await
        .unwrap();
        let _: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(
            metrics.snapshot().explicit_user_cancel_count,
            1,
            "Timeout must remain non-canceling for metrics"
        );

        // 3) Invalid token does not count.
        let listener = make_listener(broker.clone(), tokens.clone(), Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(
            &mut client,
            &BrokerMessage::CancelTask(BrokerCancelTaskRequest {
                token: "bad".into(),
                task_id: "x".into(),
                reason: CancelDelegationReason::UserCancel,
            }),
        )
        .await
        .unwrap();
        let _: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(
            metrics.snapshot().explicit_user_cancel_count,
            1,
            "invalid token must not count as explicit cancel"
        );

        // 4) MCP request cancel is a separate authenticated-attempt counter.
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(
            &mut client,
            &BrokerMessage::Cancel(BrokerCancelRequest {
                token: "tok".into(),
                external_handle: "no-such-handle".into(),
                reason: None,
            }),
        )
        .await
        .unwrap();
        let _: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(metrics.snapshot().mcp_request_cancel_count, 1);
        assert_eq!(
            metrics.snapshot().explicit_user_cancel_count,
            1,
            "MCP cancel must not bleed into explicit cancel counters"
        );
    }

    #[tokio::test]
    async fn cancel_message_routed_to_broker() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-cancel".into())).await;
        mock.queue_send(Ok(accepted(99, Utc::now()))).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));

        // Park a delegation call with a known external_handle.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move {
                let req = DelegationRequest {
                    parent_connection_id: "parent-conn".into(),
                    parent_conversation_id: 1,
                    parent_tool_use_id: "pt-cancel".into(),
                    agent_type: AgentType::Codex,
                    profile_id: None,
                    task: "do x".into(),
                    working_dir: None,
                    requested_working_dir: None,
                    external_handle: Some("h-1".into()),
                    work_unit_key: None,
                    replaces_task_id: None,
                    replacement_reason: None,
                    correlation_id: None,
                    recovery_authorization_id: None,
                };
                broker.handle_request(req).await
            })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Drive a cancel through the listener — listener should ack with
        // an empty BrokerResponse and the broker should drain the pending.
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });

        let cancel_msg = BrokerMessage::Cancel(BrokerCancelRequest {
            token: "tok".into(),
            external_handle: "h-1".into(),
            reason: Some("from test".into()),
        });
        write_frame(&mut client, &cancel_msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        assert!(resp.outcome.is_null(), "cancel ack must be null");
        server_task.await.unwrap();

        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn token_registry_revoke_and_revoke_by_parent() {
        let registry = TokenRegistry::default();
        registry
            .register("t1".into(), TokenEntry::legacy("p1", PathBuf::from("/tmp")))
            .await;
        registry
            .register("t2".into(), TokenEntry::legacy("p1", PathBuf::from("/tmp")))
            .await;
        registry
            .register("t3".into(), TokenEntry::legacy("p2", PathBuf::from("/tmp")))
            .await;

        registry.revoke("t1").await;
        assert!(registry.lookup("t1").await.is_none());
        assert!(registry.lookup("t2").await.is_some());

        registry.revoke_by_parent("p1").await;
        assert!(registry.lookup("t2").await.is_none());
        assert!(registry.lookup("t3").await.is_some());
    }

    // Sanity: spawn failure surfaces as spawn_failed when the listener path
    // is exercised. Exercises the full process() → broker.handle_request chain.
    #[tokio::test]
    async fn spawn_failure_surfaces_through_listener() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Err(SpawnerError::Spawn("agent missing".into())))
            .await;
        // `make_broker` already enables delegation; this call narrows the
        // depth limit (8 instead of the helper's default) without changing
        // the enable bit.
        let broker = make_broker(mock).await;
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 8,
                ..DelegationConfig::default()
            })
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_listener(broker, tokens, Some(1));

        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("spawn_failed"));
    }

    // --- check_user_feedback over the listener -----------------------------

    use crate::acp::feedback::PendingFeedback;

    fn pending(id: &str, text: &str) -> PendingFeedback {
        PendingFeedback {
            id: id.into(),
            text: text.into(),
            created_at: chrono::Utc::now(),
        }
    }

    /// The manager chunks each response via `bounded_feedback_batch`. The
    /// serialized `feedback_response` of any such chunk must stay under the
    /// transport cap (`MAX_FRAME_BYTES` = 16 MiB) so the companion's `read_frame`
    /// never rejects it after the listener committed delivery — for BOTH
    /// worst-case-escaping notes AND a flood of tiny notes (whose per-note JSON
    /// overhead, not text length, is what a naive text-only bound would miss).
    #[test]
    fn bounded_feedback_response_always_fits_a_transport_frame() {
        use crate::acp::delegation::transport::MAX_FRAME_BYTES;
        use crate::acp::feedback::{bounded_feedback_batch, MAX_FEEDBACK_RESPONSE_BYTES};

        // Worst-case escaping: many MAX_FEEDBACK_CHARS-sized control-char notes.
        let worst = "\u{0001}".repeat(4096);
        let big: Vec<PendingFeedback> = (0..5_000)
            .map(|i| pending(&format!("b{i}"), &worst))
            .collect();
        // A flood of tiny notes: little text, lots of per-note JSON overhead.
        let tiny: Vec<PendingFeedback> = (0..200_000)
            .map(|i| pending(&format!("t{i}"), "x"))
            .collect();

        for (label, set) in [("worst-case", big), ("tiny-flood", tiny)] {
            let total = set.len();
            let batch = bounded_feedback_batch(set, MAX_FEEDBACK_RESPONSE_BYTES);
            assert!(batch.len() < total, "{label}: batch must be chunked");
            let encoded = serde_json::to_vec(&feedback_response(&batch).unwrap()).unwrap();
            assert!(
                encoded.len() < MAX_FRAME_BYTES,
                "{label}: bounded response must fit a transport frame: {} >= {}",
                encoded.len(),
                MAX_FRAME_BYTES
            );
        }
    }

    /// A valid `check_user_feedback` returns the parent's notes in a
    /// `{ count, feedback: [..] }` envelope (lean text, no ids) scoped to the
    /// token's parent connection, and — crucially — commits them delivered ONLY
    /// after the response is written, with the exact note ids.
    #[tokio::test]
    async fn feedback_returns_notes_then_commits_after_write() {
        let feedback = Arc::new(StubFeedback::default());
        *feedback.items.lock().await = vec![
            pending("f1", "use the existing UserService"),
            pending("f2", "skip the migration"),
        ];
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_feedback_listener(tokens, feedback.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::Feedback(BrokerFeedbackRequest {
            token: "tok".into(),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["count"], 2);
        let notes = resp.outcome["feedback"].as_array().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["text"], "use the existing UserService");
        // The lean note shape carries no internal id...
        assert!(notes[0].get("id").is_none());
        // ...but the envelope carries `_commit_ids` for the companion to echo
        // back in a CommitFeedback after it delivers the result.
        let commit_ids = resp.outcome["_commit_ids"].as_array().unwrap();
        assert_eq!(commit_ids, &vec!["f1", "f2"]);
        // Read was scoped to the token's parent connection id.
        assert_eq!(
            feedback.read_conn.lock().await.as_deref(),
            Some("parent-conn")
        );
        // The Feedback arm is READ-ONLY — it does NOT commit (delivery is
        // committed later, by the companion's CommitFeedback).
        assert!(feedback.committed.lock().await.is_empty());
    }

    /// A valid `get_session_info` resolves the session by id and returns its
    /// metadata; the resolver is called with the requested id + max_messages.
    #[tokio::test]
    async fn session_info_valid_token_resolves_by_id() {
        let session_info = Arc::new(StubSessionInfo {
            known: std::collections::HashSet::from([42]),
            ..Default::default()
        });
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_session_listener(tokens, session_info.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::SessionInfo(BrokerSessionRequest {
            token: "tok".into(),
            session_id: 42,
            max_messages: Some(15),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["found"], true);
        assert_eq!(resp.outcome["session_id"], 42);
        assert_eq!(resp.outcome["title"], "session 42");
        // The resolver saw the id + the requested message budget.
        assert_eq!(session_info.calls.lock().await.as_slice(), &[(42, 15)]);
    }

    /// Accepted-policy coverage (deliberate single-tenant scope): a single valid
    /// token resolves ANY non-deleted session id — not only ids "referenced" in the
    /// prompt. Three unrelated ids all resolve through one token.
    #[tokio::test]
    async fn session_info_resolves_any_session_id_not_just_referenced() {
        let session_info = Arc::new(StubSessionInfo {
            known: std::collections::HashSet::from([7, 42, 1000]),
            ..Default::default()
        });
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_session_listener(tokens, session_info.clone());

        for id in [7, 42, 1000] {
            let (mut client, mut server) = duplex(8 * 1024);
            let l = listener.clone();
            let server_task = tokio::spawn(async move {
                l.serve_one(&mut server).await.unwrap();
            });
            let msg = BrokerMessage::SessionInfo(BrokerSessionRequest {
                token: "tok".into(),
                session_id: id,
                max_messages: Some(0),
            });
            write_frame(&mut client, &msg).await.unwrap();
            let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
            server_task.await.unwrap();
            assert_eq!(resp.outcome["found"], true, "id {id} should resolve");
            assert_eq!(resp.outcome["session_id"], id);
        }
    }

    /// An invalid token yields a `found:false` outcome WITHOUT touching the
    /// resolver (no leak of whether the session exists).
    #[tokio::test]
    async fn session_info_invalid_token_is_not_found_without_resolving() {
        let session_info = Arc::new(StubSessionInfo {
            known: std::collections::HashSet::from([42]),
            ..Default::default()
        });
        // No token registered.
        let tokens = Arc::new(TokenRegistry::default());
        let listener = make_session_listener(tokens, session_info.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::SessionInfo(BrokerSessionRequest {
            token: "bogus".into(),
            session_id: 42,
            max_messages: None,
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["found"], false);
        assert_eq!(resp.outcome["session_id"], 42);
        // The resolver was never consulted for an unauthenticated caller.
        assert!(session_info.calls.lock().await.is_empty());
    }

    /// `CommitFeedback` marks the named ids delivered, scoped (via the token) to
    /// the parent connection — the companion sends this only after it delivers.
    #[tokio::test]
    async fn commit_feedback_marks_delivered_scoped_to_parent() {
        let feedback = Arc::new(StubFeedback::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_feedback_listener(tokens, feedback.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::CommitFeedback(BrokerCommitFeedbackRequest {
            token: "tok".into(),
            ids: vec!["f1".into(), "f2".into()],
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert!(resp.outcome.is_null(), "commit ack is empty");

        let committed = feedback.committed.lock().await;
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].0, "parent-conn");
        assert_eq!(committed[0].1, vec!["f1".to_string(), "f2".to_string()]);
    }

    /// An invalid token on `CommitFeedback` is a silent no-op (no commit).
    #[tokio::test]
    async fn commit_feedback_invalid_token_is_noop() {
        let feedback = Arc::new(StubFeedback::default());
        let listener = make_feedback_listener(Arc::new(TokenRegistry::default()), feedback.clone());
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(
            &mut client,
            &BrokerMessage::CommitFeedback(BrokerCommitFeedbackRequest {
                token: "bad".into(),
                ids: vec!["f1".into()],
            }),
        )
        .await
        .unwrap();
        let _: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert!(feedback.committed.lock().await.is_empty());
    }

    /// An invalid token returns an empty `{ count: 0 }` envelope (no leak of
    /// whether any feedback exists), never reads the store, and commits nothing.
    #[tokio::test]
    async fn feedback_invalid_token_returns_empty() {
        let feedback = Arc::new(StubFeedback::default());
        *feedback.items.lock().await = vec![pending("f1", "should never be returned")];
        let tokens = Arc::new(TokenRegistry::default());
        let listener = make_feedback_listener(tokens, feedback.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::Feedback(BrokerFeedbackRequest {
            token: "bad-token".into(),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["count"], 0);
        assert!(resp.outcome["feedback"].as_array().unwrap().is_empty());
        // The store was never read or committed for an unknown token.
        assert!(feedback.read_conn.lock().await.is_none());
        assert!(feedback.committed.lock().await.is_empty());
    }

    // --- ask_user_question over the listener -------------------------------

    fn ask_msg(token: &str) -> BrokerMessage {
        BrokerMessage::Ask(BrokerAskRequest {
            token: token.into(),
            questions: vec![crate::acp::question::QuestionSpec {
                id: "qq-1".into(),
                question: "Which approach?".into(),
                header: "Approach".into(),
                multi_select: false,
                options: vec![
                    crate::acp::question::QuestionOption {
                        label: "Incremental".into(),
                        description: String::new(),
                    },
                    crate::acp::question::QuestionOption {
                        label: "Rewrite".into(),
                        description: String::new(),
                    },
                ],
                is_secret: false,
                recovery: None,
            }],
        })
    }

    use crate::acp::question::QuestionAnsweredItem;

    /// An `Ask` registers the question, parks, and — once the user answers —
    /// writes the `{ answers, declined }` envelope back over the same socket.
    #[tokio::test]
    async fn ask_registers_then_answer_resolves_response() {
        let questions = Arc::new(StubQuestion::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_question_listener(tokens, questions.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(&mut client, &ask_msg("tok")).await.unwrap();

        // The server must be parked until an answer arrives — no response yet.
        let early = tokio::time::timeout(Duration::from_millis(40), async {
            read_frame::<_, BrokerResponse>(&mut client).await
        })
        .await;
        assert!(early.is_err(), "ask must block until the user answers");

        // Wait for the stub to record the registration, then answer it.
        while questions.registered.lock().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(questions.registered.lock().await[0].0, "parent-conn");
        questions
            .answer(
                "q-1",
                QuestionOutcome {
                    answers: vec![QuestionAnsweredItem {
                        question: "Which approach?".into(),
                        header: "Approach".into(),
                        multi_select: false,
                        selected: vec!["Incremental".into()],
                    }],
                    declined: false,
                },
            )
            .await;

        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["declined"], false);
        assert_eq!(resp.outcome["answers"][0]["selected"][0], "Incremental");
        assert_eq!(resp.outcome["answers"][0]["header"], "Approach");
    }

    /// A canceled tool call drops the request socket; the listener observes the
    /// peer-close, cancels the pending question, and returns without writing.
    #[tokio::test]
    async fn ask_peer_close_cancels_question() {
        let questions = Arc::new(StubQuestion::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry::legacy("parent-conn", PathBuf::from("/tmp")),
            )
            .await;
        let listener = make_question_listener(tokens, questions.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });
        write_frame(&mut client, &ask_msg("tok")).await.unwrap();

        // Let the server park inside the wait.
        while questions.registered.lock().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Companion cancels: drop the request socket.
        drop(client);

        let result = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("serve_one must return after peer close");
        result.unwrap().unwrap();
        assert_eq!(
            questions.canceled.lock().await.as_slice(),
            &["q-1".to_string()]
        );
    }

    /// An invalid token never registers a question and returns a `declined`
    /// outcome (the LLM proceeds with its own judgment).
    #[tokio::test]
    async fn ask_invalid_token_declined() {
        let questions = Arc::new(StubQuestion::default());
        let listener =
            make_question_listener(Arc::new(TokenRegistry::default()), questions.clone());
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(&mut client, &ask_msg("bad-token"))
            .await
            .unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["declined"], true);
        assert!(questions.registered.lock().await.is_empty());
    }

    // ─── Task 7: ready-lease wire protocol (duplex / serve_one) ───────────

    fn make_ready_lease_listener(
        tokens: Arc<TokenRegistry>,
        leases: Arc<CompanionLeaseRegistry>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            leases,
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
        )
    }

    /// Valid token: Ready → ack → hold → peer EOF → closed exactly once.
    #[tokio::test]
    async fn ready_lease_wire_valid_token_acks_hold_then_closed_once_on_eof() {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, CompanionReadyAck, CompanionReadyRequest,
        };
        use std::time::Duration;

        let tokens = Arc::new(TokenRegistry::default());
        let leases = Arc::new(CompanionLeaseRegistry::default());
        tokens
            .register(
                "ready-tok".into(),
                TokenEntry::legacy("parent", PathBuf::from("/tmp")),
            )
            .await;
        let mut waiter = leases.register("ready-tok").await;
        let listener = make_ready_lease_listener(tokens, Arc::clone(&leases));

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });

        write_frame(
            &mut client,
            &BrokerMessage::Ready(CompanionReadyRequest {
                token: "ready-tok".into(),
            }),
        )
        .await
        .unwrap();
        let ack: CompanionReadyAck = read_frame(&mut client).await.unwrap();
        assert!(ack.ready);
        waiter
            .wait_ready(Duration::from_millis(200))
            .await
            .expect("host must observe ready after authenticated ack");
        assert!(*waiter.availability().borrow());

        // Peer EOF ends the hold; closed exactly once.
        drop(client);
        server_task.await.unwrap();
        // Availability may already be false (mark_closed ran); if still true, wait once.
        if *waiter.availability().borrow() {
            waiter.availability().changed().await.unwrap();
        }
        assert!(!*waiter.availability().borrow());
        // Second close is a no-op (idempotent).
        leases.mark_closed("ready-tok").await;
        assert!(!*waiter.availability().borrow());
    }

    /// Second Ready on an already-ready token acks without closing the primary
    /// hold (CLI exec re-spawn after session-open prewarm).
    #[tokio::test]
    async fn ready_lease_wire_secondary_already_ready_does_not_close_primary() {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, CompanionReadyAck, CompanionReadyRequest,
        };
        use std::time::Duration;

        let tokens = Arc::new(TokenRegistry::default());
        let leases = Arc::new(CompanionLeaseRegistry::default());
        tokens
            .register(
                "dup-tok".into(),
                TokenEntry::legacy("parent", PathBuf::from("/tmp")),
            )
            .await;
        let mut waiter = leases.register("dup-tok").await;
        let listener_primary = make_ready_lease_listener(tokens.clone(), Arc::clone(&leases));
        let listener_secondary = make_ready_lease_listener(tokens, Arc::clone(&leases));

        let (mut primary_client, mut primary_server) = duplex(8 * 1024);
        let primary_task = tokio::spawn(async move {
            listener_primary
                .serve_one(&mut primary_server)
                .await
                .unwrap();
        });

        write_frame(
            &mut primary_client,
            &BrokerMessage::Ready(CompanionReadyRequest {
                token: "dup-tok".into(),
            }),
        )
        .await
        .unwrap();
        let ack: CompanionReadyAck = read_frame(&mut primary_client).await.unwrap();
        assert!(ack.ready);
        waiter
            .wait_ready(Duration::from_millis(200))
            .await
            .expect("primary ready");
        assert!(*waiter.availability().borrow());

        let (mut secondary_client, mut secondary_server) = duplex(8 * 1024);
        let secondary_task = tokio::spawn(async move {
            listener_secondary
                .serve_one(&mut secondary_server)
                .await
                .unwrap();
        });

        write_frame(
            &mut secondary_client,
            &BrokerMessage::Ready(CompanionReadyRequest {
                token: "dup-tok".into(),
            }),
        )
        .await
        .unwrap();
        let secondary_ack: CompanionReadyAck = read_frame(&mut secondary_client).await.unwrap();
        assert!(secondary_ack.ready);
        // Secondary ends without exclusive hold; connection may close after ack.
        let _ = secondary_task.await;
        drop(secondary_client);

        // Primary hold still live — availability must stay true.
        assert!(
            *waiter.availability().borrow(),
            "secondary AlreadyReady must not mark_closed the primary lease"
        );

        drop(primary_client);
        primary_task.await.unwrap();
        if *waiter.availability().borrow() {
            waiter.availability().changed().await.unwrap();
        }
        assert!(!*waiter.availability().borrow());
    }

    /// Valid token held, then revoke → closed once; host never sees re-ready.
    #[tokio::test]
    async fn ready_lease_wire_revoke_while_held_closes_once() {
        use crate::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, CompanionReadyAck, CompanionReadyRequest,
        };
        use std::time::Duration;

        let tokens = Arc::new(TokenRegistry::default());
        let leases = Arc::new(CompanionLeaseRegistry::default());
        tokens
            .register(
                "revoke-tok".into(),
                TokenEntry::legacy("parent", PathBuf::from("/tmp")),
            )
            .await;
        let mut waiter = leases.register("revoke-tok").await;
        let listener = make_ready_lease_listener(tokens, Arc::clone(&leases));

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });

        write_frame(
            &mut client,
            &BrokerMessage::Ready(CompanionReadyRequest {
                token: "revoke-tok".into(),
            }),
        )
        .await
        .unwrap();
        let ack: CompanionReadyAck = read_frame(&mut client).await.unwrap();
        assert!(ack.ready);
        waiter.wait_ready(Duration::from_millis(200)).await.unwrap();

        leases.revoke("revoke-tok").await;
        server_task.await.unwrap();
        assert!(!*waiter.availability().borrow());
        // Keep client open until server exits so revoke path is exercised.
        drop(client);
    }

    /// Invalid token never becomes ready on the registry watch.
    #[tokio::test]
    async fn ready_lease_wire_invalid_token_never_ready() {
        use crate::acp::delegation::transport::{
            write_frame, BrokerMessage, CompanionReadyRequest,
        };
        use std::time::Duration;

        let tokens = Arc::new(TokenRegistry::default());
        let leases = Arc::new(CompanionLeaseRegistry::default());
        // Register a different token so the lease slot exists for a good token only.
        let mut good_waiter = leases.register("good-tok").await;
        let mut bad_slot = leases.register("bad-tok").await;
        // Only "good-tok" is in the token registry.
        tokens
            .register(
                "good-tok".into(),
                TokenEntry::legacy("parent", PathBuf::from("/tmp")),
            )
            .await;

        let listener = make_ready_lease_listener(tokens, Arc::clone(&leases));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        write_frame(
            &mut client,
            &BrokerMessage::Ready(CompanionReadyRequest {
                token: "bad-tok".into(),
            }),
        )
        .await
        .unwrap();
        let serve_result = server_task.await.unwrap();
        assert!(serve_result.is_err(), "invalid token must fail serve_one");
        // Neither slot becomes ready.
        assert!(good_waiter
            .wait_ready(Duration::from_millis(30))
            .await
            .is_err());
        assert!(bad_slot
            .wait_ready(Duration::from_millis(30))
            .await
            .is_err());
        assert!(!*good_waiter.availability().borrow());
        assert!(!*bad_slot.availability().borrow());
        drop(client);
    }

    /// Scripted ack write failure must leave the host waiter unable to become
    /// Ready (not only availability=false). Valid path covered separately.
    #[tokio::test]
    async fn ready_lease_ack_write_failure_never_ready() {
        use crate::acp::delegation::transport::{
            write_frame, BrokerMessage, CompanionReadyRequest,
        };
        use std::io::Cursor;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

        /// Readable Ready frame, then fail the first write (ack).
        struct ReadyThenFailAckWrite {
            read_buf: Cursor<Vec<u8>>,
            wrote: bool,
        }

        impl AsyncRead for ReadyThenFailAckWrite {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                let mut tmp = vec![0u8; buf.remaining()];
                match std::io::Read::read(&mut self.read_buf, &mut tmp) {
                    Ok(0) => Poll::Ready(Ok(())),
                    Ok(n) => {
                        buf.put_slice(&tmp[..n]);
                        Poll::Ready(Ok(()))
                    }
                    Err(e) => Poll::Ready(Err(e)),
                }
            }
        }

        impl AsyncWrite for ReadyThenFailAckWrite {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                if !self.wrote {
                    self.wrote = true;
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "injected ack write failure",
                    )));
                }
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "already failed",
                )))
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        // Encode a real Ready frame into the scripted stream's read buffer.
        let mut encode = Vec::new();
        {
            use tokio::io::AsyncWriteExt;
            let (mut w, mut r) = duplex(4 * 1024);
            write_frame(
                &mut w,
                &BrokerMessage::Ready(CompanionReadyRequest {
                    token: "fail-ack".into(),
                }),
            )
            .await
            .unwrap();
            w.shutdown().await.unwrap();
            tokio::io::AsyncReadExt::read_to_end(&mut r, &mut encode)
                .await
                .unwrap();
        }

        let tokens = Arc::new(TokenRegistry::default());
        let leases = Arc::new(CompanionLeaseRegistry::default());
        tokens
            .register(
                "fail-ack".into(),
                TokenEntry::legacy("parent", PathBuf::from("/tmp")),
            )
            .await;
        let mut waiter = leases.register("fail-ack").await;
        let listener = make_ready_lease_listener(tokens, Arc::clone(&leases));

        let mut conn = ReadyThenFailAckWrite {
            read_buf: Cursor::new(encode),
            wrote: false,
        };
        let result = listener.serve_one(&mut conn).await;
        assert!(
            result.is_err(),
            "ack write failure must surface via serve_one"
        );

        // Host bootstrap must not observe Ready (Connected / RouteBootstrap Ready
        // depend on wait_ready succeeding). Fail closed — not merely availability.
        let wait = waiter
            .wait_ready(std::time::Duration::from_millis(80))
            .await;
        assert!(
            wait.is_err(),
            "ack write failure must not make wait_ready Ready; got {wait:?}"
        );
        assert!(
            !*waiter.availability().borrow(),
            "ack write failure must leave availability false"
        );
        // A later mark_ready must not resurrect a failed handshake slot if revoked.
        assert!(
            leases.mark_ready("fail-ack").await.is_err(),
            "failed ack path should revoke/forget lease so host cannot mark ready later"
        );
    }

    // -- Role-aware parent decision tools (Task 6) -------------------------

    use crate::acp::delegation::store::mock::MockTaskStore;
    use crate::acp::delegation::store::DelegationTaskStore;
    use crate::acp::delegation::transport::{
        BrokerParentDecisionRequest, BrokerReplyDelegationRequest,
    };
    use crate::acp::delegation::types::{DelegationReplyResult, ParentDecisionResult};
    use std::collections::HashMap as StdHashMap;

    struct MapParentLookup(StdHashMap<String, i32>);
    #[async_trait]
    impl ParentSessionLookup for MapParentLookup {
        async fn current_conversation_id(&self, parent_connection_id: &str) -> Option<i32> {
            self.0.get(parent_connection_id).copied()
        }
    }

    fn child_token_entry(conn: &str) -> TokenEntry {
        TokenEntry {
            parent_connection_id: conn.to_string(),
            working_dir: PathBuf::from("/tmp"),
            coordination_v1: true,
            delegation_continuation_v1: false,
            role: CompanionRole::DelegationChild,
            workflow_v2: false,
            completion_v2: false,
            bound_task_id: None,
        }
    }

    fn root_token_entry(conn: &str) -> TokenEntry {
        TokenEntry {
            parent_connection_id: conn.to_string(),
            working_dir: PathBuf::from("/tmp"),
            coordination_v1: true,
            delegation_continuation_v1: false,
            role: CompanionRole::Root,
            workflow_v2: false,
            completion_v2: false,
            bound_task_id: None,
        }
    }

    fn child_workflow_token_entry(conn: &str) -> TokenEntry {
        TokenEntry {
            parent_connection_id: conn.to_string(),
            working_dir: PathBuf::from("/tmp"),
            coordination_v1: false,
            delegation_continuation_v1: false,
            role: CompanionRole::DelegationChild,
            // Feature bit set but role is child — must still hard-deny.
            workflow_v2: true,
            completion_v2: false,
            bound_task_id: None,
        }
    }

    mod complete_work_contract {
        use super::*;
        use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
        use crate::acp::delegation::transport::BrokerCompleteWorkRequest;
        use crate::acp::delegation::workflow::{
            accept_complete_work_txn_with_test_control, load_completion_projection,
            load_workflow_child_mcp_binding, AcceptedToolIntent, CompleteWorkError,
            CompleteWorkRequest, CompleteWorkTestControl, CompletionOutcome,
        };
        use crate::db::entities::delegation_task_run::{
            self, AdmissionClass, CompletionState, DelegationRunStatus,
        };
        use crate::db::entities::delegation_workflow::{
            self, CompletionProtocolMode, WorkflowState,
        };
        use crate::db::entities::delegation_workflow_node_binding;
        use crate::db::entities::delegation_workflow_run_binding;
        use crate::db::entities::{
            delegation_attention_request, delegation_completion_tool_intent,
        };
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
        use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, QueryOrder, Set};

        const TASK_ID: &str = "completion-tool-task";
        const WORKFLOW_ID: &str = "completion-tool-workflow";
        const NODE_ID: &str = "completion-tool-reviewer";
        const CHILD_CONNECTION_ID: &str = "completion-tool-child-connection";

        struct CompletionToolFixture {
            db: Arc<crate::db::AppDatabase>,
            listener: Arc<DelegationListener>,
        }

        impl CompletionToolFixture {
            fn root_token(&self) -> &'static str {
                "completion-root"
            }

            fn v1_child_token(&self) -> &'static str {
                "completion-v1-child"
            }

            fn unbound_child_token(&self) -> &'static str {
                "completion-unbound-child"
            }

            fn v2_child_token(&self) -> &'static str {
                "completion-v2-child"
            }

            async fn complete(
                &self,
                token: &str,
                child_tool_call_id: &str,
                request: CompleteWorkRequest,
            ) -> Value {
                self.listener
                    .process_complete_work(BrokerCompleteWorkRequest {
                        token: token.to_string(),
                        child_tool_call_id: child_tool_call_id.to_string(),
                        request,
                    })
                    .await
            }

            async fn run(&self) -> crate::db::entities::delegation_task_run::Model {
                crate::db::entities::delegation_task_run::Entity::find_by_id(TASK_ID)
                    .one(&self.db.conn)
                    .await
                    .unwrap()
                    .unwrap()
            }

            async fn latest_tool_intent(&self) -> delegation_completion_tool_intent::Model {
                delegation_completion_tool_intent::Entity::find()
                    .order_by_desc(delegation_completion_tool_intent::Column::AcceptedOrdinal)
                    .one(&self.db.conn)
                    .await
                    .unwrap()
                    .unwrap()
            }
        }

        fn approve() -> CompleteWorkRequest {
            CompleteWorkRequest {
                outcome: CompletionOutcome::Approve,
                summary: Some("ready".into()),
                report_file: Some("reports/review.md".into()),
            }
        }

        fn request_changes() -> CompleteWorkRequest {
            CompleteWorkRequest {
                outcome: CompletionOutcome::RequestChanges,
                summary: Some("one blocking issue".into()),
                report_file: None,
            }
        }

        fn response_code(response: &Value) -> Option<&str> {
            response.pointer("/error/code").and_then(Value::as_str)
        }

        fn accepted(response: Value) -> AcceptedToolIntent {
            serde_json::from_value(response).expect("accepted completion intent")
        }

        async fn completion_tool_fixture_with_db(
            db: Arc<crate::db::AppDatabase>,
        ) -> CompletionToolFixture {
            let folder = seed_folder(&db, "/tmp/completion-tool-contract").await;
            let parent = seed_conversation(&db, folder, AgentType::Codex).await;
            let child = seed_conversation(&db, folder, AgentType::Codex).await;
            let runs = Arc::new(RunStore::new(Arc::clone(&db)));
            runs.insert_reserving(ReservingRunInsert {
                task_id: TASK_ID.into(),
                root_task_id: TASK_ID.into(),
                previous_task_id: None,
                generation: 1,
                parent_conversation_id: parent,
                parent_tool_use_id: Some("parent-tool".into()),
                child_conversation_id: child,
                agent_type: "codex".into(),
                profile_id: None,
                workspace_path: Some("/tmp/completion-tool-contract".into()),
                route_fingerprint: Some("aabbccdd".into()),
                launch_snapshot_version: Some("v1".into()),
                mode_id: None,
                config_values_json: Some("{}".into()),
                task_preview: Some("review the task".into()),
                request_fingerprint: Some("a".repeat(64)),
                admission_class: AdmissionClass::NormalRevision,
                lineage_root_task_id: TASK_ID.into(),
                work_unit_key: None,
                history_only: false,
                replaced_task_id: None,
                replacement_reason: None,
                started_at: Some(Utc::now()),
            })
            .await
            .unwrap();
            runs.bind_child_connection_while_reserving(TASK_ID, CHILD_CONNECTION_ID)
                .await
                .unwrap();
            runs.promote_running(TASK_ID, CHILD_CONNECTION_ID, Utc::now())
                .await
                .unwrap();

            let now = Utc::now();
            delegation_workflow::ActiveModel {
                workflow_id: Set(WORKFLOW_ID.into()),
                parent_conversation_id: Set(parent),
                workflow_kind: Set("brainstorm_to_delivery".into()),
                schema_version: Set(2),
                active_manifest_revision: Set(1),
                graph_revision: Set(1),
                workflow_state: Set(WorkflowState::Estimated),
                capability_version: Set("workflow_manifest_v2".into()),
                publication_token: Set("completion-tool-publication".into()),
                supersedes_approved_revision: Set(None),
                structural_revision: Set(1),
                design_fingerprint: Set("design".into()),
                plan_fingerprint: Set("plan".into()),
                block_cause_code: Set(None),
                block_source_manifest_revision: Set(None),
                completion_protocol_version: Set(2),
                completion_protocol_mode: Set(CompletionProtocolMode::V2Enforce),
                legacy_source_workflow_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&db.conn)
            .await
            .unwrap();
            delegation_workflow_node_binding::ActiveModel {
                workflow_id: Set(WORKFLOW_ID.into()),
                node_id: Set(NODE_ID.into()),
                work_unit_key: Set("task|6|reviewer|codex|none".into()),
                role: Set("reviewer".into()),
                agent_type: Set("codex".into()),
                profile_id: Set(None),
                phase_id: Set("tasks".into()),
                task_index: Set(Some(6)),
                introduced_revision: Set(1),
                retired_revision: Set(None),
                is_observed: Set(false),
                retained_observed: Set(false),
                cohort_frozen: Set(false),
                node_outcome: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&db.conn)
            .await
            .unwrap();
            delegation_workflow_run_binding::ActiveModel {
                task_id: Set(TASK_ID.into()),
                workflow_id: Set(WORKFLOW_ID.into()),
                node_id: Set(NODE_ID.into()),
                gate_id: Set(None),
                gate_cycle: Set(None),
                manifest_revision: Set(1),
                content_fingerprint: Set(None),
                evidence_scope_digest: Set(None),
                gate_lineage: Set(None),
                review_round: Set(None),
                instruction_block_digest: Set(None),
                material_selector_digest: Set(None),
                subject_material_digest: Set(None),
                requirements_identity: Set(None),
                task_specification_identity: Set(None),
                final_findings_identity: Set(None),
                producer_baseline_head: Set(None),
                artifact_digest: Set(None),
                reviewed_task_id: Set(None),
                reviewed_implementer_generation: Set(None),
                lineage_ordinal: Set(1),
                summary_validated: Set(false),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&db.conn)
            .await
            .unwrap();

            let broker = Arc::new(
                DelegationBroker::new(
                    Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
                    Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
                )
                .with_run_store(Arc::clone(&runs)),
            );
            let tokens = Arc::new(TokenRegistry::default());
            for (token, role, completion_v2, bound_task_id, connection_id) in [
                (
                    "completion-root",
                    CompanionRole::Root,
                    true,
                    Some(TASK_ID),
                    CHILD_CONNECTION_ID,
                ),
                (
                    "completion-v1-child",
                    CompanionRole::DelegationChild,
                    false,
                    Some(TASK_ID),
                    CHILD_CONNECTION_ID,
                ),
                (
                    "completion-unbound-child",
                    CompanionRole::DelegationChild,
                    true,
                    None,
                    CHILD_CONNECTION_ID,
                ),
                (
                    "completion-v2-child",
                    CompanionRole::DelegationChild,
                    true,
                    Some(TASK_ID),
                    CHILD_CONNECTION_ID,
                ),
            ] {
                tokens
                    .register(
                        token.into(),
                        TokenEntry {
                            parent_connection_id: connection_id.into(),
                            working_dir: PathBuf::from("/tmp/completion-tool-contract"),
                            coordination_v1: true,
                            delegation_continuation_v1: false,
                            role,
                            workflow_v2: false,
                            completion_v2,
                            bound_task_id: bound_task_id.map(str::to_string),
                        },
                    )
                    .await;
            }
            let listener = make_listener(broker, tokens, Some(parent));
            CompletionToolFixture { db, listener }
        }

        async fn completion_tool_fixture() -> CompletionToolFixture {
            completion_tool_fixture_with_db(Arc::new(fresh_in_memory_db().await)).await
        }

        #[tokio::test]
        async fn broker_accepts_only_live_bound_v2_child_and_orders_distinct_calls() {
            let fixture = completion_tool_fixture().await;
            for caller in [
                fixture.root_token(),
                fixture.v1_child_token(),
                fixture.unbound_child_token(),
            ] {
                let response = fixture.complete(caller, "call-1", approve()).await;
                assert_eq!(
                    response_code(&response),
                    Some("completion_tool_unauthorized")
                );
            }

            let first = accepted(
                fixture
                    .complete(fixture.v2_child_token(), "call-1", approve())
                    .await,
            );
            let replay = accepted(
                fixture
                    .complete(fixture.v2_child_token(), "call-1", approve())
                    .await,
            );
            assert_eq!(first.intent_id, replay.intent_id);
            assert_eq!(first.accepted_ordinal, 1);

            let conflict = fixture
                .complete(fixture.v2_child_token(), "call-1", request_changes())
                .await;
            assert_eq!(
                response_code(&conflict),
                Some("completion_tool_call_conflict")
            );
            let second = accepted(
                fixture
                    .complete(fixture.v2_child_token(), "call-2", request_changes())
                    .await,
            );
            assert_eq!(second.accepted_ordinal, 2);
            assert_eq!(
                fixture.latest_tool_intent().await.outcome,
                CompletionOutcome::RequestChanges.as_str()
            );
        }

        #[tokio::test]
        async fn complete_work_records_intent_without_terminating_the_run() {
            let fixture = completion_tool_fixture().await;
            accepted(
                fixture
                    .complete(fixture.v2_child_token(), "call-1", approve())
                    .await,
            );
            let run = fixture.run().await;
            assert_eq!(run.status, DelegationRunStatus::Running);
            assert_eq!(run.completion_evidence_json, None);
        }

        #[tokio::test]
        async fn listener_report_status_and_mcp_render_share_the_durable_completion_projection() {
            let fixture = completion_tool_fixture().await;
            let run = fixture.run().await;
            let parent_conversation_id = run.parent_conversation_id;
            let mut run: delegation_task_run::ActiveModel = run.into();
            run.status = Set(DelegationRunStatus::Completed);
            run.finished_at = Set(Some(Utc::now()));
            run.completion_state = Set(Some(CompletionState::NeedsDecision));
            run.update(&fixture.db.conn).await.unwrap();
            let binding = delegation_workflow_run_binding::Entity::find_by_id(TASK_ID)
                .one(&fixture.db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut binding: delegation_workflow_run_binding::ActiveModel = binding.into();
            binding.evidence_scope_digest = Set(Some(format!("sha256:{}", "e".repeat(64))));
            binding.update(&fixture.db.conn).await.unwrap();
            delegation_attention_request::ActiveModel {
                request_id: Set("listener-projection-attention".into()),
                task_id: Set(TASK_ID.into()),
                parent_conversation_id: Set(parent_conversation_id),
                child_conversation_id: Set(None),
                child_tool_call_id: Set(None),
                status: Set("open".into()),
                message: Set("Choose the reviewer outcome.".into()),
                reply: Set(None),
                resolution_code: Set(None),
                created_at: Set(Utc::now()),
                resolved_at: Set(None),
                kind: Set(delegation_attention_request::AttentionKind::CompletionDecision),
                latest_run_id: Set(Some(TASK_ID.into())),
                node_id: Set(Some(NODE_ID.into())),
                payload_json: Set(Some(
                    serde_json::json!({
                        "version": 1,
                        "reason_code": "completion_intent_missing",
                        "role": "reviewer",
                        "legal_outcomes": [
                            "approve",
                            "approve_with_minors",
                            "request_changes",
                            "block"
                        ],
                        "bounded_candidates": [],
                        "diagnostics": []
                    })
                    .to_string(),
                )),
                resolution_json: Set(None),
                captured_scope_digest: Set(Some(format!("sha256:{}", "e".repeat(64)))),
            }
            .insert(&fixture.db.conn)
            .await
            .unwrap();

            let expected = serde_json::to_value(
                load_completion_projection(&fixture.db.conn, TASK_ID)
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let report = DelegationTaskReport {
                task_id: Some(TASK_ID.into()),
                continued_from_task_id: None,
                reused_session: None,
                status: TaskStatus::Completed,
                child_conversation_id: None,
                agent_type: None,
                text: Some("done".into()),
                error_code: None,
                message: None,
                duration_ms: Some(1),
                observation: None,
                last_agent_activity_at: None,
                stalled_since: None,
                recovery: None,
            };

            let report_response = fixture
                .listener
                .report_response(report.clone())
                .await
                .unwrap();
            assert_eq!(report_response.outcome["completion"], expected);

            let status_response = fixture
                .listener
                .status_response(DelegationStatusBatch::legacy(vec![report]))
                .await
                .unwrap();
            assert_eq!(status_response.outcome["tasks"][0]["completion"], expected);

            let rendered =
                crate::acp::delegation::companion::render_status_result(&status_response.outcome);
            assert_eq!(
                rendered["structuredContent"]["tasks"][0]["completion"],
                expected
            );
        }

        #[tokio::test]
        async fn listener_report_status_and_mcp_share_completion_projection_corruption() {
            let fixture = completion_tool_fixture().await;
            let run = fixture.run().await;
            let mut run: delegation_task_run::ActiveModel = run.into();
            run.status = Set(DelegationRunStatus::Completed);
            run.finished_at = Set(Some(Utc::now()));
            run.completion_state = Set(Some(CompletionState::NeedsDecision));
            run.update(&fixture.db.conn).await.unwrap();

            let binding = delegation_workflow_run_binding::Entity::find_by_id(TASK_ID)
                .one(&fixture.db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut binding: delegation_workflow_run_binding::ActiveModel = binding.into();
            binding.evidence_scope_digest = Set(Some(format!("sha256:{}", "e".repeat(64))));
            binding.update(&fixture.db.conn).await.unwrap();

            delegation_attention_request::ActiveModel {
                request_id: Set("listener-corrupt-projection".into()),
                task_id: Set(TASK_ID.into()),
                parent_conversation_id: Set(fixture.run().await.parent_conversation_id),
                child_conversation_id: Set(None),
                child_tool_call_id: Set(None),
                status: Set("open".into()),
                message: Set("Choose the reviewer outcome.".into()),
                reply: Set(None),
                resolution_code: Set(None),
                created_at: Set(Utc::now()),
                resolved_at: Set(None),
                kind: Set(delegation_attention_request::AttentionKind::CompletionDecision),
                latest_run_id: Set(Some(TASK_ID.into())),
                node_id: Set(Some(NODE_ID.into())),
                payload_json: Set(Some("{not-json".into())),
                resolution_json: Set(None),
                captured_scope_digest: Set(Some(format!("sha256:{}", "e".repeat(64)))),
            }
            .insert(&fixture.db.conn)
            .await
            .unwrap();

            let report = DelegationTaskReport {
                task_id: Some(TASK_ID.into()),
                continued_from_task_id: None,
                reused_session: None,
                status: TaskStatus::Completed,
                child_conversation_id: None,
                agent_type: None,
                text: Some("done".into()),
                error_code: None,
                message: None,
                duration_ms: Some(1),
                observation: None,
                last_agent_activity_at: None,
                stalled_since: None,
                recovery: None,
            };

            let report_response = fixture
                .listener
                .report_response(report.clone())
                .await
                .unwrap();
            let status_response = fixture
                .listener
                .status_response(DelegationStatusBatch::legacy(vec![report]))
                .await
                .unwrap();
            let report_rendered =
                crate::acp::delegation::companion::render_task_report(&report_response.outcome);
            let rendered =
                crate::acp::delegation::companion::render_status_result(&status_response.outcome);

            for projected in [
                &report_response.outcome,
                &status_response.outcome["tasks"][0],
                &report_rendered["structuredContent"],
                &rendered["structuredContent"]["tasks"][0],
            ] {
                assert_eq!(projected["status"], "failed");
                assert_eq!(projected["error_code"], "completion_terminal_state_invalid");
                assert!(projected["message"]
                    .as_str()
                    .unwrap()
                    .contains("typed completion attention is corrupt"));
                assert!(projected.get("completion").is_none());
            }
            assert_eq!(report_rendered["isError"], true);
            assert_eq!(rendered["isError"], true);
        }

        #[tokio::test]
        async fn complete_work_rejects_over_byte_payload_without_superseding() {
            let fixture = completion_tool_fixture().await;
            let first = accepted(
                fixture
                    .complete(fixture.v2_child_token(), "call-1", approve())
                    .await,
            );

            let invalid = fixture
                .complete(
                    fixture.v2_child_token(),
                    "call-2",
                    CompleteWorkRequest {
                        outcome: CompletionOutcome::RequestChanges,
                        summary: Some("界".repeat(1366)),
                        report_file: None,
                    },
                )
                .await;
            assert_eq!(response_code(&invalid), Some("invalid_arguments"));

            let latest = fixture.latest_tool_intent().await;
            assert_eq!(latest.intent_id, first.intent_id);
            assert_eq!(latest.accepted_ordinal, 1);
            assert_eq!(latest.outcome, CompletionOutcome::Approve.as_str());
        }

        #[tokio::test]
        async fn complete_work_concurrent_redelivery_is_idempotent() {
            let dir = tempfile::tempdir().unwrap();
            let fixture = completion_tool_fixture_with_db(Arc::new(
                crate::db::init_database(dir.path(), "completion-concurrency-test")
                    .await
                    .unwrap(),
            ))
            .await;
            let control = Arc::new(CompleteWorkTestControl::snapshot_race(5));
            let request = approve();
            let responses = futures_util::future::join_all((0..5).map(|_| {
                accept_complete_work_txn_with_test_control(
                    fixture.db.as_ref(),
                    TASK_ID,
                    CHILD_CONNECTION_ID,
                    "same-call",
                    &request,
                    Arc::clone(&control),
                )
            }))
            .await;

            let accepted: Vec<AcceptedToolIntent> =
                responses.into_iter().map(Result::unwrap).collect();
            assert!(accepted
                .iter()
                .all(|intent| intent.intent_id == accepted[0].intent_id));
            assert!(accepted.iter().all(|intent| intent.accepted_ordinal == 1));
            assert!(
                control.retries() > 0,
                "the synchronized stale-snapshot race must exercise retry"
            );
        }

        #[tokio::test]
        async fn complete_work_concurrent_distinct_calls_receive_contiguous_ordinals() {
            let dir = tempfile::tempdir().unwrap();
            let fixture = completion_tool_fixture_with_db(Arc::new(
                crate::db::init_database(dir.path(), "completion-concurrency-test")
                    .await
                    .unwrap(),
            ))
            .await;
            let call_ids: Vec<String> = (0..5).map(|index| format!("call-{index}")).collect();
            let control = Arc::new(CompleteWorkTestControl::snapshot_race(5));
            let request = approve();
            let responses = futures_util::future::join_all(call_ids.iter().map(|call_id| {
                accept_complete_work_txn_with_test_control(
                    fixture.db.as_ref(),
                    TASK_ID,
                    CHILD_CONNECTION_ID,
                    call_id,
                    &request,
                    Arc::clone(&control),
                )
            }))
            .await;

            let mut ordinals: Vec<i64> = responses
                .into_iter()
                .map(Result::unwrap)
                .map(|intent| intent.accepted_ordinal)
                .collect();
            ordinals.sort_unstable();
            assert_eq!(ordinals, (1..=5).collect::<Vec<_>>());
            assert!(
                control.retries() > 0,
                "the synchronized ordinal race must exercise retry"
            );
        }

        #[tokio::test]
        async fn complete_work_rolls_back_each_retry_and_exhausts_at_the_bound() {
            let fixture = completion_tool_fixture().await;
            let control = Arc::new(CompleteWorkTestControl::transient_body_failures(usize::MAX));

            let error = accept_complete_work_txn_with_test_control(
                fixture.db.as_ref(),
                TASK_ID,
                CHILD_CONNECTION_ID,
                "retry-exhaustion",
                &approve(),
                Arc::clone(&control),
            )
            .await
            .unwrap_err();

            assert!(matches!(error, CompleteWorkError::Persistence(_)));
            assert_eq!(control.attempts(), 10);
            assert_eq!(control.rollbacks(), 10);
            assert_eq!(control.retries(), 9);
            assert!(delegation_completion_tool_intent::Entity::find()
                .one(&fixture.db.conn)
                .await
                .unwrap()
                .is_none());
        }

        #[tokio::test]
        async fn complete_work_commit_failure_rolls_back_without_retry() {
            let fixture = completion_tool_fixture().await;
            let control = Arc::new(CompleteWorkTestControl::commit_failures(1));

            let error = accept_complete_work_txn_with_test_control(
                fixture.db.as_ref(),
                TASK_ID,
                CHILD_CONNECTION_ID,
                "commit-failure",
                &approve(),
                Arc::clone(&control),
            )
            .await
            .unwrap_err();

            assert!(matches!(error, CompleteWorkError::Persistence(_)));
            assert_eq!(control.attempts(), 1);
            assert_eq!(control.rollbacks(), 1);
            assert_eq!(control.retries(), 0);
            assert!(delegation_completion_tool_intent::Entity::find()
                .one(&fixture.db.conn)
                .await
                .unwrap()
                .is_none());
        }

        #[tokio::test]
        async fn complete_work_dangling_committed_binding_fails_closed() {
            let fixture = completion_tool_fixture().await;
            fixture
                .db
                .conn
                .execute_unprepared("PRAGMA foreign_keys=OFF")
                .await
                .unwrap();
            delegation_workflow::Entity::delete_by_id(WORKFLOW_ID)
                .exec(&fixture.db.conn)
                .await
                .unwrap();

            let error = load_workflow_child_mcp_binding(&fixture.db, TASK_ID)
                .await
                .unwrap_err();
            assert!(matches!(error, CompleteWorkError::Persistence(_)));
        }
    }

    #[tokio::test]
    async fn child_publish_workflow_denied_root_only() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register("child-wf".into(), child_workflow_token_entry("child-conn"))
            .await;
        let listener = make_listener(broker, tokens, Some(42));
        let outcome = listener
            .process_publish_workflow(BrokerPublishWorkflowRequest {
                token: "child-wf".into(),
                document: serde_json::json!({
                    "schema_version": 1,
                    "workflow_kind": "brainstorm_to_delivery",
                    "publication_token": "tok",
                    "workflow_state": "skeleton",
                    "phases": [],
                    "nodes": [],
                    "edges": [],
                    "gates": [],
                }),
            })
            .await;
        assert_eq!(
            outcome["error"]["code"], "root_only",
            "child must not publish even when workflow_v2 token bit is set: {outcome}"
        );
    }

    #[tokio::test]
    async fn workflow_feature_disabled_token_is_rejected() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register("root-no-wf".into(), root_token_entry("parent"))
            .await;
        let listener = make_listener(broker, tokens, Some(42));
        let outcome = listener
            .process_get_workflow_state(BrokerGetWorkflowStateRequest {
                token: "root-no-wf".into(),
                workflow_id: None,
            })
            .await;
        assert_eq!(outcome["error"]["code"], "feature_disabled");
    }

    #[tokio::test]
    async fn workflow_manifest_v2_framed_publish_and_plan_settle_reach_store() {
        use crate::acp::delegation::companion::{
            dispatch_line, CompanionContext, CompanionFeatures, InflightCalls, LineAction,
        };
        use crate::acp::delegation::run_store::RunStore;
        use crate::db::entities::delegation_workflow::CompletionProtocolMode;
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
        use sea_orm::{ActiveModelTrait, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/workflow-v2-listener").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let runs = Arc::new(RunStore::new(Arc::clone(&db)));
        let broker = Arc::new(
            DelegationBroker::new(
                Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
                Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_run_store(runs),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "workflow-v2-token".into(),
                TokenEntry {
                    parent_connection_id: "parent-v2".into(),
                    working_dir: PathBuf::from("/tmp"),
                    coordination_v1: false,
                    delegation_continuation_v1: false,
                    role: CompanionRole::Root,
                    workflow_v2: true,
                    completion_v2: false,
                    bound_task_id: None,
                },
            )
            .await;
        let listener = make_listener(broker, tokens, Some(parent));
        #[cfg(windows)]
        let socket_path = PathBuf::from(format!(
            r"\\.\pipe\codeg-workflow-v2-test-{}",
            uuid::Uuid::new_v4()
        ));
        #[cfg(unix)]
        let socket_path = std::env::temp_dir().join(format!(
            "codeg-workflow-v2-test-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let listener_task = tokio::spawn(Arc::clone(&listener).run(socket_path.clone()));
        #[cfg(unix)]
        while !socket_path.try_exists().expect("check listener socket") {
            tokio::task::yield_now().await;
        }

        let companion = CompanionContext {
            parent_connection_id: "parent-v2".into(),
            socket_path: socket_path.to_string_lossy().into_owned(),
            token: "workflow-v2-token".into(),
            features: CompanionFeatures::parse(Some("workflow_v2")),
            role: CompanionRole::Root,
            connection_incarnation_id: "test-incarnation".into(),
            disabled_agents: Vec::new(),
        };
        let inflight = Arc::new(InflightCalls::new());

        async fn call_companion_workflow(
            context: &CompanionContext,
            inflight: Arc<InflightCalls>,
            id: u64,
            name: &str,
            arguments: Value,
        ) -> Value {
            let line = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            })
            .to_string();
            let LineAction::Spawn(call) = dispatch_line(context, inflight, &line).await else {
                panic!("workflow call must cross the companion transport")
            };
            call.future
                .await
                .response
                .expect("workflow response")
                .result
                .expect("workflow result")
        }

        let manifest = json!({
            "schema_version": 2,
            "workflow_kind": "brainstorm_to_delivery",
            "publication_token": "listener-v2-plan",
            "workflow_state": "estimated",
            "plan_target_rel_path": "docs/superpowers/plans/p.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "plan": {
                "rel_path": "docs/superpowers/plans/p.md",
                "digest": "sha256:plan"
            },
            "phases": [{"id": "plan"}, {"id": "tasks"}],
            "nodes": [
                {
                    "id": "plan-author",
                    "kind": "work_unit",
                    "phase_id": "plan",
                    "role": "author",
                    "agent_type": "codex",
                    "work_unit_key": "plan|docs/superpowers/plans/p.md|author|codex|none",
                    "deps": []
                },
                {
                    "id": "plan-reviewer-codex",
                    "kind": "work_unit",
                    "phase_id": "plan",
                    "role": "reviewer",
                    "agent_type": "codex",
                    "work_unit_key": "plan|docs/superpowers/plans/p.md|reviewer|codex|none",
                    "deps": ["plan-author"]
                },
                {
                    "id": "plan-reviewer-grok",
                    "kind": "work_unit",
                    "phase_id": "plan",
                    "role": "reviewer",
                    "agent_type": "grok",
                    "work_unit_key": "plan|docs/superpowers/plans/p.md|reviewer|grok|none",
                    "deps": ["plan-author"]
                },
                {
                    "id": "task-1-implementer",
                    "kind": "work_unit",
                    "phase_id": "tasks",
                    "role": "implementer",
                    "agent_type": "codex",
                    "task_index": 1,
                    "work_unit_key": "task|1|implementer|codex|none",
                    "deps": []
                },
                {
                    "id": "task-1-reviewer-codex",
                    "kind": "work_unit",
                    "phase_id": "tasks",
                    "role": "reviewer",
                    "agent_type": "codex",
                    "task_index": 1,
                    "work_unit_key": "task|1|reviewer|codex|none",
                    "deps": ["task-1-implementer"]
                },
                {
                    "id": "task-1-reviewer-grok",
                    "kind": "work_unit",
                    "phase_id": "tasks",
                    "role": "reviewer",
                    "agent_type": "grok",
                    "task_index": 1,
                    "work_unit_key": "task|1|reviewer|grok|none",
                    "deps": ["task-1-implementer"]
                }
            ],
            "edges": [],
            "gates": [{
                "id": "plan-gate",
                "gate_kind": "plan",
                "reviewer_cohort_node_ids": [
                    "plan-reviewer-codex",
                    "plan-reviewer-grok"
                ],
                "required_reviewer_node_ids": ["plan-reviewer-codex"],
                "resolution_mode": "parent_adjudication"
            }],
            "task_policies": [{
                "task_index": 1,
                "risk": {
                    "level": "high",
                    "hard_triggers": [{
                        "kind": "public_compatibility",
                        "evidence": ["serialized workflow protocol"]
                    }],
                    "soft_signals": [],
                    "score": 0,
                    "reason": "public compatibility freezes high risk"
                },
                "route": {
                    "implementer_node_id": "task-1-implementer",
                    "reviewer_node_ids": [
                        "task-1-reviewer-codex",
                        "task-1-reviewer-grok"
                    ]
                }
            }]
        });

        let publish_result = call_companion_workflow(
            &companion,
            Arc::clone(&inflight),
            1,
            "publish_workflow_manifest",
            manifest.clone(),
        )
        .await;
        assert_eq!(publish_result["isError"], false);
        let workflow_id = publish_result["structuredContent"]["workflow_id"]
            .as_str()
            .expect("published workflow id")
            .to_string();
        let header = delegation_workflow::Entity::find_by_id(&workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut header: delegation_workflow::ActiveModel = header.into();
        header.completion_protocol_version = Set(2);
        header.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
        header.update(&db.conn).await.unwrap();

        let outcome = listener
            .process_get_workflow_state(BrokerGetWorkflowStateRequest {
                token: "workflow-v2-token".into(),
                workflow_id: Some(workflow_id.clone()),
            })
            .await;
        assert_eq!(outcome["detail"], "index");
        assert!(outcome.get("_codeg_omission_state").is_some());
        assert!(outcome
            .pointer("/latest_plan_review/findings/0/summary")
            .is_none());

        let not_found = listener
            .process_get_workflow_state(BrokerGetWorkflowStateRequest {
                token: "workflow-v2-token".into(),
                workflow_id: Some("missing-workflow".into()),
            })
            .await;
        assert_eq!(not_found["error"]["code"], "not_found");

        let foreign_folder = seed_folder(&db, "/tmp/workflow-v2-listener-foreign").await;
        let foreign_parent = seed_conversation(&db, foreign_folder, AgentType::Codex).await;
        let mut foreign_document: ManifestDocument =
            serde_json::from_value(manifest.clone()).unwrap();
        foreign_document.publication_token = "listener-v2-foreign".into();
        let foreign = publish_workflow_manifest_core(
            &db,
            &EventEmitter::Noop,
            foreign_parent,
            PublishWorkflowRequest {
                document: foreign_document,
            },
        )
        .await
        .unwrap();
        let cross_parent = listener
            .process_get_workflow_state(BrokerGetWorkflowStateRequest {
                token: "workflow-v2-token".into(),
                workflow_id: Some(foreign.workflow_id),
            })
            .await;
        assert_eq!(cross_parent["error"]["code"], "cross_parent");

        let mut v1 = manifest;
        v1["schema_version"] = json!(1);
        v1["publication_token"] = json!("listener-v1-rejected");
        let v1_result = call_companion_workflow(
            &companion,
            Arc::clone(&inflight),
            2,
            "publish_workflow_manifest",
            v1,
        )
        .await;
        assert_eq!(
            v1_result["structuredContent"]["error"]["code"],
            "validation"
        );

        let reduced = call_companion_workflow(
            &companion,
            Arc::clone(&inflight),
            3,
            "settle_workflow_gate",
            json!({
                "workflow_id": workflow_id,
                "gate_id": "plan-gate",
                "expected_graph_revision": 1,
                "expected_review_round": 1,
                "expected_outcome": "changes_requested",
                "summary": "derive evidence from platform state"
            }),
        )
        .await;
        assert_eq!(
            reduced["structuredContent"]["error"]["code"],
            "gate_not_ready"
        );

        let plan_evidence = json!({
            "kind": "plan",
            "scope": "full",
            "revision_kind": "initial",
            "scope_reason": "initial independent review",
            "covered_author_task_id": "author-task-1",
            "covered_plan_digest": "sha256:contradictory-plan",
            "required_reviewer_node_ids": ["plan-reviewer-codex"],
            "finding_updates": [],
            "lineage_reset_reason": null
        });
        let legacy_result = call_companion_workflow(
            &companion,
            inflight,
            4,
            "settle_workflow_gate",
            json!({
                "workflow_id": workflow_id,
                "manifest_revision": 1,
                "gate_id": "plan-gate",
                "expected_graph_revision": 1,
                "gate_cycle": 1,
                "outcome": "changes_requested",
                "evidence": plan_evidence,
                "summary": "framed contradictory Plan evidence"
            }),
        )
        .await;
        assert_eq!(
            legacy_result["structuredContent"]["error"]["code"],
            "invalid_arguments"
        );
        listener_task.abort();
        let _ = listener_task.await;
        #[cfg(unix)]
        let _ = tokio::fs::remove_file(socket_path).await;
    }

    async fn decision_fixture() -> (
        Arc<DelegationListener>,
        Arc<DelegationBroker>,
        Arc<MemoryDelegationAttentionStore>,
        String,
    ) {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(accepted(22, Utc::now()))).await;
        let task_store = Arc::new(MockTaskStore::accept_any_running(22));
        let attention = Arc::new(MemoryDelegationAttentionStore::new());
        let broker = Arc::new(
            DelegationBroker::new(
                mock as Arc<dyn ConnectionSpawner>,
                Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>)
            .with_attention_store(attention.clone() as Arc<dyn DelegationAttentionStore>),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        let ack = broker
            .start_delegation(crate::acp::delegation::types::DelegationRequest {
                parent_connection_id: "parent".into(),
                parent_conversation_id: 11,
                parent_tool_use_id: "pt-decision".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "decide".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await;
        let task_id = ack.task_id.expect("running");
        task_store.seed_edge(&task_id, 11, 22).await;
        attention.seed_edge(&task_id, 11, 22).await;

        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register("child-tok".into(), child_token_entry("child-conn"))
            .await;
        tokens
            .register("parent-tok".into(), root_token_entry("parent"))
            .await;
        tokens
            .register("foreign-tok".into(), root_token_entry("foreign"))
            .await;
        tokens
            .register(
                "legacy-tok".into(),
                TokenEntry::legacy("parent", PathBuf::from("/tmp")),
            )
            .await;

        let mut convs = StdHashMap::new();
        convs.insert("parent".into(), 11);
        convs.insert("child-conn".into(), 22);
        convs.insert("foreign".into(), 99);

        let listener = DelegationListener::new(
            broker.clone(),
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(MapParentLookup(convs)),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
        );
        (listener, broker, attention, task_id)
    }

    async fn wait_open_request(
        attention: &MemoryDelegationAttentionStore,
        task_id: &str,
    ) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(open) = attention
                .list_open_for_tasks(11, &[task_id.to_string()])
                .await
            {
                if let Some(summary) = open.into_iter().next() {
                    return summary.request_id;
                }
            }
            if std::time::Instant::now() >= deadline {
                panic!("open attention request did not appear for {task_id}");
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn root_token_request_parent_decision_is_rejected() {
        let (listener, _broker, _attention, _task_id) = decision_fixture().await;
        let outcome = listener
            .process_parent_decision(BrokerParentDecisionRequest {
                token: "parent-tok".into(),
                child_tool_call_id: "tc-1".into(),
                message: "choose".into(),
            })
            .await;
        assert!(matches!(
            outcome,
            ParentDecisionResult::Rejected {
                code,
                ..
            } if code == "not_delegation_child"
        ));
    }

    #[tokio::test]
    async fn parent_decision_round_trip_blocks_until_direct_parent_replies() {
        let (listener, _broker, attention, task_id) = decision_fixture().await;

        // Child connection: ParentDecision blocks until reply.
        let (mut child_client, mut child_server) = duplex(16 * 1024);
        let child_listener = listener.clone();
        let child_task = tokio::spawn(async move {
            child_listener.serve_one(&mut child_server).await.unwrap();
        });
        write_frame(
            &mut child_client,
            &BrokerMessage::ParentDecision(BrokerParentDecisionRequest {
                token: "child-tok".into(),
                child_tool_call_id: "child-tool-1".into(),
                message: "Use A or B?".into(),
            }),
        )
        .await
        .unwrap();

        let request_id = wait_open_request(&attention, &task_id).await;

        // Still pending: negative 25 ms timeout must not observe a response.
        let early = tokio::time::timeout(Duration::from_millis(25), async {
            read_frame::<_, BrokerResponse>(&mut child_client).await
        })
        .await;
        assert!(
            early.is_err(),
            "ParentDecision must remain pending until reply"
        );

        // Parent replies on a second connection.
        let (mut parent_client, mut parent_server) = duplex(8 * 1024);
        let parent_listener = listener.clone();
        let parent_task = tokio::spawn(async move {
            parent_listener.serve_one(&mut parent_server).await.unwrap();
        });
        write_frame(
            &mut parent_client,
            &BrokerMessage::ReplyDelegation(BrokerReplyDelegationRequest {
                token: "parent-tok".into(),
                request_id: request_id.clone(),
                reply: "Use A".into(),
            }),
        )
        .await
        .unwrap();
        let parent_resp: BrokerResponse = tokio::time::timeout(Duration::from_secs(1), async {
            read_frame(&mut parent_client).await.unwrap()
        })
        .await
        .expect("reply should complete");
        parent_task.await.unwrap();
        assert_eq!(parent_resp.outcome["status"], "replied");

        let child_resp: BrokerResponse = tokio::time::timeout(Duration::from_secs(1), async {
            read_frame(&mut child_client).await.unwrap()
        })
        .await
        .expect("decision should unblock");
        child_task.await.unwrap();
        assert_eq!(child_resp.outcome["status"], "replied");
        assert_eq!(child_resp.outcome["reply"], "Use A");
        assert_eq!(child_resp.outcome["request_id"], request_id);
    }

    #[tokio::test]
    async fn decision_socket_peer_close_keeps_row_open_and_replay_recovers_request_id() {
        let (listener, _broker, attention, task_id) = decision_fixture().await;

        let (mut child_client, mut child_server) = duplex(16 * 1024);
        let child_listener = listener.clone();
        let child_task = tokio::spawn(async move {
            child_listener.serve_one(&mut child_server).await.unwrap();
        });
        write_frame(
            &mut child_client,
            &BrokerMessage::ParentDecision(BrokerParentDecisionRequest {
                token: "child-tok".into(),
                child_tool_call_id: "replay-tool".into(),
                message: "Need choice".into(),
            }),
        )
        .await
        .unwrap();
        let request_id = wait_open_request(&attention, &task_id).await;

        // Peer close after persistence: abandon waiter only.
        drop(child_client);
        tokio::time::timeout(Duration::from_secs(1), child_task)
            .await
            .expect("serve_one must exit on peer close")
            .unwrap();

        let still_open = attention
            .list_open_for_tasks(11, std::slice::from_ref(&task_id))
            .await
            .unwrap();
        assert_eq!(still_open.len(), 1);
        assert_eq!(still_open[0].request_id, request_id);

        // Replay with same internal call id recovers the same request_id.
        let (mut child_client2, mut child_server2) = duplex(16 * 1024);
        let child_listener2 = listener.clone();
        let child_task2 = tokio::spawn(async move {
            child_listener2.serve_one(&mut child_server2).await.unwrap();
        });
        write_frame(
            &mut child_client2,
            &BrokerMessage::ParentDecision(BrokerParentDecisionRequest {
                token: "child-tok".into(),
                child_tool_call_id: "replay-tool".into(),
                message: "Need choice".into(),
            }),
        )
        .await
        .unwrap();
        // Let recover settle; row still open with same id.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let open_again = attention
            .list_open_for_tasks(11, std::slice::from_ref(&task_id))
            .await
            .unwrap();
        assert_eq!(open_again.len(), 1);
        assert_eq!(open_again[0].request_id, request_id);

        // Clean up by replying so the server task finishes.
        let (mut parent_client, mut parent_server) = duplex(8 * 1024);
        let parent_listener = listener.clone();
        let parent_task = tokio::spawn(async move {
            parent_listener.serve_one(&mut parent_server).await.unwrap();
        });
        write_frame(
            &mut parent_client,
            &BrokerMessage::ReplyDelegation(BrokerReplyDelegationRequest {
                token: "parent-tok".into(),
                request_id: request_id.clone(),
                reply: "ok".into(),
            }),
        )
        .await
        .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            read_frame::<_, BrokerResponse>(&mut parent_client)
                .await
                .unwrap()
        })
        .await;
        parent_task.await.unwrap();
        let child_resp: BrokerResponse = tokio::time::timeout(Duration::from_secs(1), async {
            read_frame(&mut child_client2).await.unwrap()
        })
        .await
        .expect("replay should unblock");
        child_task2.await.unwrap();
        assert_eq!(child_resp.outcome["request_id"], request_id);
        assert_eq!(child_resp.outcome["status"], "replied");
    }

    #[tokio::test]
    async fn foreign_parent_reply_is_unauthorized() {
        let (listener, _broker, attention, task_id) = decision_fixture().await;
        let decision = tokio::spawn({
            let listener = listener.clone();
            async move {
                listener
                    .process_parent_decision(BrokerParentDecisionRequest {
                        token: "child-tok".into(),
                        child_tool_call_id: "tc-foreign".into(),
                        message: "x".into(),
                    })
                    .await
            }
        });
        let request_id = wait_open_request(&attention, &task_id).await;
        let reply = listener
            .process_reply_delegation(BrokerReplyDelegationRequest {
                token: "foreign-tok".into(),
                request_id: request_id.clone(),
                reply: "nope".into(),
            })
            .await;
        assert_eq!(reply, DelegationReplyResult::Unauthorized);
        // Direct parent still succeeds.
        let ok = listener
            .process_reply_delegation(BrokerReplyDelegationRequest {
                token: "parent-tok".into(),
                request_id,
                reply: "yes".into(),
            })
            .await;
        assert!(matches!(ok, DelegationReplyResult::Replied { .. }));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), decision)
                .await
                .expect("decision completes")
                .unwrap(),
            ParentDecisionResult::Replied { .. }
        ));
    }

    #[tokio::test]
    async fn same_direct_parent_reply_replay_is_idempotent_and_conflict_is_already_resolved() {
        let (listener, _broker, attention, task_id) = decision_fixture().await;
        let decision = tokio::spawn({
            let listener = listener.clone();
            async move {
                listener
                    .process_parent_decision(BrokerParentDecisionRequest {
                        token: "child-tok".into(),
                        child_tool_call_id: "tc-idem".into(),
                        message: "x".into(),
                    })
                    .await
            }
        });
        let request_id = wait_open_request(&attention, &task_id).await;
        assert!(matches!(
            listener
                .process_reply_delegation(BrokerReplyDelegationRequest {
                    token: "parent-tok".into(),
                    request_id: request_id.clone(),
                    reply: "A".into(),
                })
                .await,
            DelegationReplyResult::Replied { .. }
        ));
        assert!(matches!(
            listener
                .process_reply_delegation(BrokerReplyDelegationRequest {
                    token: "parent-tok".into(),
                    request_id: request_id.clone(),
                    reply: "A".into(),
                })
                .await,
            DelegationReplyResult::Idempotent { .. }
        ));
        assert!(matches!(
            listener
                .process_reply_delegation(BrokerReplyDelegationRequest {
                    token: "parent-tok".into(),
                    request_id,
                    reply: "B".into(),
                })
                .await,
            DelegationReplyResult::AlreadyResolved { .. }
        ));
        let _ = tokio::time::timeout(Duration::from_secs(1), decision).await;
    }

    #[tokio::test]
    async fn task_terminal_while_decision_blocked_closes_with_task_terminal() {
        let (listener, broker, attention, task_id) = decision_fixture().await;
        let decision = tokio::spawn({
            let listener = listener.clone();
            async move {
                listener
                    .process_parent_decision(BrokerParentDecisionRequest {
                        token: "child-tok".into(),
                        child_tool_call_id: "tc-term".into(),
                        message: "continue?".into(),
                    })
                    .await
            }
        });
        wait_open_request(&attention, &task_id).await;
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 22,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 1,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = tokio::time::timeout(Duration::from_secs(1), decision)
            .await
            .expect("decision closed")
            .unwrap();
        assert!(matches!(
            outcome,
            ParentDecisionResult::Closed {
                resolution_code: AttentionResolutionCode::TaskTerminal,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn nested_child_can_request_from_parent_and_reply_to_grandchild() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(accepted(2, Utc::now()))).await;
        mock.queue_spawn(Ok("grand-conn".into())).await;
        mock.queue_send(Ok(accepted(3, Utc::now()))).await;
        let task_store = Arc::new(MockTaskStore::accept_any_running(2));
        let attention = Arc::new(MemoryDelegationAttentionStore::new());
        let broker = Arc::new(
            DelegationBroker::new(
                mock as Arc<dyn ConnectionSpawner>,
                Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_task_store(task_store.clone() as Arc<dyn DelegationTaskStore>)
            .with_attention_store(attention.clone() as Arc<dyn DelegationAttentionStore>),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;

        let root_child = broker
            .start_delegation(crate::acp::delegation::types::DelegationRequest {
                parent_connection_id: "root-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "pt-root".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "mid".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await
            .task_id
            .unwrap();
        let grandchild = broker
            .start_delegation(crate::acp::delegation::types::DelegationRequest {
                parent_connection_id: "child-conn".into(),
                parent_conversation_id: 2,
                parent_tool_use_id: "pt-mid".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "leaf".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await
            .task_id
            .unwrap();
        task_store.seed_edge(&root_child, 1, 2).await;
        task_store.seed_edge(&grandchild, 2, 3).await;
        attention.seed_edge(&root_child, 1, 2).await;
        attention.seed_edge(&grandchild, 2, 3).await;

        let tokens = Arc::new(TokenRegistry::default());
        // Middle connection is a coordination child (can request + reply).
        tokens
            .register("mid-tok".into(), child_token_entry("child-conn"))
            .await;
        tokens
            .register("grand-tok".into(), child_token_entry("grand-conn"))
            .await;
        tokens
            .register("root-tok".into(), root_token_entry("root-conn"))
            .await;

        let mut convs = StdHashMap::new();
        convs.insert("root-conn".into(), 1);
        convs.insert("child-conn".into(), 2);
        convs.insert("grand-conn".into(), 3);

        let listener = DelegationListener::new(
            broker,
            tokens,
            Arc::new(CompanionLeaseRegistry::default()),
            Arc::new(MapParentLookup(convs)),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
        );

        // Grandchild requests from middle child.
        let grand_wait = tokio::spawn({
            let listener = listener.clone();
            async move {
                listener
                    .process_parent_decision(BrokerParentDecisionRequest {
                        token: "grand-tok".into(),
                        child_tool_call_id: "tc-grand".into(),
                        message: "Which API?".into(),
                    })
                    .await
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let grand_request_id = loop {
            if let Ok(open) = attention
                .list_open_for_tasks(2, std::slice::from_ref(&grandchild))
                .await
            {
                if let Some(s) = open.into_iter().next() {
                    break s.request_id;
                }
            }
            if std::time::Instant::now() >= deadline {
                panic!("grandchild attention missing");
            }
            tokio::task::yield_now().await;
        };
        // Middle child replies to its child.
        assert!(matches!(
            listener
                .process_reply_delegation(BrokerReplyDelegationRequest {
                    token: "mid-tok".into(),
                    request_id: grand_request_id,
                    reply: "v2".into(),
                })
                .await,
            DelegationReplyResult::Replied { .. }
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), grand_wait)
                .await
                .unwrap()
                .unwrap(),
            ParentDecisionResult::Replied { .. }
        ));

        // Middle child can also request from root.
        let mid_wait = tokio::spawn({
            let listener = listener.clone();
            async move {
                listener
                    .process_parent_decision(BrokerParentDecisionRequest {
                        token: "mid-tok".into(),
                        child_tool_call_id: "tc-mid".into(),
                        message: "Ship?".into(),
                    })
                    .await
            }
        });
        let mid_request_id = loop {
            if let Ok(open) = attention
                .list_open_for_tasks(1, std::slice::from_ref(&root_child))
                .await
            {
                if let Some(s) = open.into_iter().next() {
                    break s.request_id;
                }
            }
            if std::time::Instant::now() >= deadline {
                panic!("mid attention missing");
            }
            tokio::task::yield_now().await;
        };
        assert!(matches!(
            listener
                .process_reply_delegation(BrokerReplyDelegationRequest {
                    token: "root-tok".into(),
                    request_id: mid_request_id,
                    reply: "ship".into(),
                })
                .await,
            DelegationReplyResult::Replied { .. }
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), mid_wait)
                .await
                .unwrap()
                .unwrap(),
            ParentDecisionResult::Replied { .. }
        ));
    }

    #[tokio::test]
    async fn legacy_token_cannot_use_decision_tools() {
        let (listener, _broker, _attention, _task_id) = decision_fixture().await;
        assert!(matches!(
            listener
                .process_parent_decision(BrokerParentDecisionRequest {
                    token: "legacy-tok".into(),
                    child_tool_call_id: "tc".into(),
                    message: "x".into(),
                })
                .await,
            ParentDecisionResult::Rejected {
                code,
                ..
            } if code == "coordination_unavailable"
        ));
        assert!(matches!(
            listener
                .process_reply_delegation(BrokerReplyDelegationRequest {
                    token: "legacy-tok".into(),
                    request_id: "missing".into(),
                    reply: "x".into(),
                })
                .await,
            DelegationReplyResult::Rejected {
                code,
                ..
            } if code == "coordination_unavailable"
        ));
    }

    mod recovery_tool_contract {
        use super::*;
        use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
        use crate::acp::delegation::spawner::DelegationLink;
        use crate::acp::delegation::store::TerminalTaskWrite;
        use crate::acp::question::QuestionAnsweredItem;
        use crate::acp::termination::DelegationTerminationAuditV1;
        use crate::db::entities::delegation_task_run::{
            self as delegation_task_run, AdmissionClass, DelegationRunStatus,
        };
        use crate::db::entities::recovery_authorization::RecoveryAuthorizationStatus;
        use crate::db::entities::{conversation, recovery_authorization};
        use crate::db::service::conversation_service;
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
        use sea_orm::{
            ActiveModelTrait, ColumnTrait, IntoActiveModel, PaginatorTrait, QueryFilter, Set,
        };
        use tokio::io::DuplexStream;

        struct RecoveryFixture {
            db: Arc<crate::db::AppDatabase>,
            folder_id: i32,
            parent_id: i32,
            runs: Arc<RunStore>,
            spawner: Arc<MockSpawner>,
            tokens: Arc<TokenRegistry>,
            listener: Arc<DelegationListener>,
            questions: Arc<StubQuestion>,
        }

        async fn recovery_fixture() -> RecoveryFixture {
            let db = Arc::new(fresh_in_memory_db().await);
            let folder_id = seed_folder(&db, "/tmp/recovery-listener-contract").await;
            let parent_id = seed_conversation(&db, folder_id, AgentType::ClaudeCode).await;
            let runs = Arc::new(RunStore::new(Arc::clone(&db)));
            let spawner = Arc::new(MockSpawner::new());
            let broker = Arc::new(
                DelegationBroker::new(
                    Arc::clone(&spawner) as Arc<dyn ConnectionSpawner>,
                    Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
                )
                .with_run_store(Arc::clone(&runs)),
            );
            broker
                .set_config(DelegationConfig {
                    enabled: true,
                    ..DelegationConfig::default()
                })
                .await;
            let tokens = Arc::new(TokenRegistry::default());
            tokens
                .register(
                    "recovery-child-token".into(),
                    TokenEntry {
                        parent_connection_id: "recovery-parent-conn".into(),
                        working_dir: PathBuf::from("/tmp"),
                        coordination_v1: true,
                        delegation_continuation_v1: false,
                        role: CompanionRole::DelegationChild,
                        workflow_v2: true,
                        completion_v2: false,
                        bound_task_id: None,
                    },
                )
                .await;
            let questions = Arc::new(StubQuestion::default());
            let listener = DelegationListener::new(
                broker,
                Arc::clone(&tokens),
                Arc::new(CompanionLeaseRegistry::default()),
                Arc::new(StaticParentLookup(Some(parent_id))),
                Arc::new(StubFeedback::default()),
                Arc::clone(&questions) as Arc<dyn SessionQuestionAccess>,
                Arc::new(StubSessionInfo::default()),
            );
            RecoveryFixture {
                db,
                folder_id,
                parent_id,
                runs,
                spawner,
                tokens,
                listener,
                questions,
            }
        }

        async fn seed_confirmable_task(
            fixture: &RecoveryFixture,
            parent_id: i32,
            label: &str,
        ) -> String {
            let task_id = format!("{label}-{}", uuid::Uuid::new_v4());
            let child = conversation_service::create_with_delegation(
                &fixture.db.conn,
                fixture.folder_id,
                AgentType::Codex,
                Some(format!("child {label}")),
                None,
                Some(DelegationLink {
                    parent_conversation_id: parent_id,
                    parent_tool_use_id: format!("tool-{label}"),
                    delegation_call_id: format!("call-{label}"),
                }),
            )
            .await
            .expect("seed delegated child");
            let mut active: conversation::ActiveModel = child.clone().into();
            active.external_id = Set(Some(format!("session-{label}")));
            active
                .update(&fixture.db.conn)
                .await
                .expect("seed reusable session identity");

            fixture
                .runs
                .insert_reserving(ReservingRunInsert {
                    task_id: task_id.clone(),
                    root_task_id: task_id.clone(),
                    previous_task_id: None,
                    generation: 1,
                    parent_conversation_id: parent_id,
                    parent_tool_use_id: Some(format!("tool-{label}")),
                    child_conversation_id: child.id,
                    agent_type: "codex".into(),
                    profile_id: None,
                    workspace_path: Some("/tmp/recovery-listener-contract".into()),
                    route_fingerprint: Some("aabbccdd".into()),
                    launch_snapshot_version: Some("v1".into()),
                    mode_id: Some("default".into()),
                    config_values_json: Some("{}".into()),
                    task_preview: Some("recovery contract task".into()),
                    request_fingerprint: Some("a".repeat(64)),
                    admission_class: AdmissionClass::NormalRevision,
                    lineage_root_task_id: task_id.clone(),
                    work_unit_key: Some(format!("task|{label}|implementer|codex|none")),
                    history_only: false,
                    replaced_task_id: None,
                    replacement_reason: None,
                    started_at: Some(Utc::now()),
                })
                .await
                .expect("insert recovery source");
            let child_connection_id = format!("child-connection-{label}");
            fixture
                .runs
                .bind_child_connection_while_reserving(&task_id, &child_connection_id)
                .await
                .expect("bind recovery child");
            fixture
                .runs
                .promote_running(&task_id, &child_connection_id, Utc::now())
                .await
                .expect("promote recovery child");
            let finished_at = Utc::now();
            fixture
                .runs
                .settle_terminal(
                    &task_id,
                    TerminalTaskWrite::canceled(
                        "parent_turn_failed",
                        finished_at,
                        DelegationTerminationAuditV1::for_terminal_code(
                            "parent_turn_failed",
                            DelegationRunStatus::Running,
                            true,
                            finished_at,
                        ),
                    ),
                )
                .await
                .expect("settle recovery source");
            task_id
        }

        fn authorization_request(token: &str, task_id: &str, suffix: &str) -> BrokerMessage {
            BrokerMessage::RequestRecoveryAuthorization(BrokerRecoveryAuthorizationRequest {
                token: token.into(),
                subject_kind: RecoverySubjectKind::DelegationTask,
                subject_id: task_id.into(),
                correlation_id: format!("recovery-{suffix}"),
                proposed_user_reason: None,
            })
        }

        async fn start_call(
            listener: Arc<DelegationListener>,
            message: BrokerMessage,
        ) -> (DuplexStream, tokio::task::JoinHandle<std::io::Result<()>>) {
            let (mut client, mut server) = duplex(64 * 1024);
            let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });
            write_frame(&mut client, &message)
                .await
                .expect("write recovery request");
            (client, server_task)
        }

        async fn finish_call(
            mut client: DuplexStream,
            server_task: tokio::task::JoinHandle<std::io::Result<()>>,
        ) -> Value {
            let response: BrokerResponse = read_frame(&mut client)
                .await
                .expect("read recovery response");
            server_task
                .await
                .expect("listener task")
                .expect("listener response");
            response.outcome
        }

        async fn immediate_call(
            listener: Arc<DelegationListener>,
            message: BrokerMessage,
        ) -> Value {
            let (client, task) = start_call(listener, message).await;
            finish_call(client, task).await
        }

        async fn wait_for_questions(questions: &StubQuestion, expected: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if questions.registered.lock().await.len() >= expected {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("recovery question registration");
        }

        fn question_outcome(label: Option<&str>, declined: bool) -> QuestionOutcome {
            QuestionOutcome {
                answers: label
                    .map(|label| {
                        vec![QuestionAnsweredItem {
                            question: "recovery_authorization".into(),
                            header: "Recovery".into(),
                            multi_select: false,
                            selected: vec![label.into()],
                        }]
                    })
                    .unwrap_or_default(),
                declined,
            }
        }

        async fn authorization_count(fixture: &RecoveryFixture) -> u64 {
            recovery_authorization::Entity::find()
                .count(&fixture.db.conn)
                .await
                .expect("count recovery authorization rows")
        }

        #[test]
        fn authorization_result_projects_workflow_target_and_reset_metadata() {
            let base = RecoveryAuthorizationResult {
                authorization_id: "workflow-authorization".into(),
                status: RecoveryAuthorizationStatus::Approved,
                subject_kind: "workflow".into(),
                subject_id: "workflow-a".into(),
                allowed_action: "recover_workflow".into(),
                action_payload: json!({ "target_state": "approved" }),
                cause_code: "legacy_block_with_current_plan_approval".into(),
                display_reason: None,
                approved_at: Some(Utc::now()),
                expires_at: Some(Utc::now()),
            };
            let recovered = recovery_authorization_result_value(&base, false);
            assert_eq!(recovered["subject_kind"], "workflow");
            assert_eq!(recovered["subject_id"], "workflow-a");
            assert_eq!(recovered["allowed_action"], "recover_workflow");
            assert_eq!(recovered["target_state"], "approved");
            assert_eq!(
                recovered["cause_code"],
                "legacy_block_with_current_plan_approval"
            );
            assert!(recovered["expires_at"].as_str().is_some());
            assert!(recovered.get("replacement_reason").is_none());

            let reason = "Reset the exact approved Plan review lineage.";
            let reset = RecoveryAuthorizationResult {
                allowed_action: "reset_plan_lineage".into(),
                action_payload: json!({ "displayed_reason_sha256": "abc123" }),
                cause_code: "plan_user_decision_required".into(),
                display_reason: Some(reason.into()),
                ..base
            };
            let reset_value = recovery_authorization_result_value(&reset, true);
            assert_eq!(reset_value["allowed_action"], "reset_plan_lineage");
            assert_eq!(reset_value["display_reason"], reason);
            assert_eq!(reset_value["reused"], true);
            assert!(reset_value.get("target_state").is_none());

            let invalid = RecoveryAuthorizationResult {
                action_payload: json!({ "target_state": "unknown" }),
                ..reset
            };
            assert_eq!(
                recovery_authorization_result_value(&invalid, false)["error"]["code"],
                "recovery_authorization_contract_invalid"
            );
        }

        #[tokio::test]
        async fn listener_forwards_recovery_receipt_into_continue_admission() {
            let fixture = recovery_fixture().await;
            let source_task_id =
                seed_confirmable_task(&fixture, fixture.parent_id, "listener-continue").await;
            let source = fixture
                .runs
                .load_by_task_id(&source_task_id)
                .await
                .expect("load continue source")
                .expect("continue source row");

            let (client, server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request(
                    "recovery-child-token",
                    &source_task_id,
                    "listener-continue-authorize",
                ),
            )
            .await;
            wait_for_questions(&fixture.questions, 1).await;
            fixture
                .questions
                .answer("q-1", question_outcome(Some(RECOVERY_APPROVE_LABEL), false))
                .await;
            let authorization = finish_call(client, server).await;
            assert_eq!(authorization["status"], "approved");
            assert_eq!(authorization["subject_kind"], "delegation_task");
            assert_eq!(authorization["subject_id"], source_task_id);
            assert_eq!(authorization["allowed_action"], "continue");
            assert_eq!(authorization["cause_code"], "parent_turn_failed");
            assert_eq!(authorization["replacement_reason"], Value::Null);
            assert!(authorization["expires_at"].as_str().is_some());
            let authorization_id = authorization["recovery_authorization_id"]
                .as_str()
                .expect("approved recovery receipt")
                .to_string();

            fixture
                .spawner
                .queue_spawn(Ok("child-connection-listener-continue".into()))
                .await;
            fixture
                .spawner
                .queue_send(Ok(accepted(source.child_conversation_id, Utc::now())))
                .await;
            let replay = immediate_call(
                Arc::clone(&fixture.listener),
                BrokerMessage::Call(BrokerRequest {
                    token: "recovery-child-token".into(),
                    parent_connection_id: "recovery-parent-conn".into(),
                    parent_tool_use_id: "listener-continue-replay".into(),
                    external_handle: None,
                    input: json!({
                        "_codeg_tool": "continue_delegation",
                        "task_id": source_task_id,
                        "task": "recovery contract task",
                        "work_unit_key": "task|listener-continue|implementer|codex|none",
                        "correlation_id": "listener-continue-replay",
                        "recovery_authorization_id": authorization_id,
                    }),
                }),
            )
            .await;

            assert_eq!(
                replay["status"], "running",
                "authorized continue must admit"
            );
            assert_eq!(replay["continued_from_task_id"], source_task_id);
            assert_eq!(replay["reused_session"], true);
            assert_eq!(
                replay["child_conversation_id"],
                source.child_conversation_id
            );

            let row = recovery_authorization::Entity::find_by_id(&authorization_id)
                .one(&fixture.db.conn)
                .await
                .expect("load consumed recovery authorization")
                .expect("consumed recovery authorization row");
            assert_eq!(row.status, RecoveryAuthorizationStatus::Consumed);
            assert_eq!(row.authorization_id, authorization_id);
        }

        #[tokio::test]
        async fn listener_forwards_recovery_receipt_into_replacement_admission() {
            let fixture = recovery_fixture().await;
            let source_task_id =
                seed_confirmable_task(&fixture, fixture.parent_id, "listener-replace").await;
            let source = fixture
                .runs
                .load_by_task_id(&source_task_id)
                .await
                .expect("load replacement source")
                .expect("replacement source row");
            let workspace = std::env::current_dir()
                .expect("test workspace")
                .to_string_lossy()
                .into_owned();
            let mut source_row = delegation_task_run::Entity::find_by_id(&source_task_id)
                .one(&fixture.db.conn)
                .await
                .expect("load replacement source for workspace")
                .expect("replacement source model")
                .into_active_model();
            source_row.status = Set(DelegationRunStatus::Failed);
            source_row.error_code = Set(Some("admission_unknown".into()));
            source_row.reached_running_at = Set(None);
            source_row.termination_audit_json = Set(None);
            source_row.workspace_path = Set(Some(workspace.clone()));
            source_row
                .update(&fixture.db.conn)
                .await
                .expect("set replacement source workspace");
            let child = conversation::Entity::find_by_id(source.child_conversation_id)
                .one(&fixture.db.conn)
                .await
                .expect("load replacement child")
                .expect("replacement child row");
            let mut child = child.into_active_model();
            child.external_id = Set(None);
            child
                .update(&fixture.db.conn)
                .await
                .expect("remove resume identity");

            let (client, server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request(
                    "recovery-child-token",
                    &source_task_id,
                    "listener-replace-authorize",
                ),
            )
            .await;
            wait_for_questions(&fixture.questions, 1).await;
            fixture
                .questions
                .answer("q-1", question_outcome(Some(RECOVERY_APPROVE_LABEL), false))
                .await;
            let authorization = finish_call(client, server).await;
            assert_eq!(authorization["status"], "approved");
            assert_eq!(authorization["subject_kind"], "delegation_task");
            assert_eq!(authorization["subject_id"], source_task_id);
            assert_eq!(authorization["allowed_action"], "replace");
            assert_eq!(authorization["cause_code"], "admission_unknown");
            assert_eq!(authorization["replacement_reason"], "admission_unknown");
            assert!(authorization["expires_at"].as_str().is_some());
            let authorization_id = authorization["recovery_authorization_id"]
                .as_str()
                .expect("approved replacement receipt")
                .to_string();

            fixture
                .spawner
                .queue_spawn(Ok("child-connection-listener-replace".into()))
                .await;
            fixture
                .spawner
                .queue_send(Ok(accepted(0, Utc::now())))
                .await;
            let replay = immediate_call(
                Arc::clone(&fixture.listener),
                BrokerMessage::Call(BrokerRequest {
                    token: "recovery-child-token".into(),
                    parent_connection_id: "recovery-parent-conn".into(),
                    parent_tool_use_id: "listener-replace-replay".into(),
                    external_handle: None,
                    input: json!({
                        "agent_type": "codex",
                        "task": "replacement contract task",
                        "working_dir": workspace,
                        "work_unit_key": "task|listener-replace|implementer|codex|none",
                        "replaces_task_id": source_task_id,
                        "replacement_reason": "admission_unknown",
                        "correlation_id": "listener-replace-replay",
                        "recovery_authorization_id": authorization_id,
                    }),
                }),
            )
            .await;

            assert_eq!(
                replay["status"], "running",
                "authorized replacement must admit: {replay:?}"
            );
            let row = recovery_authorization::Entity::find_by_id(&authorization_id)
                .one(&fixture.db.conn)
                .await
                .expect("load consumed replacement authorization")
                .expect("consumed replacement authorization row");
            assert_eq!(row.status, RecoveryAuthorizationStatus::Consumed);
        }

        #[tokio::test]
        async fn delegation_child_cannot_call_recover_workflow_or_authorize_foreign_subject() {
            let fixture = recovery_fixture().await;
            let owned = seed_confirmable_task(&fixture, fixture.parent_id, "owned").await;
            let other_parent =
                seed_conversation(&fixture.db, fixture.folder_id, AgentType::ClaudeCode).await;
            let sibling = seed_confirmable_task(&fixture, other_parent, "sibling").await;
            let ancestor = seed_confirmable_task(&fixture, other_parent, "ancestor").await;
            let unrelated = seed_confirmable_task(&fixture, other_parent, "unrelated").await;

            for (index, foreign) in [&sibling, &ancestor, &unrelated].into_iter().enumerate() {
                let outcome = immediate_call(
                    Arc::clone(&fixture.listener),
                    authorization_request(
                        "recovery-child-token",
                        foreign,
                        &format!("foreign-{index}"),
                    ),
                )
                .await;
                assert_eq!(outcome["error"]["code"], "recovery_subject_not_owned");
                assert_eq!(authorization_count(&fixture).await, 0);
            }

            let workflow = immediate_call(
                Arc::clone(&fixture.listener),
                BrokerMessage::RequestRecoveryAuthorization(BrokerRecoveryAuthorizationRequest {
                    token: "recovery-child-token".into(),
                    subject_kind: RecoverySubjectKind::Workflow,
                    subject_id: "foreign-workflow".into(),
                    correlation_id: "recovery-foreign-workflow".into(),
                    proposed_user_reason: None,
                }),
            )
            .await;
            assert_eq!(workflow["error"]["code"], "root_only");
            assert_eq!(authorization_count(&fixture).await, 0);

            let (client, server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &owned, "owned"),
            )
            .await;
            wait_for_questions(&fixture.questions, 1).await;
            let registered = fixture.questions.registered.lock().await;
            assert_eq!(registered[0].0, "recovery-parent-conn");
            assert_eq!(registered[0].1.len(), 1);
            assert_eq!(registered[0].1[0].question, "recovery_authorization");
            let presentation = registered[0].1[0]
                .recovery
                .as_ref()
                .expect("fixed recovery presentation");
            assert_eq!(presentation.subject, "delegation_task");
            assert_eq!(presentation.target, "existing_session");
            assert_eq!(presentation.display_reason, None);
            drop(registered);
            fixture
                .questions
                .answer("q-1", question_outcome(Some(RECOVERY_DECLINE_LABEL), false))
                .await;
            let outcome = finish_call(client, server).await;
            assert_eq!(outcome["status"], "declined");
            assert_eq!(authorization_count(&fixture).await, 1);

            let recover = immediate_call(
                Arc::clone(&fixture.listener),
                BrokerMessage::RecoverWorkflow(BrokerRecoverWorkflowRequest {
                    token: "recovery-child-token".into(),
                    workflow_id: "foreign-workflow".into(),
                    recovery_authorization_id: "authorization-token".into(),
                    expected_manifest_revision: 1,
                    correlation_id: "recovery-child-workflow".into(),
                }),
            )
            .await;
            assert_eq!(recover["error"]["code"], "root_only");
            assert_eq!(authorization_count(&fixture).await, 1);
        }

        #[tokio::test]
        async fn authorization_question_decline_dismiss_disconnect_and_reconnect_map_to_stable_statuses(
        ) {
            let fixture = recovery_fixture().await;
            let approved = seed_confirmable_task(&fixture, fixture.parent_id, "approve").await;
            let declined = seed_confirmable_task(&fixture, fixture.parent_id, "decline").await;
            let dismissed = seed_confirmable_task(&fixture, fixture.parent_id, "dismiss").await;
            let abandoned = seed_confirmable_task(&fixture, fixture.parent_id, "abandon").await;
            let duplicate = seed_confirmable_task(&fixture, fixture.parent_id, "duplicate").await;

            let (client, server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &approved, "approve"),
            )
            .await;
            wait_for_questions(&fixture.questions, 1).await;
            fixture
                .questions
                .answer("q-1", question_outcome(Some(RECOVERY_APPROVE_LABEL), false))
                .await;
            let approved_outcome = finish_call(client, server).await;
            assert_eq!(approved_outcome["status"], "approved");
            assert_eq!(approved_outcome["subject_kind"], "delegation_task");
            assert_eq!(approved_outcome["subject_id"], approved);
            assert_eq!(approved_outcome["allowed_action"], "continue");
            assert_eq!(approved_outcome["cause_code"], "parent_turn_failed");
            assert_eq!(approved_outcome["replacement_reason"], Value::Null);
            assert!(approved_outcome["expires_at"].as_str().is_some());
            let approved_id = approved_outcome["recovery_authorization_id"]
                .as_str()
                .expect("approved authorization id")
                .to_string();

            fixture
                .tokens
                .register(
                    "recovery-reconnect-token".into(),
                    TokenEntry {
                        parent_connection_id: "recovery-reconnected-parent".into(),
                        working_dir: PathBuf::from("/tmp"),
                        coordination_v1: true,
                        delegation_continuation_v1: true,
                        role: CompanionRole::DelegationChild,
                        workflow_v2: false,
                        completion_v2: false,
                        bound_task_id: None,
                    },
                )
                .await;
            let reconnected = immediate_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-reconnect-token", &approved, "reconnect"),
            )
            .await;
            assert_eq!(reconnected["status"], "approved");
            assert_eq!(reconnected["reused"], true);
            assert_eq!(reconnected["recovery_authorization_id"], approved_id);
            assert_eq!(reconnected["subject_kind"], "delegation_task");
            assert_eq!(reconnected["subject_id"], approved);
            assert_eq!(reconnected["allowed_action"], "continue");
            assert_eq!(reconnected["cause_code"], "parent_turn_failed");
            assert_eq!(reconnected["replacement_reason"], Value::Null);
            assert_eq!(reconnected["expires_at"], approved_outcome["expires_at"]);
            assert_eq!(fixture.questions.registered.lock().await.len(), 1);

            let (client, server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &declined, "decline"),
            )
            .await;
            wait_for_questions(&fixture.questions, 2).await;
            fixture
                .questions
                .answer("q-2", question_outcome(Some(RECOVERY_DECLINE_LABEL), false))
                .await;
            let declined_outcome = finish_call(client, server).await;
            assert_eq!(declined_outcome["status"], "declined");
            assert_eq!(declined_outcome["subject_kind"], "delegation_task");
            assert_eq!(declined_outcome["subject_id"], declined);
            assert_eq!(declined_outcome["allowed_action"], "continue");
            assert_eq!(declined_outcome["cause_code"], "parent_turn_failed");
            assert_eq!(declined_outcome["replacement_reason"], Value::Null);
            assert_eq!(declined_outcome["expires_at"], Value::Null);

            let (client, server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &dismissed, "dismiss"),
            )
            .await;
            wait_for_questions(&fixture.questions, 3).await;
            fixture
                .questions
                .answer("q-3", question_outcome(None, true))
                .await;
            assert_eq!(finish_call(client, server).await["status"], "declined");

            let (abandoned_client, abandoned_server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &abandoned, "abandon"),
            )
            .await;
            wait_for_questions(&fixture.questions, 4).await;
            drop(abandoned_client);
            tokio::time::timeout(Duration::from_secs(5), abandoned_server)
                .await
                .expect("disconnect cleanup")
                .expect("disconnect listener task")
                .expect("disconnect listener result");
            let abandoned_row = recovery_authorization::Entity::find()
                .filter(recovery_authorization::Column::SubjectId.eq(&abandoned))
                .one(&fixture.db.conn)
                .await
                .expect("load abandoned authorization")
                .expect("abandoned authorization row");
            assert_eq!(abandoned_row.status, RecoveryAuthorizationStatus::Abandoned);

            let (first_client, first_server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &duplicate, "duplicate-a"),
            )
            .await;
            wait_for_questions(&fixture.questions, 5).await;
            let (second_client, second_server) = start_call(
                Arc::clone(&fixture.listener),
                authorization_request("recovery-child-token", &duplicate, "duplicate-b"),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert_eq!(
                fixture.questions.registered.lock().await.len(),
                5,
                "duplicate pending callers must share one question"
            );
            fixture
                .questions
                .answer("q-5", question_outcome(Some(RECOVERY_APPROVE_LABEL), false))
                .await;
            let first = finish_call(first_client, first_server).await;
            let second = finish_call(second_client, second_server).await;
            assert_eq!(first["status"], "approved");
            assert_eq!(second["status"], "approved");
            assert_eq!(
                first["recovery_authorization_id"],
                second["recovery_authorization_id"]
            );
            assert!(first["reused"] == true || second["reused"] == true);
        }
    }
}
