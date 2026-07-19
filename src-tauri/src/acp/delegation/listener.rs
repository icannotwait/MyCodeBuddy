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
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::acp::delegation::broker::{DelegationBroker, StatusWait};
use crate::acp::delegation::continuation::coordinator::{
    ContinuationError, DelegationContinuationCoordinator, JoinArmOutcome, JoinArmRequest,
};
use crate::acp::delegation::lease::CompanionLeaseRegistry;
use crate::acp::delegation::transport::{
    read_frame, write_frame, BrokerAskRequest, BrokerCancelRequest, BrokerCancelTaskRequest,
    BrokerCommitFeedbackRequest, BrokerFeedbackRequest, BrokerMessage, BrokerParentDecisionRequest,
    BrokerReplyDelegationRequest, BrokerRequest, BrokerResponse, BrokerSessionRequest,
    BrokerStatusRequest, CancelDelegationReason, CompanionReadyAck, CompanionRole,
};
use crate::acp::delegation::types::{
    DelegationReplyResult, DelegationRequest, DelegationReturnWhen, DelegationStatusBatch,
    DelegationTaskReport, DelegationWakeReason, ParentDecisionResult, TaskStatus,
};
use crate::acp::feedback::{PendingFeedback, SessionFeedbackAccess};
use crate::acp::question::{QuestionOutcome, SessionQuestionAccess};
use crate::acp::session_info::{SessionInfo, SessionInfoAccess};
use crate::models::AgentType;
use serde_json::Value;

/// Hard ceiling on a *positive* `get_delegation_status` long-poll, so a single
/// MCP tool call can't block the companion's round-trip unbounded. The child
/// keeps running past this; the LLM simply re-issues the wait. An explicit
/// `wait_ms = 0` opts out of the ceiling and blocks until the task is terminal.
const STATUS_WAIT_MAX_MS: u64 = 60_000;

/// Pluggable "what conversation is this parent currently in?" lookup. The
/// production impl wraps `ConnectionManager.get_state`; tests use an
/// in-memory map.
///
/// Kept as a trait so the listener can be unit-tested without spinning up a
/// real `ConnectionManager` or RwLock<SessionState>.
#[async_trait]
pub trait ParentSessionLookup: Send + Sync {
    async fn current_conversation_id(&self, parent_connection_id: &str) -> Option<i32>;
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
        }
    }
}

#[derive(Default)]
pub struct TokenRegistry {
    inner: RwLock<HashMap<String, TokenEntry>>,
    continuation_coordinator: OnceLock<Arc<DelegationContinuationCoordinator>>,
}

impl TokenRegistry {
    pub fn with_continuation_coordinator(
        coordinator: Arc<DelegationContinuationCoordinator>,
    ) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            continuation_coordinator: OnceLock::from(coordinator),
        }
    }

    fn continuation_coordinator(&self) -> Option<Arc<DelegationContinuationCoordinator>> {
        self.continuation_coordinator.get().cloned()
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
        let metrics = broker.metrics();
        Arc::new(Self {
            broker,
            tokens,
            leases,
            parent_lookup,
            feedback,
            questions,
            session_info,
            metrics,
        })
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
        let resp = match msg {
            BrokerMessage::Ready(_) => unreachable!("handled above"),
            BrokerMessage::Call(req) => report_response(self.process(req).await)?,
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
                    Ok(batch) => status_response(batch)?,
                    Err(_) => value_response(&StatusErrorEnvelope::continuation_arm_failed())?,
                }
            }
            BrokerMessage::CancelTask(req) => report_response(self.process_cancel_task(req).await)?,
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
    ) -> Result<DelegationStatusBatch, ContinuationError> {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            let unknown_reports: Vec<_> =
                req.task_ids.iter().map(|id| unknown_report(id)).collect();
            return Ok(match req.return_when {
                None => DelegationStatusBatch::legacy(unknown_reports),
                Some(_) => DelegationStatusBatch::joined(
                    unknown_reports,
                    DelegationWakeReason::Unavailable,
                    Vec::new(),
                ),
            });
        };
        // Connection-bound capability: a legacy token must not enter Join or
        // consult Broker ownership even if raw socket JSON sends return_when.
        if req.return_when.is_some() && !entry.coordination_v1 {
            return Ok(DelegationStatusBatch::joined(
                req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                DelegationWakeReason::Unavailable,
                Vec::new(),
            ));
        }
        let parent_conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        match req.return_when {
            None => Ok(DelegationStatusBatch::legacy(
                self.broker
                    .get_tasks_status(
                        &entry.parent_connection_id,
                        parent_conversation_id,
                        &req.task_ids,
                        legacy_wait_from(req.wait_ms),
                    )
                    .await,
            )),
            Some(DelegationReturnWhen::AllTerminalOrAttention) if req.wait_ms == Some(0) => {
                if !entry.delegation_continuation_v1 {
                    return Ok(self
                        .broker
                        .join_tasks_status(
                            &entry.parent_connection_id,
                            parent_conversation_id,
                            &req.task_ids,
                        )
                        .await);
                }
                let Some(parent_conversation_id) = parent_conversation_id else {
                    return Ok(DelegationStatusBatch::joined(
                        req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                        DelegationWakeReason::Unavailable,
                        Vec::new(),
                    ));
                };
                if let crate::acp::delegation::broker::JoinEvaluation::Ready(batch) = self
                    .broker
                    .evaluate_join_snapshot(
                        &entry.parent_connection_id,
                        parent_conversation_id,
                        &req.task_ids,
                    )
                    .await
                {
                    return Ok(batch);
                }
                let coordinator = self
                    .tokens
                    .continuation_coordinator()
                    .ok_or(ContinuationError::ArmWorkerDropped)?;
                let waiter_closed = CancellationToken::new();
                let _cancel_waiter_on_drop = CancelWaiterOnDrop(waiter_closed.clone());
                let request = JoinArmRequest {
                    parent_connection_id: entry.parent_connection_id,
                    parent_conversation_id,
                    task_ids: req.task_ids,
                    waiter_closed,
                };
                let arm_task = tokio::spawn(async move {
                    match coordinator.begin_arm_from_join(request).await? {
                        JoinArmOutcome::Immediate(batch) => {
                            Ok::<ArmStatus, ContinuationError>(ArmStatus::Immediate(batch))
                        }
                        JoinArmOutcome::Arming { completion, .. } => {
                            completion
                                .await
                                .map_err(|_| ContinuationError::ArmWorkerDropped)??;
                            Ok::<ArmStatus, ContinuationError>(ArmStatus::Suspended)
                        }
                    }
                });
                let status = arm_task
                    .await
                    .map_err(|_| ContinuationError::ArmWorkerDropped)??;
                match status {
                    ArmStatus::Immediate(batch) => Ok(batch),
                    ArmStatus::Suspended => {
                        std::future::pending::<()>().await;
                        drop(_cancel_waiter_on_drop);
                        unreachable!("the suspended canonical Join never returns")
                    }
                }
            }
            Some(_) => Ok(DelegationStatusBatch::joined(
                req.task_ids.iter().map(|id| unknown_report(id)).collect(),
                DelegationWakeReason::Unavailable,
                Vec::new(),
            )),
        }
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

        let delegation_req = DelegationRequest {
            parent_connection_id: req.parent_connection_id,
            parent_conversation_id,
            parent_tool_use_id: req.parent_tool_use_id,
            agent_type,
            profile_id,
            task,
            working_dir,
            requested_working_dir,
            external_handle: req.external_handle,
        };
        self.broker.start_delegation(delegation_req).await
    }
}

/// Serialize a [`DelegationTaskReport`] into a [`BrokerResponse`] for the wire.
/// Used by the `Call` / `CancelTask` arms, which each resolve to one report.
fn report_response(report: DelegationTaskReport) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&report).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
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

/// Serialize a [`DelegationStatusBatch`] for the `Status` arm. Legacy batches
/// omit Join fields; Join batches include `wake_reason` and
/// `attention_requests`.
fn status_response(batch: DelegationStatusBatch) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(batch).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("encode status batch: {error}"),
            )
        })?,
    })
}

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
    }
}

/// A `Failed` report carrying a wire-stable `error_code` for a bad argument.
fn report_failed(error_code: &str, message: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: None,
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
    }
}

/// An `Unknown` report — used when a status/cancel request fails the token
/// check (we don't leak whether the task exists).
fn unknown_report(task_id: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
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
    }
}

fn timeout_cancel_guidance_report(task_id: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
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
    use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationConfig};
    use crate::acp::delegation::continuation::coordinator::{
        ContinuationError, ContinuationPromptRequest, DelegationContinuationCoordinator,
        ParentContinuationPort, ParentTurnSnapshot, PromptAdmissionResult, SuspendRequest,
        SystemContinuationClock,
    };
    use crate::acp::delegation::continuation::store::{
        ContinuationStore, InMemoryContinuationStore,
    };
    use crate::acp::delegation::continuation::types::{
        ContinuationFailureCode, ContinuationState, ContinuationWaitingProjection,
    };
    use crate::acp::delegation::spawner::{
        accepted, mock::MockSpawner, ConnectionSpawner, SpawnerError,
    };
    use chrono::Utc;
    use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationSuccess};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::duplex;

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

    struct ContinuationTestPort {
        snapshot_calls: AtomicUsize,
        snapshot_entered: std::sync::Mutex<Option<oneshot::Sender<()>>>,
        snapshot_release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
        suspend_entered: std::sync::Mutex<Option<oneshot::Sender<SuspendRequest>>>,
        suspend_release: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
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
            _request: ContinuationPromptRequest,
        ) -> Result<PromptAdmissionResult, ContinuationError> {
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

    fn continuation_registry(
        broker: Arc<DelegationBroker>,
        store: Arc<InMemoryContinuationStore>,
        port: Arc<ContinuationTestPort>,
    ) -> (Arc<TokenRegistry>, Arc<DelegationContinuationCoordinator>) {
        let coordinator = Arc::new(DelegationContinuationCoordinator::new(
            store as Arc<dyn ContinuationStore>,
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

    fn continuation_token_entry(enabled: bool) -> TokenEntry {
        TokenEntry {
            parent_connection_id: "parent-conn".into(),
            working_dir: PathBuf::from("/tmp"),
            coordination_v1: true,
            delegation_continuation_v1: enabled,
            role: CompanionRole::Root,
        }
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
        BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "parent-conn".into(),
            parent_tool_use_id: "pt-1".into(),
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
    async fn continuation_capability_off_keeps_existing_parked_join() {
        let broker = make_broker(Arc::new(MockSpawner::new())).await;
        let task_id = broker
            .seed_live_task_for_test("parent-conn", "continuation-off-running")
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register("tok".into(), continuation_token_entry(false))
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));

        let join = tokio::spawn(async move {
            listener
                .process_status(BrokerStatusRequest {
                    token: "tok".into(),
                    task_ids: vec![task_id],
                    wait_ms: Some(0),
                    return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !join.is_finished(),
            "capability-off Join must retain the existing parked listener behavior"
        );

        complete_running_task(&broker, "continuation-off-running").await;
        let batch = join.await.unwrap().unwrap();
        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::AllTerminal));
        assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
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
            })
            .await
            .unwrap();

        assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
        assert_eq!(batch.tasks[0].status, TaskStatus::Unknown);

        let bound_listener = make_listener(broker, tokens, Some(1));
        let invalid_batch = bound_listener
            .process_status(BrokerStatusRequest {
                token: "tok".into(),
                task_ids: vec!["missing".into(), "foreign-running".into()],
                wait_ms: Some(0),
                return_when: Some(DelegationReturnWhen::AllTerminalOrAttention),
            })
            .await
            .unwrap();
        assert_eq!(
            invalid_batch.wake_reason,
            Some(DelegationWakeReason::Unavailable)
        );
        assert!(
            invalid_batch
                .tasks
                .iter()
                .all(|task| task.status == TaskStatus::Unknown)
        );
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
            }),
        )
        .await
        .expect("legacy-token Join rejection must not park")
        .unwrap();
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

    #[tokio::test]
    async fn continuation_status_peer_close_leaves_children_running() {
        assert_status_peer_close_leaves_children_running().await;
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
            listener_primary.serve_one(&mut primary_server).await.unwrap();
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

    use crate::acp::delegation::attention::{
        mock::MemoryDelegationAttentionStore, AttentionResolutionCode, DelegationAttentionStore,
    };
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
        }
    }

    fn root_token_entry(conn: &str) -> TokenEntry {
        TokenEntry {
            parent_connection_id: conn.to_string(),
            working_dir: PathBuf::from("/tmp"),
            coordination_v1: true,
            delegation_continuation_v1: false,
            role: CompanionRole::Root,
        }
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
            if let Ok(open) = attention.list_open_for_tasks(11, &[task_id.to_string()]).await {
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
        assert!(early.is_err(), "ParentDecision must remain pending until reply");

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
            .list_open_for_tasks(11, &[task_id.clone()])
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
            .list_open_for_tasks(11, &[task_id.clone()])
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
            read_frame::<_, BrokerResponse>(&mut parent_client).await.unwrap()
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
                .list_open_for_tasks(2, &[grandchild.clone()])
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
            if let Ok(open) = attention.list_open_for_tasks(1, &[root_child.clone()]).await {
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
}
