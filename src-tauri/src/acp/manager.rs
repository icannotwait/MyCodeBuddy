use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ActiveValue::Set, DatabaseConnection, EntityTrait,
    TransactionTrait,
};

#[cfg(any(test, feature = "test-utils"))]
use crate::acp::connection::{connection_channel, matching_config_pair};
use crate::acp::connection::{
    spawn_agent_connection, AgentConnection, ConnectionCommand, ConnectionControl,
    GoalControlAction, LaneSender, RegisteredSpawnAttempt, RouteBootstrapOutcome, SpawnHandshake,
    SuspensionAck,
};
use crate::acp::delegation::continuation::build_continuation_prompt_text;
use crate::acp::delegation::continuation::coordinator::{
    ContinuationError, ContinuationPromptRequest, PromptAdmissionResult,
};
use crate::acp::delegation::continuation::store::{
    ContinuationPatch, ContinuationStore, FieldPatch,
};
use crate::acp::delegation::continuation::types::ContinuationState;
use crate::acp::delegation::metrics::PromptAdmissionSource;
use crate::acp::delegation::route::{
    safe_native_fallback, DelegationConnectionOrigin, DelegationRoutePlan, DelegationRoutePolicy,
    DelegationRouteSource, RouteDegradedReason,
};
use crate::acp::delegation::workflow::{
    require_writable_conversation_workflow, WorkflowStoreError,
};
use crate::acp::error::AcpError;
use crate::acp::feedback::{
    bounded_feedback_batch, FeedbackItem, FeedbackStatus, PendingFeedback, SessionFeedbackAccess,
    MAX_FEEDBACK_CHARS, MAX_FEEDBACK_RESPONSE_BYTES,
};
use crate::acp::plan_approval::{
    PlanApprovalAnswer, RegisteredPlanApproval, SessionPlanApprovalAccess,
};
use crate::acp::question::{
    build_outcome, QuestionAnswer, QuestionOutcome, QuestionSpec,
    RecoveryQuestionRegistrationError, RegisteredQuestion, SessionQuestionAccess,
};
use crate::acp::session_state::{ActiveTurnContext, InternalPromptAdmission, SessionState};
use crate::acp::shared_session::{
    DispatchHeadDecision, PromptEnqueueResult, RegisteredReplacementPermit,
    SharedConfigConflictKind, SharedInteractionKind, SharedInteractionRequest,
    SharedLaunchIdentity, SharedLifecycleState, SharedMutationGuard, SharedPromptAdmission,
    SharedPromptRequest, SharedReserveRequest, SharedRuntimeWorkSnapshot, SharedSessionAttachment,
    SharedSessionBroker, SharedSessionDiagnostic, SharedSessionError, SharedSessionKey,
    SharedSessionPhase, SharedSessionProjection, SharedStopClaimDecision, SharedStopRequest,
    SharedSweepCandidateKind, SharedSweepReport, StopAdmissionResolution,
};
use crate::acp::terminal_context::{finalize_acp_launch_config, AcpLaunchConfig, AcpLaunchInputs};
use crate::acp::termination::AcpDisconnectOrigin;
use crate::acp::types::{
    AcpEvent, AgentOptionsSnapshot, ConfigStaleKind, ConnectionInfo, ConnectionStatus,
    ForkResultInfo, PromptCapabilitiesInfo, PromptInputBlock,
};
use crate::auto_title::{
    capture_prompt_context, ConnectionLaunchContext, ConnectionPurpose, PromptCaptureContext,
};
use crate::db::entities::conversation::{self, ConversationKind, ConversationStatus};
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::models::system::AppLocale;
use crate::web::event_bridge::{emit_with_state, emit_with_state_gated, EventEmitter};

pub(crate) async fn dispatch_suspension_control(
    control_tx: LaneSender<ConnectionControl>,
    continuation_id: impl Into<String>,
    parent_turn_generation: u64,
) -> Result<SuspensionAck, ContinuationError> {
    let (reply, receiver) = tokio::sync::oneshot::channel();
    control_tx
        .send(ConnectionControl::SuspendForDelegation {
            continuation_id: continuation_id.into(),
            parent_turn_generation,
            reply,
        })
        .await
        .map_err(|_| ContinuationError::SuspendDispatch(AcpError::ProcessExited))?;
    match receiver.await {
        Err(_) => Err(ContinuationError::ParentConnectionLost),
        Ok(Ok(ack)) => Ok(ack),
        Ok(Err(AcpError::ProcessExited)) => Err(ContinuationError::ParentConnectionLost),
        Ok(Err(AcpError::Protocol(code))) => match code.as_str() {
            "suspend_no_active_turn"
            | "suspend_turn_generation_mismatch"
            | "suspend_already_pending"
            | "suspend_session_fence_mismatch"
            | "suspend_turn_ended_before_cancel" => Err(ContinuationError::SuspendRejected(code)),
            "suspend_drain_timeout" => Err(ContinuationError::SuspendDrainTimeout),
            "suspend_parent_disconnected" | "suspend_prompt_response_failed" => {
                Err(ContinuationError::ParentConnectionLost)
            }
            "suspend_cancelled_by_user" => Err(ContinuationError::ParentStopRequested),
            _ => Err(ContinuationError::ParentConnectionLost),
        },
        Ok(Err(error)) => Err(ContinuationError::SuspendDispatch(error)),
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn test_control_sender() -> LaneSender<ConnectionControl> {
    // Synthetic connections have no conversation loop. Retain their bounded
    // receivers so manager control sends preserve the prior enqueue contract.
    static RECEIVERS: std::sync::OnceLock<
        std::sync::Mutex<Vec<tokio::sync::mpsc::Receiver<ConnectionControl>>>,
    > = std::sync::OnceLock::new();
    let (tx, rx, _liveness_rx) = connection_channel(32);
    RECEIVERS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .expect("test control receiver lock")
        .push(rx);
    tx
}

/// Cap on the number of prompt-text chars kept in the `user_prompt_sent`
/// preview. Past this, `truncate_str` keeps this many chars and appends a short
/// `...` marker (so the rendered string can be a few chars longer). Bounds the
/// event payload so a large paste can't bloat the ring buffer, the per-channel
/// IM message, or the webhook body.
const USER_PROMPT_PREVIEW_MAX_CHARS: usize = 500;

/// Production primary wait for map absence after unexposed teardown.
const TEARDOWN_MAP_WAIT_PRIMARY: Duration = Duration::from_secs(5);
/// Production extended wait after primary timeout before fail-closed.
const TEARDOWN_MAP_WAIT_EXTENDED: Duration = Duration::from_secs(2);

/// Launch policy for delegated children. Built only by the spawn-owned parent
/// launch snapshot resolver that `ConnectionManagerSpawner::spawn` consumes.
fn delegation_launch_context(parent_effective_locale: AppLocale) -> ConnectionLaunchContext {
    ConnectionLaunchContext {
        purpose: ConnectionPurpose::Delegation,
        inherited_locale: Some(parent_effective_locale),
    }
}

/// Launch policy for `probe_agent_options`. Must stay in lockstep with that
/// call site — the unit test exercises this helper as the production policy.
/// Internal probes have no user/channel locale; connection launch falls back
/// to effective English when `inherited_locale` is `None`.
fn internal_probe_launch_context() -> ConnectionLaunchContext {
    ConnectionLaunchContext {
        purpose: ConnectionPurpose::InternalProbe,
        inherited_locale: None,
    }
}

/// Residual close predicate: only idle Connected sessions (no pending
/// permission, no active background work) may be reaped. Extracted so TOCTOU
/// revalidation under the removal lock shares the same rule as unit tests.
fn is_idle_for_residual(state: &SessionState, now: chrono::DateTime<chrono::Utc>) -> bool {
    state.status == ConnectionStatus::Connected
        && state.pending_permission.is_none()
        && !state.has_active_background_work(now)
}

/// Grace window `disconnect_all` waits after firing every `Disconnect` before
/// hard-killing surviving agent process trees. Long enough for a driver thread
/// to unwind and run its own post-loop cleanup (delegation/question/plan-approval
/// reclaim), short enough not to stall a quit noticeably. It is NOT there to
/// make the agent's death gentler — the graceful path ends in the same
/// `kill_tree`.
const DISCONNECT_ALL_GRACE: Duration = Duration::from_millis(500);

/// True for ids in the parsers' turn-id namespace (`turn-<digits>`), which every
/// parser assigns via `format!("turn-{}", n)`. A broadcast `message_id` must
/// never land here: it would collide with a persisted transcript turn id and let
/// id-keyed cross-client dedup suppress or hide a prompt. Used to reject an
/// untrusted client-supplied `message_id` of that shape.
fn is_reserved_turn_id(id: &str) -> bool {
    matches!(id.strip_prefix("turn-"), Some(rest)
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Prefer shell drift over route drift over agent drift so the banner wording
/// matches the highest-priority surface the user still needs to reapply.
/// When all components match spawn, returns `None` (not stale).
fn effective_stale_kind(conn: &AgentConnection) -> Option<ConfigStaleKind> {
    let observed = &conn.observed_config.fingerprint;
    if observed.terminal_shell != conn.spawn_config.terminal_shell {
        Some(ConfigStaleKind::TerminalShell)
    } else if observed.delegation_route != conn.spawn_config.delegation_route {
        Some(ConfigStaleKind::DelegationRoute)
    } else if observed.agent_config != conn.spawn_config.agent_config {
        Some(conn.observed_config.agent_kind)
    } else {
        None
    }
}

/// Session-id dedup reuses only when the route fingerprint matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteReuseDecision {
    Reuse,
    Conflict { existing_connection_id: String },
}

fn route_reuse_decision(
    existing_fingerprint: &str,
    requested_fingerprint: &str,
    existing_connection_id: &str,
) -> RouteReuseDecision {
    if existing_fingerprint == requested_fingerprint {
        RouteReuseDecision::Reuse
    } else {
        RouteReuseDecision::Conflict {
            existing_connection_id: existing_connection_id.to_string(),
        }
    }
}

/// Pure spawn-policy inputs for unit-testing the max-two-attempt fallback
/// without real Agent binaries. Production `spawn_agent` inlines the same
/// match arms against live `spawn_agent_connection` outcomes.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone)]
pub struct SpawnAttemptRequest {
    pub origin: DelegationConnectionOrigin,
    pub plan: DelegationRoutePlan,
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
pub struct SpawnAttemptResult {
    pub connection_id: String,
    pub plan: DelegationRoutePlan,
}

#[cfg(any(test, feature = "test-utils"))]
pub struct SpawnAttemptHarness {
    outcomes: std::sync::Mutex<std::vec::IntoIter<Result<String, RouteBootstrapOutcome>>>,
    attempts: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-utils"))]
impl SpawnAttemptHarness {
    pub fn new(outcomes: impl IntoIterator<Item = Result<String, RouteBootstrapOutcome>>) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into_iter().collect::<Vec<_>>().into_iter()),
            attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn attempt_count(&self) -> usize {
        self.attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn spawn_once(
        &self,
        _plan: &DelegationRoutePlan,
    ) -> Result<String, RouteBootstrapOutcome> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.outcomes
            .lock()
            .unwrap()
            .next()
            .unwrap_or(Err(RouteBootstrapOutcome::Fatal(AcpError::ProcessExited)))
    }
}

/// Explicit max-two-attempt policy: root may retry once on RouteSpecific;
/// child never falls back; Fatal never retries. Second attempt cannot recurse.
#[cfg(any(test, feature = "test-utils"))]
pub async fn spawn_with_safe_fallback(
    request: SpawnAttemptRequest,
    harness: &SpawnAttemptHarness,
) -> Result<SpawnAttemptResult, AcpError> {
    let requested_plan = request.plan;
    match harness.spawn_once(&requested_plan).await {
        Ok(connection_id) => Ok(SpawnAttemptResult {
            connection_id,
            plan: requested_plan,
        }),
        Err(RouteBootstrapOutcome::RouteSpecific(reason))
            if request.origin == DelegationConnectionOrigin::Root =>
        {
            // teardown_unexposed_attempt is a no-op in the harness (no process).
            let fallback = safe_native_fallback(&requested_plan, reason);
            match harness.spawn_once(&fallback).await {
                Ok(id) => Ok(SpawnAttemptResult {
                    connection_id: id,
                    plan: fallback,
                }),
                Err(outcome) => Err(outcome.into_acp_error()),
            }
        }
        Err(RouteBootstrapOutcome::RouteSpecific(reason)) => {
            Err(AcpError::RouteUnavailable { reason })
        }
        Err(RouteBootstrapOutcome::Fatal(error)) => Err(error),
        Err(RouteBootstrapOutcome::Ready) => {
            Err(AcpError::Protocol("unexpected Ready as spawn error".into()))
        }
    }
}

/// Build the bounded preview string for a `user_prompt_sent` notification from
/// the `Text` blocks of a user prompt. Joins the (trimmed, non-empty) text
/// blocks with a space and caps the kept text at `USER_PROMPT_PREVIEW_MAX_CHARS`
/// chars (a `...` marker is appended past the cap). Returns `None` when the
/// prompt carries no text (e.g. image-only) — the notification fires for text
/// messages only.
fn user_prompt_text_preview(blocks: &[PromptInputBlock]) -> Option<String> {
    let joined = blocks
        .iter()
        .filter_map(|b| match b {
            PromptInputBlock::Text { text } => {
                let t = text.trim();
                (!t.is_empty()).then_some(t)
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(crate::parsers::truncate_str(
            trimmed,
            USER_PROMPT_PREVIEW_MAX_CHARS,
        ))
    }
}

/// Seed title for a freshly-created delegation child row, derived from the
/// delegating prompt's text blocks (the sub-agent's task). Uses the parser's own
/// `title_from_user_text` (folds reference links, caps at 100 chars) so the value
/// matches what `refresh_auto_title` would later compute from that same first
/// turn — the conditional UPDATE then sees no change and doesn't churn. Returns
/// `None` for a textless prompt, leaving the title unset to be backfilled on
/// first detail load as before. Kept unlocked by the caller so an AI-generated
/// title can still replace it later.
fn delegation_child_title_seed(blocks: &[PromptInputBlock]) -> Option<String> {
    let joined = blocks
        .iter()
        .filter_map(|b| match b {
            PromptInputBlock::Text { text } => {
                let t = text.trim();
                (!t.is_empty()).then_some(t)
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(crate::parsers::title_from_user_text(trimmed))
    }
}

/// Composite key identifying a logical agent session for spawn-time dedup.
/// Two `acp_connect` calls with the same triple race for the same `Mutex`,
/// so the second one observes the first's freshly-spawned connection in
/// `find_connection_for_reuse` instead of starting a duplicate process.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SpawnDedupKey {
    agent_type: AgentType,
    working_dir: Option<PathBuf>,
    session_id: String,
}

/// Whether a session-dedup hit may be returned for a detached cold connect
/// that requested `want_op` under `want_label`.
///
/// Only same-incarnation reuse is safe: otherwise the frontend would record a
/// cold lease for a connection still owned by main (or another op).
pub(crate) fn cold_connect_reuse_allowed(
    existing_label: &str,
    existing_op: Option<&str>,
    want_label: &str,
    want_op: &str,
) -> bool {
    existing_op == Some(want_op) && existing_label == want_label
}

/// Default upper bound on how long `spawn_agent` will hold the per-session
/// dedup lock waiting for `SessionStarted`. Picked to comfortably cover
/// cold-start agents (claude-code/codex warm: <2s; npx-fetched cold: 10–30s)
/// without deadlocking the next concurrent acp_connect when an agent is
/// genuinely broken.
pub(crate) const SPAWN_HANDSHAKE_TIMEOUT_SECS: u64 = 60;

/// Read the spawn-handshake timeout from `CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS`,
/// falling back to `SPAWN_HANDSHAKE_TIMEOUT_SECS`. Returns the configured
/// `Duration`.
fn spawn_handshake_timeout_from_env() -> Duration {
    let secs = std::env::var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(SPAWN_HANDSHAKE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Outcome of the `spawn_agent` dedup wait. Logged so production can audit
/// how often the timeout fires vs. the agent handshake completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandshakeWaitOutcome {
    /// `SessionStarted` applied; `external_id` is now set on the state.
    Ready,
    /// Sender was dropped before SessionStarted fired (typically the
    /// connection died during init — `run_connection` returned Err).
    Aborted,
    /// Timeout elapsed before either of the above. Releases the dedup lock
    /// so the next caller can proceed; the slow agent is no worse off.
    TimedOut,
}

impl HandshakeWaitOutcome {
    fn as_str(self) -> &'static str {
        match self {
            HandshakeWaitOutcome::Ready => "ready",
            HandshakeWaitOutcome::Aborted => "aborted",
            HandshakeWaitOutcome::TimedOut => "timeout",
        }
    }
}

/// Wait for the spawn-time `SessionStarted` signal, bounded by `timeout`.
/// Extracted so the outcome enum can be unit-tested without spawning a
/// real agent process.
async fn wait_for_session_started(
    rx: tokio::sync::oneshot::Receiver<()>,
    timeout: Duration,
) -> (HandshakeWaitOutcome, Duration) {
    let start = std::time::Instant::now();
    let outcome = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(())) => HandshakeWaitOutcome::Ready,
        Ok(Err(_)) => HandshakeWaitOutcome::Aborted,
        Err(_) => HandshakeWaitOutcome::TimedOut,
    };
    (outcome, start.elapsed())
}

#[derive(Clone)]
pub struct SharedConnectLaunch {
    pub database: sea_orm::DatabaseConnection,
    pub key: SharedSessionKey,
    pub conversation_id: Option<i32>,
    pub folder_id: Option<i32>,
    pub launch_identity: SharedLaunchIdentity,
    pub agent_type: AgentType,
    pub working_dir: Option<String>,
    pub external_session_id: Option<String>,
    pub launch_inputs: AcpLaunchInputs,
    pub emitter: EventEmitter,
    pub preferred_mode_id: Option<String>,
    pub preferred_config_values: BTreeMap<String, String>,
    pub launch_context: ConnectionLaunchContext,
    pub session_attach_mode: crate::acp::session_attach::SessionAttachMode,
    pub device_id: String,
    pub client_instance_id: String,
    pub request_id: String,
    pub retry_failed_generation: Option<u64>,
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait::async_trait]
pub trait SharedSpawnDriver: Send + Sync {
    async fn start(
        &self,
        connection_id: String,
        launch: SharedConnectLaunch,
        existing_public_state: Option<Arc<RwLock<SessionState>>>,
    ) -> Result<RegisteredSpawnAttempt, AcpError>;
}

#[derive(Debug, PartialEq, Eq)]
enum SharedBootstrapAction {
    Ready,
    AllowedFallback(RouteDegradedReason),
    Fail(SharedSessionError),
}

fn map_route_failure(reason: RouteDegradedReason) -> SharedSessionError {
    match reason {
        RouteDegradedReason::NativeSuppressionUnsupported
        | RouteDegradedReason::NativeSuppressionInvalid
        | RouteDegradedReason::CompanionBinaryUnavailable
        | RouteDegradedReason::AgentMcpUnsupported
        | RouteDegradedReason::CompanionInitializationFailed => {
            SharedSessionError::CompanionInitializationFailed
        }
    }
}

fn shared_bootstrap_action(
    plan: &DelegationRoutePlan,
    outcome: RouteBootstrapOutcome,
) -> SharedBootstrapAction {
    match outcome {
        RouteBootstrapOutcome::Ready => SharedBootstrapAction::Ready,
        RouteBootstrapOutcome::RouteSpecific(reason)
            if plan.requested == DelegationRoutePolicy::Codeg
                && plan.source == DelegationRouteSource::SessionOverride =>
        {
            SharedBootstrapAction::Fail(map_route_failure(reason))
        }
        RouteBootstrapOutcome::RouteSpecific(reason)
            if plan.managed
                && plan.effective == DelegationRoutePolicy::Codeg
                && plan.source == DelegationRouteSource::GlobalDefault =>
        {
            SharedBootstrapAction::AllowedFallback(reason)
        }
        RouteBootstrapOutcome::RouteSpecific(reason) => {
            SharedBootstrapAction::Fail(map_route_failure(reason))
        }
        RouteBootstrapOutcome::Fatal(_) => {
            SharedBootstrapAction::Fail(SharedSessionError::SessionUnavailable)
        }
    }
}

fn shared_projection(
    generation: u64,
    phase: SharedSessionPhase,
    lease_expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> SharedSessionProjection {
    SharedSessionProjection {
        generation,
        phase,
        queue: Vec::new(),
        active_turn: None,
        lease_expires_at,
        expired_lease_tombstone_count: 0,
    }
}

fn shared_registration_error(error: AcpError) -> SharedSessionError {
    match error {
        AcpError::Shared(error) => error,
        AcpError::RouteUnavailable { .. } => SharedSessionError::CompanionInitializationFailed,
        _ => SharedSessionError::SessionUnavailable,
    }
}

fn stable_dispatch_error(error: &AcpError) -> &'static str {
    match error.code() {
        Some(
            code @ ("workflow_v2_retired"
            | "legacy_completion_protocol_read_only"
            | "unsupported_completion_protocol"
            | "workflow_identity_corrupt"
            | "delegate_viewer_only"),
        ) => code,
        _ => "session_unavailable",
    }
}

fn stable_conversation_write_error(error: &WorkflowStoreError) -> &'static str {
    match error.code() {
        code @ ("workflow_v2_retired"
        | "workflow_identity_corrupt"
        | "legacy_completion_protocol_read_only"
        | "unsupported_completion_protocol") => code,
        _ => "session_unavailable",
    }
}

#[cfg(test)]
#[derive(Clone)]
struct DisconnectFinalCasHook {
    reached: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct AdmissionInsertHold {
    reached: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct RebindAfterSnapshotHook {
    reached: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct SharedEnqueueFinalizeHook {
    reached: Arc<tokio::sync::Barrier>,
    resume: Arc<tokio::sync::Barrier>,
}

#[cfg(test)]
#[derive(Clone)]
struct SharedEnqueuePublicationHook {
    reached: Arc<tokio::sync::Barrier>,
    resume: Arc<tokio::sync::Barrier>,
}

pub(crate) enum SharedControlAdmissionError {
    DefinitelyNotAdmitted(AcpError),
    MayHaveBeenAdmitted(AcpError),
    InteractionAlreadyResolved { local_error: Option<AcpError> },
}

impl SharedControlAdmissionError {
    fn into_error(self) -> AcpError {
        match self {
            Self::DefinitelyNotAdmitted(error) | Self::MayHaveBeenAdmitted(error) => error,
            Self::InteractionAlreadyResolved { local_error } => local_error.unwrap_or(
                AcpError::Shared(SharedSessionError::InteractionAlreadyResolved),
            ),
        }
    }

    fn into_local_result(self) -> Result<(), AcpError> {
        match self {
            Self::DefinitelyNotAdmitted(error) | Self::MayHaveBeenAdmitted(error) => Err(error),
            Self::InteractionAlreadyResolved { local_error } => match local_error {
                Some(error) => Err(error),
                None => Ok(()),
            },
        }
    }
}

#[async_trait::async_trait]
pub(crate) trait SharedControlAdapter: Send + Sync {
    async fn cancel(
        &self,
        manager: &ConnectionManager,
        db: &DatabaseConnection,
        connection_id: &str,
        claim: &crate::acp::shared_session::SharedStopClaim,
    ) -> Result<(), SharedControlAdmissionError>;

    async fn respond_permission(
        &self,
        manager: &ConnectionManager,
        connection_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<(), SharedControlAdmissionError>;

    async fn answer_question(
        &self,
        manager: &ConnectionManager,
        connection_id: &str,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<(), SharedControlAdmissionError>;

    async fn answer_plan_approval(
        &self,
        manager: &ConnectionManager,
        connection_id: &str,
        approval_id: &str,
        answer: PlanApprovalAnswer,
    ) -> Result<(), SharedControlAdmissionError>;
}

struct ManagerSharedControlAdapter;

#[async_trait::async_trait]
impl SharedControlAdapter for ManagerSharedControlAdapter {
    async fn cancel(
        &self,
        manager: &ConnectionManager,
        db: &DatabaseConnection,
        connection_id: &str,
        claim: &crate::acp::shared_session::SharedStopClaim,
    ) -> Result<(), SharedControlAdmissionError> {
        manager
            .cancel_with_admission(db, connection_id, Some(claim))
            .await
    }

    async fn respond_permission(
        &self,
        manager: &ConnectionManager,
        connection_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<(), SharedControlAdmissionError> {
        manager
            .respond_permission_with_admission(connection_id, request_id, option_id)
            .await
    }

    async fn answer_question(
        &self,
        manager: &ConnectionManager,
        connection_id: &str,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<(), SharedControlAdmissionError> {
        manager
            .answer_question_with_admission(connection_id, question_id, answer)
            .await
    }

    async fn answer_plan_approval(
        &self,
        manager: &ConnectionManager,
        connection_id: &str,
        approval_id: &str,
        answer: PlanApprovalAnswer,
    ) -> Result<(), SharedControlAdmissionError> {
        manager
            .answer_plan_approval_with_admission(connection_id, approval_id, answer)
            .await
    }
}

struct AdmissionState {
    accepting: bool,
    in_flight: usize,
}

/// Process-wide ACP connection admission. Closed by
/// [`ConnectionManager::begin_shutdown`]; RAII permits keep in-flight
/// creates visible until they insert into the map or fail.
pub struct ConnectionAdmissionGate {
    state: std::sync::Mutex<AdmissionState>,
    drained: tokio::sync::Notify,
}

/// Held from the start of a create/spawn path until the connection is in
/// the manager map or the attempt fails.
pub struct ConnectionAdmissionPermit {
    gate: Arc<ConnectionAdmissionGate>,
}

impl ConnectionAdmissionGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(AdmissionState {
                accepting: true,
                in_flight: 0,
            }),
            drained: tokio::sync::Notify::new(),
        })
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, AdmissionState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn admit(self: &Arc<Self>) -> Result<ConnectionAdmissionPermit, AcpError> {
        let mut state = self.lock_state();
        if !state.accepting {
            return Err(AcpError::ServerShuttingDown);
        }
        state.in_flight += 1;
        Ok(ConnectionAdmissionPermit {
            gate: Arc::clone(self),
        })
    }

    fn ensure_accepting(&self) -> Result<(), AcpError> {
        if self.lock_state().accepting {
            Ok(())
        } else {
            Err(AcpError::ServerShuttingDown)
        }
    }

    fn close(&self) {
        let mut state = self.lock_state();
        state.accepting = false;
        let idle = state.in_flight == 0;
        drop(state);
        if idle {
            self.drained.notify_waiters();
        }
    }

    async fn close_and_wait(&self) {
        self.close();
        self.wait_until_idle().await;
    }

    async fn wait_until_idle(&self) {
        loop {
            let notified = self.drained.notified();
            if self.lock_state().in_flight == 0 {
                return;
            }
            notified.await;
        }
    }

    fn snapshot(&self) -> (bool, usize) {
        let state = self.lock_state();
        (state.accepting, state.in_flight)
    }

    fn release(&self) {
        let mut state = self.lock_state();
        state.in_flight = state.in_flight.saturating_sub(1);
        let notify = !state.accepting;
        drop(state);
        if notify {
            self.drained.notify_waiters();
        }
    }
}

impl Drop for ConnectionAdmissionPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

pub struct ConnectionManager {
    pub(crate) connections: Arc<Mutex<HashMap<String, AgentConnection>>>,
    /// Per-(agent, working_dir, session_id) async mutex. Held across the
    /// dedup-lookup + spawn + SessionStarted-wait critical section so two
    /// concurrent `spawn_agent` calls for the same logical session can't
    /// both miss dedup during the handshake window. Entries persist for
    /// process lifetime — bounded by the number of distinct sessions ever
    /// connected.
    spawn_locks: Arc<Mutex<HashMap<SpawnDedupKey, Arc<Mutex<()>>>>>,
    /// Bound on how long `spawn_agent` waits for the agent's handshake
    /// before releasing the dedup lock. Configurable per-instance for
    /// tests; in production initialized from env via
    /// `spawn_handshake_timeout_from_env`.
    spawn_handshake_timeout: Duration,
    /// Shared General Settings shell used by ACP terminal fallbacks.
    terminal_shell_config: crate::acp::terminal_runtime::TerminalShellRuntimeConfig,
    /// Host-owned tool-execution lease registry. Shared through every
    /// `clone_ref` and stamped onto each `AgentConnection` / `SessionState`.
    pub(crate) tool_lease_registry: Arc<crate::acp::tool_watchdog::ToolExecutionLeaseRegistry>,
    /// Secret-safe process-local tool-watchdog counters (agent + category labels).
    pub(crate) tool_watchdog_metrics: Arc<crate::acp::tool_watchdog::ToolWatchdogMetrics>,
    /// Coalescing wake for the production supervisor scan loop.
    pub(crate) tool_watchdog_wake: Arc<tokio::sync::Notify>,
    /// Serializes durable settings write + live registry apply so concurrent
    /// saves cannot leave persistence and the live registry divergent.
    pub(crate) tool_watchdog_settings_gate: Arc<tokio::sync::Mutex<()>>,
    /// Host-only multi-task wait cancel handles (never cancels child tasks).
    pub(crate) wait_cancel_registry: Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
    /// Host-only MCP request cancel tokens.
    pub(crate) mcp_cancel_registry: Arc<crate::acp::tool_watchdog::McpCancelRegistry>,
    /// Delegation broker + token registry + UDS path installed during app
    /// bootstrap (`install_delegation`). When present, `spawn_agent` propagates
    /// the injection to `spawn_agent_connection`, which makes
    /// `codeg-mcp` appear in the agent's MCP server list during ACP
    /// init. `Arc<OnceLock>` so the inner `Self` cloned from `clone_ref` sees
    /// the install too — the lock is set once at startup and never mutated.
    delegation_injection: Arc<std::sync::OnceLock<crate::acp::connection::DelegationInjection>>,
    /// Durable continuation ownership store installed once with the shared
    /// delegation runtime. The outer Arc keeps `clone_ref` clones on one slot.
    continuation_store: Arc<std::sync::OnceLock<Arc<dyn ContinuationStore>>>,
    /// Per-agent-type serialization for `probe_agent_options`. Without
    /// this, rapid agent-tab clicks in the settings UI would fan out one
    /// real CLI process per click — each one running up to 60s. The
    /// mutex bounds concurrent probes for the same agent_type to one;
    /// different agent_types remain parallel.
    probe_locks: Arc<Mutex<HashMap<AgentType, Arc<tokio::sync::Mutex<()>>>>>,
    /// In-flight `ask_user_question` calls awaiting the user's answer, keyed by
    /// the globally-unique `question_id`. The listener parks on the receiver;
    /// the answer / cancel path resolves (and removes) the matching sender.
    /// Shared across `clone_ref` clones so the listener-facing
    /// `register_question` and the command-facing `answer_question` touch the
    /// same map. Size tracks live concurrency (the agent is blocked per ask) —
    /// no cap, no cumulative growth; entries are removed on answer / cancel /
    /// connection teardown.
    pending_questions: Arc<Mutex<HashMap<String, PendingQuestionEntry>>>,
    /// In-flight Grok `exit_plan_mode` approvals awaiting the user's decision,
    /// keyed by the globally-unique `approval_id`. The connection's ext handler
    /// parks on the receiver; the answer / cancel path resolves (and removes) the
    /// matching sender. Shared across `clone_ref` clones so the connection-facing
    /// `register_plan_approval` and the command-facing `answer_plan_approval`
    /// touch the same map. At most one per connection (the agent is blocked in
    /// its `exit_plan_mode` call) — no cap, no cumulative growth.
    pending_plan_approvals: Arc<Mutex<HashMap<String, PendingPlanApprovalEntry>>>,
    recovery_authorization_service: Arc<
        std::sync::OnceLock<Arc<crate::acp::recovery_authorization::RecoveryAuthorizationService>>,
    >,
    shared_session_broker: SharedSessionBroker,
    shared_control_adapter: Arc<dyn SharedControlAdapter>,
    shared_launches: Arc<Mutex<HashMap<(String, u64), SharedConnectLaunch>>>,
    admission: Arc<ConnectionAdmissionGate>,
    #[cfg(any(test, feature = "test-utils"))]
    shared_spawn_override: Option<Arc<dyn SharedSpawnDriver>>,
    #[cfg(any(test, feature = "test-utils"))]
    shared_spawn_count: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(any(test, feature = "test-utils"))]
    shared_registered_root_count: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(any(test, feature = "test-utils"))]
    shared_teardown_count: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(any(test, feature = "test-utils"))]
    shared_fallback_trace: Arc<std::sync::Mutex<Vec<&'static str>>>,
    #[cfg(test)]
    shared_settler_panic_after_replacement: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    shared_settler_supervisor_completed: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    disconnect_final_cas_hook: Arc<std::sync::Mutex<Option<DisconnectFinalCasHook>>>,
    #[cfg(test)]
    rebind_after_snapshot_hook: Arc<std::sync::Mutex<Option<RebindAfterSnapshotHook>>>,
    #[cfg(test)]
    shared_enqueue_finalize_hook: Arc<std::sync::Mutex<Option<SharedEnqueueFinalizeHook>>>,
    #[cfg(test)]
    shared_enqueue_publication_hook: Arc<std::sync::Mutex<Option<SharedEnqueuePublicationHook>>>,
    #[cfg(test)]
    admission_insert_hold: Arc<std::sync::Mutex<Option<AdmissionInsertHold>>>,
    #[cfg(test)]
    stub_direct_spawn: Arc<std::sync::atomic::AtomicBool>,
}

/// A parked `ask_user_question` awaiting its answer. The `sender` resolves the
/// blocked listener round-trip; `questions` is retained so `answer_question` can
/// build the self-describing outcome without a `SessionState` read (race-free).
struct PendingQuestionEntry {
    parent_connection_id: String,
    questions: Vec<QuestionSpec>,
    sender: tokio::sync::oneshot::Sender<QuestionOutcome>,
    recovery_authorization_id: Option<String>,
    settling: bool,
}

enum QuestionSettlementClaim {
    Missing,
    InFlight,
    Claimed {
        questions: Vec<QuestionSpec>,
        recovery_authorization_id: Option<String>,
    },
}

/// A parked Grok `exit_plan_mode` approval awaiting the user's decision. The
/// `sender` resolves the blocked ext-request round-trip in the connection's
/// handler. `parent_connection_id` routes the resolved event + answer.
struct PendingPlanApprovalEntry {
    parent_connection_id: String,
    sender: tokio::sync::oneshot::Sender<PlanApprovalAnswer>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

async fn tool_watchdog_resume_from_state(
    state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
) {
    use crate::acp::tool_watchdog::WatchdogInstant;
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    attr.resume(&turn, WatchdogInstant::now()).await;
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            spawn_locks: Arc::new(Mutex::new(HashMap::new())),
            spawn_handshake_timeout: spawn_handshake_timeout_from_env(),
            terminal_shell_config: crate::acp::terminal_runtime::TerminalShellRuntimeConfig::new(),
            tool_lease_registry: Arc::new(
                crate::acp::tool_watchdog::ToolExecutionLeaseRegistry::new(
                    crate::acp::tool_watchdog::ToolWatchdogSettings::default(),
                ),
            ),
            tool_watchdog_metrics: Arc::new(
                crate::acp::tool_watchdog::ToolWatchdogMetrics::default(),
            ),
            tool_watchdog_wake: Arc::new(tokio::sync::Notify::new()),
            tool_watchdog_settings_gate: Arc::new(tokio::sync::Mutex::new(())),
            wait_cancel_registry:
                crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared(),
            mcp_cancel_registry: crate::acp::tool_watchdog::McpCancelRegistry::new_shared(),
            delegation_injection: Arc::new(std::sync::OnceLock::new()),
            continuation_store: Arc::new(std::sync::OnceLock::new()),
            probe_locks: Arc::new(Mutex::new(HashMap::new())),
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
            pending_plan_approvals: Arc::new(Mutex::new(HashMap::new())),
            recovery_authorization_service: Arc::new(std::sync::OnceLock::new()),
            shared_session_broker: SharedSessionBroker::default(),
            shared_control_adapter: Arc::new(ManagerSharedControlAdapter),
            shared_launches: Arc::new(Mutex::new(HashMap::new())),
            admission: ConnectionAdmissionGate::new(),
            #[cfg(any(test, feature = "test-utils"))]
            shared_spawn_override: None,
            #[cfg(any(test, feature = "test-utils"))]
            shared_spawn_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(any(test, feature = "test-utils"))]
            shared_registered_root_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(any(test, feature = "test-utils"))]
            shared_teardown_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(any(test, feature = "test-utils"))]
            shared_fallback_trace: Arc::new(std::sync::Mutex::new(Vec::new())),
            #[cfg(test)]
            shared_settler_panic_after_replacement: Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            #[cfg(test)]
            shared_settler_supervisor_completed: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            disconnect_final_cas_hook: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            rebind_after_snapshot_hook: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            shared_enqueue_finalize_hook: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            shared_enqueue_publication_hook: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            admission_insert_hold: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            stub_direct_spawn: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Returns a shallow clone sharing the same underlying connection map.
    pub fn clone_ref(&self) -> Self {
        Self {
            connections: self.connections.clone(),
            spawn_locks: self.spawn_locks.clone(),
            spawn_handshake_timeout: self.spawn_handshake_timeout,
            terminal_shell_config: self.terminal_shell_config.clone(),
            tool_lease_registry: self.tool_lease_registry.clone(),
            tool_watchdog_metrics: self.tool_watchdog_metrics.clone(),
            tool_watchdog_wake: self.tool_watchdog_wake.clone(),
            tool_watchdog_settings_gate: self.tool_watchdog_settings_gate.clone(),
            wait_cancel_registry: self.wait_cancel_registry.clone(),
            mcp_cancel_registry: self.mcp_cancel_registry.clone(),
            delegation_injection: self.delegation_injection.clone(),
            continuation_store: self.continuation_store.clone(),
            probe_locks: self.probe_locks.clone(),
            pending_questions: self.pending_questions.clone(),
            pending_plan_approvals: self.pending_plan_approvals.clone(),
            recovery_authorization_service: self.recovery_authorization_service.clone(),
            shared_session_broker: self.shared_session_broker.clone(),
            shared_control_adapter: self.shared_control_adapter.clone(),
            shared_launches: self.shared_launches.clone(),
            admission: self.admission.clone(),
            #[cfg(any(test, feature = "test-utils"))]
            shared_spawn_override: self.shared_spawn_override.clone(),
            #[cfg(any(test, feature = "test-utils"))]
            shared_spawn_count: self.shared_spawn_count.clone(),
            #[cfg(any(test, feature = "test-utils"))]
            shared_registered_root_count: self.shared_registered_root_count.clone(),
            #[cfg(any(test, feature = "test-utils"))]
            shared_teardown_count: self.shared_teardown_count.clone(),
            #[cfg(any(test, feature = "test-utils"))]
            shared_fallback_trace: self.shared_fallback_trace.clone(),
            #[cfg(test)]
            shared_settler_panic_after_replacement: self
                .shared_settler_panic_after_replacement
                .clone(),
            #[cfg(test)]
            shared_settler_supervisor_completed: self.shared_settler_supervisor_completed.clone(),
            #[cfg(test)]
            disconnect_final_cas_hook: self.disconnect_final_cas_hook.clone(),
            #[cfg(test)]
            rebind_after_snapshot_hook: self.rebind_after_snapshot_hook.clone(),
            #[cfg(test)]
            shared_enqueue_finalize_hook: self.shared_enqueue_finalize_hook.clone(),
            #[cfg(test)]
            shared_enqueue_publication_hook: self.shared_enqueue_publication_hook.clone(),
            #[cfg(test)]
            admission_insert_hold: self.admission_insert_hold.clone(),
            #[cfg(test)]
            stub_direct_spawn: self.stub_direct_spawn.clone(),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_with_shared_spawn_driver(driver: Arc<dyn SharedSpawnDriver>) -> Self {
        let mut manager = Self::new();
        manager.shared_spawn_override = Some(driver);
        manager
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_with_shared_spawn_driver_and_prompt_ledger_limit_for_test(
        driver: Arc<dyn SharedSpawnDriver>,
        max_prompt_ledger_entries: usize,
    ) -> Self {
        let mut manager = Self::new_with_shared_spawn_driver(driver);
        manager.shared_session_broker =
            SharedSessionBroker::with_prompt_ledger_limit_for_test(max_prompt_ledger_entries);
        manager
    }

    #[cfg(test)]
    pub(crate) fn new_with_shared_control_adapter(adapter: Arc<dyn SharedControlAdapter>) -> Self {
        let mut manager = Self::new();
        manager.shared_control_adapter = adapter;
        manager
    }

    pub fn shared_session_broker(&self) -> SharedSessionBroker {
        self.shared_session_broker.clone()
    }

    pub async fn shared_session_diagnostics(&self) -> Vec<SharedSessionDiagnostic> {
        self.shared_session_broker.diagnostics().await
    }

    pub async fn is_broker_managed_connection(&self, connection_id: &str) -> bool {
        self.shared_session_broker
            .is_managed_connection(connection_id)
            .await
    }

    pub async fn stop_shared_turn(
        &self,
        db: &DatabaseConnection,
        request: SharedStopRequest,
    ) -> Result<(), AcpError> {
        loop {
            match self
                .shared_session_broker
                .claim_stop_request(&request)
                .await?
            {
                SharedStopClaimDecision::Requested => return Ok(()),
                SharedStopClaimDecision::Resolving(mut resolution) => loop {
                    let current = *resolution.borrow();
                    match current {
                        Some(StopAdmissionResolution::Requested) => return Ok(()),
                        Some(StopAdmissionResolution::DefinitelyNotAdmitted) => break,
                        None => {
                            resolution.changed().await.map_err(|_| {
                                AcpError::Shared(SharedSessionError::SessionUnavailable)
                            })?;
                        }
                    }
                },
                SharedStopClaimDecision::Claimed(claim) => {
                    let manager = self.clone_ref();
                    let db = db.clone();
                    let connection_id = request.guard.connection_id.clone();
                    let recovery_claim = claim.clone();
                    let admission_task =
                        tokio::spawn(async move {
                            let admission = manager
                                .shared_control_adapter
                                .cancel(&manager, &db, &connection_id, &claim)
                                .await;
                            match admission {
                            Ok(()) => {
                                manager
                                    .shared_session_broker
                                    .complete_stop_request(&claim)
                                    .await?;
                                Ok(())
                            }
                            Err(SharedControlAdmissionError::DefinitelyNotAdmitted(error)) => {
                                manager
                                    .shared_session_broker
                                    .release_stop_request(&claim)
                                    .await?;
                                Err(error)
                            }
                            Err(SharedControlAdmissionError::MayHaveBeenAdmitted(error)) => {
                                manager
                                    .shared_session_broker
                                    .complete_stop_request(&claim)
                                    .await?;
                                Err(error)
                            }
                            Err(error @ SharedControlAdmissionError::InteractionAlreadyResolved {
                                ..
                            }) => {
                                manager
                                    .shared_session_broker
                                    .release_stop_request(&claim)
                                    .await?;
                                Err(error.into_error())
                            }
                        }
                        });
                    return match admission_task.await {
                        Ok(result) => result,
                        Err(error) => {
                            self.shared_session_broker
                                .complete_stop_request(&recovery_claim)
                                .await?;
                            Err(AcpError::protocol(error.to_string()))
                        }
                    };
                }
            }
        }
    }

    pub async fn respond_shared_permission(
        &self,
        request: SharedInteractionRequest<String>,
    ) -> Result<(), AcpError> {
        let claim = self
            .shared_session_broker
            .claim_interaction(
                &request.guard,
                SharedInteractionKind::Permission,
                &request.interaction_id,
            )
            .await?;
        let manager = self.clone_ref();
        let recovery_claim = claim.clone();
        let admission_task = tokio::spawn(async move {
            let admission = manager
                .shared_control_adapter
                .respond_permission(
                    &manager,
                    &request.guard.connection_id,
                    &request.interaction_id,
                    &request.answer,
                )
                .await;
            manager
                .finish_shared_interaction_admission(&claim, admission)
                .await
        });
        self.finish_shared_interaction_task(recovery_claim, admission_task)
            .await
    }

    pub async fn answer_shared_question(
        &self,
        request: SharedInteractionRequest<QuestionAnswer>,
    ) -> Result<(), AcpError> {
        let claim = self
            .shared_session_broker
            .claim_interaction(
                &request.guard,
                SharedInteractionKind::Question,
                &request.interaction_id,
            )
            .await?;
        let manager = self.clone_ref();
        let recovery_claim = claim.clone();
        let admission_task = tokio::spawn(async move {
            let admission = manager
                .shared_control_adapter
                .answer_question(
                    &manager,
                    &request.guard.connection_id,
                    &request.interaction_id,
                    request.answer,
                )
                .await;
            manager
                .finish_shared_interaction_admission(&claim, admission)
                .await
        });
        self.finish_shared_interaction_task(recovery_claim, admission_task)
            .await
    }

    pub async fn answer_shared_plan_approval(
        &self,
        request: SharedInteractionRequest<PlanApprovalAnswer>,
    ) -> Result<(), AcpError> {
        let claim = self
            .shared_session_broker
            .claim_interaction(
                &request.guard,
                SharedInteractionKind::PlanApproval,
                &request.interaction_id,
            )
            .await?;
        let manager = self.clone_ref();
        let recovery_claim = claim.clone();
        let admission_task = tokio::spawn(async move {
            let admission = manager
                .shared_control_adapter
                .answer_plan_approval(
                    &manager,
                    &request.guard.connection_id,
                    &request.interaction_id,
                    request.answer,
                )
                .await;
            manager
                .finish_shared_interaction_admission(&claim, admission)
                .await
        });
        self.finish_shared_interaction_task(recovery_claim, admission_task)
            .await
    }

    async fn finish_shared_interaction_task(
        &self,
        recovery_claim: crate::acp::shared_session::SharedInteractionClaim,
        task: tokio::task::JoinHandle<Result<(), AcpError>>,
    ) -> Result<(), AcpError> {
        match task.await {
            Ok(result) => result,
            Err(error) => {
                self.shared_session_broker
                    .complete_interaction(&recovery_claim)
                    .await?;
                Err(AcpError::protocol(error.to_string()))
            }
        }
    }

    async fn finish_shared_interaction_admission(
        &self,
        claim: &crate::acp::shared_session::SharedInteractionClaim,
        admission: Result<(), SharedControlAdmissionError>,
    ) -> Result<(), AcpError> {
        match admission {
            Ok(()) => {
                self.shared_session_broker
                    .complete_interaction(claim)
                    .await?;
                Ok(())
            }
            Err(SharedControlAdmissionError::DefinitelyNotAdmitted(error)) => {
                self.shared_session_broker
                    .release_interaction_claim(claim)
                    .await?;
                Err(error)
            }
            Err(SharedControlAdmissionError::MayHaveBeenAdmitted(error)) => {
                self.shared_session_broker
                    .complete_interaction(claim)
                    .await?;
                Err(error)
            }
            Err(SharedControlAdmissionError::InteractionAlreadyResolved { .. }) => {
                self.shared_session_broker
                    .complete_interaction_as_stale(claim)
                    .await?;
                Err(AcpError::Shared(
                    SharedSessionError::InteractionAlreadyResolved,
                ))
            }
        }
    }

    pub async fn enqueue_shared_prompt(
        &self,
        request: SharedPromptRequest,
    ) -> Result<PromptEnqueueResult, AcpError> {
        let connection_id = request.guard.connection_id.clone();
        let generation = request.guard.generation;
        let admission = self.shared_session_broker.enqueue_prompt(request).await?;
        let queue_item_id = admission.queue_item_id.clone();
        self.publish_shared_prompt_admission(&connection_id, generation, admission)
            .await?;
        #[cfg(test)]
        let finalize_hook = self
            .shared_enqueue_finalize_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        #[cfg(test)]
        if let Some(hook) = finalize_hook {
            hook.reached.wait().await;
            hook.resume.wait().await;
        }
        self.shared_session_broker
            .finalize_enqueue_response(&connection_id, generation, &queue_item_id)
            .await
            .map_err(Into::into)
    }

    async fn publish_shared_prompt_admission(
        &self,
        connection_id: &str,
        generation: u64,
        admission: SharedPromptAdmission,
    ) -> Result<(), AcpError> {
        let manager = self.clone_ref();
        let connection_id = connection_id.to_string();
        let queue_item_id = admission.queue_item_id;
        let events = admission.events;
        let publication = admission.publication;
        let publication_invalidated = admission.publication_invalidated;
        let notify = admission.notify;
        let publication_task = tokio::spawn(async move {
            publication
                .get_or_try_init(|| async move {
                    #[cfg(test)]
                    let publication_hook = manager
                        .shared_enqueue_publication_hook
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    #[cfg(test)]
                    if let Some(hook) = publication_hook {
                        hook.reached.wait().await;
                        hook.resume.wait().await;
                    }
                    if !manager
                        .publish_shared_prompt_admission_events(
                            &connection_id,
                            events,
                            publication_invalidated,
                        )
                        .await?
                    {
                        return Ok::<(), AcpError>(());
                    }
                    let published = manager
                        .shared_session_broker
                        .mark_prompt_admission_published(&connection_id, generation, &queue_item_id)
                        .await?;
                    if published {
                        notify.notify_one();
                    }
                    Ok::<(), AcpError>(())
                })
                .await
                .map(|_| ())
        });
        publication_task
            .await
            .map_err(|_| AcpError::from(SharedSessionError::SessionUnavailable))?
    }

    pub async fn cancel_shared_queued_prompt(
        &self,
        guard: SharedMutationGuard,
        queue_item_id: &str,
    ) -> Result<(), AcpError> {
        let connection_id = guard.connection_id.clone();
        let cancelled = self
            .shared_session_broker
            .cancel_queued_prompt(&guard, queue_item_id)
            .await?;
        self.publish_shared_events(&connection_id, cancelled.events)
            .await?;
        cancelled.notify.notify_one();
        Ok(())
    }

    async fn publish_shared_events(
        &self,
        connection_id: &str,
        events: Vec<AcpEvent>,
    ) -> Result<(), AcpError> {
        for event in events {
            self.publish_shared_event(connection_id, event).await?;
        }
        Ok(())
    }

    async fn publish_shared_prompt_admission_events(
        &self,
        connection_id: &str,
        events: Vec<AcpEvent>,
        publication_invalidated: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<bool, AcpError> {
        if publication_invalidated.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(false);
        }
        let handles = self
            .shared_session_broker()
            .public_state_and_emitter(connection_id)
            .await;
        let (state, emitter) = match handles {
            Some(handles) => handles,
            None => self
                .get_state_and_emitter(connection_id)
                .await
                .ok_or_else(|| AcpError::ConnectionNotFound(connection_id.into()))?,
        };
        for event in events {
            let publication_invalidated = publication_invalidated.clone();
            let applied = emit_with_state_gated(&state, &emitter, event, move |_| {
                !publication_invalidated.load(std::sync::atomic::Ordering::Acquire)
            })
            .await;
            if !applied {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn bind_shared_conversation_if_present(
        &self,
        connection_id: &str,
        conversation_id: i32,
    ) -> Result<(), AcpError> {
        if let Some(generation) = self
            .shared_session_broker
            .generation_for_connection(connection_id)
            .await
        {
            self.shared_session_broker
                .bind_conversation_key(connection_id, generation, conversation_id)
                .await?;
        }
        Ok(())
    }

    async fn shared_runtime_work_snapshot(
        &self,
        db: &AppDatabase,
        connection_id: &str,
    ) -> Option<SharedRuntimeWorkSnapshot> {
        let state = self.get_state(connection_id).await?;
        let (mut snapshot, conversation_id) = {
            let state = state.read().await;
            (
                state.shared_runtime_work_snapshot(None),
                state.conversation_id,
            )
        };
        if let Some(conversation_id) = conversation_id {
            snapshot.conversation_write_error =
                require_writable_conversation_workflow(&db.conn, conversation_id)
                    .await
                    .err()
                    .map(|error| stable_conversation_write_error(&error));
        }
        Some(snapshot)
    }

    fn spawn_shared_dispatcher(
        &self,
        connection_id: String,
        generation: u64,
        db: Arc<AppDatabase>,
    ) {
        let manager = self.clone_ref();
        tokio::spawn(async move {
            let Ok(mut subscription) = manager
                .shared_session_broker
                .runtime_subscription(&connection_id, generation)
                .await
            else {
                return;
            };

            loop {
                tokio::select! {
                    _ = subscription.notify.notified() => {}
                    changed = subscription.lifecycle.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    changed = subscription.registration.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
                if *subscription.lifecycle.borrow() != SharedLifecycleState::Active {
                    return;
                }

                let Some(snapshot) = manager
                    .shared_runtime_work_snapshot(&db, &connection_id)
                    .await
                else {
                    // A same-generation fallback temporarily removes the old
                    // driver from the manager map before publishing the new
                    // registration. Keep the creator-owned dispatcher alive;
                    // the registration watch wakes it after replacement.
                    continue;
                };
                let driver_incarnation = match manager
                    .shared_session_broker
                    .driver_incarnation_for_generation(&connection_id, generation)
                    .await
                {
                    Ok(Some(driver_incarnation)) => driver_incarnation,
                    _ => continue,
                };
                let reconcile_events = match manager
                    .shared_session_broker
                    .reconcile_runtime_snapshot(
                        &connection_id,
                        generation,
                        &driver_incarnation,
                        &snapshot,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(_) => continue,
                };
                if manager
                    .publish_shared_events(&connection_id, reconcile_events)
                    .await
                    .is_err()
                {
                    return;
                }
                if *subscription.lifecycle.borrow() != SharedLifecycleState::Active {
                    return;
                }

                // Re-sample immediately before claim. The snapshot used for
                // reconcile can go stale while that await runs; a queued
                // prompt plus an old turn_in_flight=false would dispatch
                // over an in-flight turn.
                let Some(snapshot) = manager
                    .shared_runtime_work_snapshot(&db, &connection_id)
                    .await
                else {
                    continue;
                };
                let turn_id = uuid::Uuid::new_v4().to_string();
                let decision = match manager
                    .shared_session_broker
                    .claim_dispatchable_head(&connection_id, generation, &turn_id, &snapshot)
                    .await
                {
                    Ok(decision) => decision,
                    Err(_) => return,
                };
                match decision {
                    DispatchHeadDecision::Blocked => {}
                    DispatchHeadDecision::Failed(failed) => {
                        if manager
                            .publish_shared_events(&connection_id, failed.events)
                            .await
                            .is_err()
                        {
                            return;
                        }
                        failed.notify.notify_one();
                    }
                    DispatchHeadDecision::Claimed(claimed) => {
                        if manager
                            .publish_shared_events(&connection_id, claimed.events)
                            .await
                            .is_err()
                        {
                            return;
                        }
                        let result = manager
                            .send_prompt_linked_with_message_id(
                                &db,
                                &connection_id,
                                claimed.blocks,
                                claimed.folder_id,
                                claimed.conversation_id,
                                None,
                                Some(claimed.client_message_id),
                                claimed.capture,
                            )
                            .await;
                        if let Err(error) = result {
                            let failed = match manager
                                .shared_session_broker
                                .fail_claimed_item(
                                    &connection_id,
                                    generation,
                                    &turn_id,
                                    stable_dispatch_error(&error),
                                )
                                .await
                            {
                                Ok(failed) => failed,
                                Err(_) => return,
                            };
                            if manager
                                .publish_shared_events(&connection_id, failed.events)
                                .await
                                .is_err()
                            {
                                return;
                            }
                            let connected = manager
                                .shared_runtime_work_snapshot(&db, &connection_id)
                                .await
                                .is_some_and(|snapshot| {
                                    snapshot.status == ConnectionStatus::Connected
                                });
                            if connected {
                                failed.notify.notify_one();
                            } else if let Ok(events) = manager
                                .shared_session_broker
                                .fail_live_session(
                                    &connection_id,
                                    generation,
                                    &driver_incarnation,
                                    "session_unavailable",
                                )
                                .await
                            {
                                let _ = manager.publish_shared_events(&connection_id, events).await;
                            }
                        }
                    }
                }
            }
        });
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn shared_spawn_count_for_test(&self) -> usize {
        self.shared_spawn_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn shared_registered_root_count_for_test(&self) -> usize {
        self.shared_registered_root_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn shared_teardown_count_for_test(&self) -> usize {
        self.shared_teardown_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn has_connection_map_entry_for_test(&self, connection_id: &str) -> bool {
        self.connections.lock().await.contains_key(connection_id)
    }

    pub async fn connect_or_attach_shared(
        &self,
        launch: SharedConnectLaunch,
    ) -> Result<SharedSessionAttachment, AcpError> {
        let _permit = self.admission.admit()?;
        #[cfg(test)]
        self.hold_after_admission_for_test().await;
        self.admission.ensure_accepting()?;
        assert_eq!(
            launch.launch_context.purpose, launch.launch_identity.purpose,
            "shared launch purpose must match its reserved identity"
        );
        assert_eq!(
            launch.session_attach_mode, launch.launch_identity.attach_mode,
            "shared attach mode must match its reserved identity"
        );

        let connection_id = uuid::Uuid::new_v4().to_string();
        let reserve = self
            .shared_session_broker
            .reserve_or_attach(SharedReserveRequest {
                key: launch.key.clone(),
                connection_id,
                launch_identity: launch.launch_identity.clone(),
                client_instance_id: launch.client_instance_id.clone(),
                device_id: launch.device_id.clone(),
                request_id: launch.request_id.clone(),
                retry_failed_generation: launch.retry_failed_generation,
                now: tokio::time::Instant::now(),
                now_utc: chrono::Utc::now(),
            })
            .await?;

        if reserve.created {
            let database = launch.database.clone();
            let dispatcher_database = Arc::new(crate::db::AppDatabase { conn: database });
            self.spawn_shared_registration(reserve.attachment.clone(), launch);
            self.spawn_shared_dispatcher(
                reserve.attachment.connection_id.clone(),
                reserve.attachment.generation,
                dispatcher_database,
            );
        }

        let registration = self
            .shared_session_broker
            .wait_until_registered(
                &reserve.attachment.connection_id,
                reserve.attachment.generation,
            )
            .await?;
        let mut attachment = reserve.attachment;
        attachment.phase = registration.phase;
        Ok(attachment)
    }

    fn spawn_shared_registration(
        &self,
        attachment: SharedSessionAttachment,
        launch: SharedConnectLaunch,
    ) {
        let manager = self.clone_ref();
        let connection_id = attachment.connection_id.clone();
        let generation = attachment.generation;
        let supervisor_launch = launch.clone();
        let task = tokio::spawn(async move {
            manager.run_shared_registration(attachment, launch).await;
        });

        let supervisor = self.clone_ref();
        tokio::spawn(async move {
            if task.await.is_err() {
                let expected_driver_incarnation = supervisor
                    .shared_session_broker
                    .driver_incarnation_for_generation(&connection_id, generation)
                    .await
                    .ok()
                    .flatten();
                supervisor
                    .fail_shared_generation(
                        connection_id,
                        generation,
                        expected_driver_incarnation,
                        SharedSessionError::SessionUnavailable,
                        supervisor_launch,
                    )
                    .await;
            }
        });
    }

    async fn run_shared_registration(
        &self,
        attachment: SharedSessionAttachment,
        launch: SharedConnectLaunch,
    ) {
        let connection_id = attachment.connection_id.clone();
        let generation = attachment.generation;
        self.shared_launches
            .lock()
            .await
            .insert((connection_id.clone(), generation), launch.clone());

        match self
            .start_shared_attempt(connection_id.clone(), launch.clone(), None)
            .await
        {
            Ok(registered) => {
                {
                    let mut state = registered.state.write().await;
                    state.conversation_id = launch.conversation_id;
                    state.folder_id = launch.folder_id;
                    state.status = ConnectionStatus::Connecting;
                    state.shared_session = Some(shared_projection(
                        generation,
                        SharedSessionPhase::Bootstrapping,
                        None,
                    ));
                }
                let driver_incarnation = registered.connection_incarnation.clone();
                let events = match self
                    .shared_session_broker
                    .install_registered(
                        &connection_id,
                        generation,
                        driver_incarnation.clone(),
                        registered.state.clone(),
                        registered.emitter.clone(),
                        registered.child_pid.clone(),
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(_) => {
                        let _ = self.teardown_unexposed_attempt(&connection_id).await;
                        return;
                    }
                };
                for event in events {
                    if self
                        .publish_shared_event(&connection_id, event)
                        .await
                        .is_err()
                    {
                        let _ = self.teardown_unexposed_attempt(&connection_id).await;
                        return;
                    }
                }
                let event_rx = {
                    let state = registered.state.read().await;
                    state.event_stream().subscribe()
                };
                self.spawn_shared_runtime_monitor(
                    connection_id.clone(),
                    generation,
                    driver_incarnation.clone(),
                    registered.state.clone(),
                    event_rx,
                )
                .await;
                self.spawn_shared_bootstrap_settler(
                    connection_id,
                    generation,
                    driver_incarnation,
                    registered,
                );
            }
            Err(error) => {
                self.fail_shared_generation(
                    connection_id,
                    generation,
                    None,
                    shared_registration_error(error),
                    launch,
                )
                .await;
            }
        }
    }

    fn spawn_shared_bootstrap_settler(
        &self,
        connection_id: String,
        generation: u64,
        driver_incarnation: String,
        mut registered: RegisteredSpawnAttempt,
    ) {
        let driver_start_tx = registered.driver_start_tx.take();
        let manager = self.clone_ref();
        let task_connection_id = connection_id.clone();
        let task_driver_incarnation = driver_incarnation.clone();
        let task = tokio::spawn(async move {
            let outcome = registered
                .handshake
                .route_bootstrap_rx
                .await
                .unwrap_or(RouteBootstrapOutcome::Fatal(AcpError::ProcessExited));
            manager
                .settle_shared_bootstrap(
                    task_connection_id,
                    generation,
                    task_driver_incarnation,
                    registered.route_plan,
                    outcome,
                )
                .await;
        });

        let supervisor = self.clone_ref();
        tokio::spawn(async move {
            if task.await.is_err() {
                let launch = supervisor
                    .shared_launches
                    .lock()
                    .await
                    .get(&(connection_id.clone(), generation))
                    .cloned();
                if let Some(launch) = launch {
                    supervisor
                        .fail_shared_generation(
                            connection_id,
                            generation,
                            Some(driver_incarnation),
                            SharedSessionError::SessionUnavailable,
                            launch,
                        )
                        .await;
                }
                #[cfg(test)]
                supervisor.shared_settler_supervisor_completed.notify_one();
            }
        });
        if let Some(driver_start_tx) = driver_start_tx {
            let _ = driver_start_tx.send(());
        }
    }

    async fn spawn_shared_runtime_monitor(
        &self,
        connection_id: String,
        generation: u64,
        driver_incarnation: String,
        state: Arc<RwLock<SessionState>>,
        mut event_rx: tokio::sync::broadcast::Receiver<Arc<crate::acp::types::EventEnvelope>>,
    ) {
        let manager = self.clone_ref();
        let Ok(mut subscription) = manager
            .shared_session_broker
            .runtime_subscription(&connection_id, generation)
            .await
        else {
            return;
        };

        let initial = state.read().await.shared_runtime_work_snapshot(None);
        let initial_events = match manager
            .shared_session_broker
            .reconcile_runtime_snapshot(&connection_id, generation, &driver_incarnation, &initial)
            .await
        {
            Ok(events) => events,
            Err(_) => return,
        };
        if manager
            .publish_shared_events(&connection_id, initial_events)
            .await
            .is_err()
        {
            return;
        }
        subscription.notify.notify_one();

        tokio::spawn(async move {
            loop {
                let envelope = tokio::select! {
                    changed = subscription.lifecycle.changed() => {
                        if changed.is_err()
                            || *subscription.lifecycle.borrow() != SharedLifecycleState::Active
                        {
                            return;
                        }
                        continue;
                    }
                    changed = subscription.registration.changed() => {
                        if changed.is_err()
                            || subscription.registration.borrow().driver_incarnation.as_deref()
                                != Some(driver_incarnation.as_str())
                        {
                            return;
                        }
                        continue;
                    }
                    received = event_rx.recv() => {
                        match received {
                            Ok(envelope) => envelope,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                manager
                                    .handle_unexpected_shared_driver_exit(
                                        &connection_id,
                                        generation,
                                        &driver_incarnation,
                                    )
                                    .await;
                                return;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                let snapshot = state
                                    .read()
                                    .await
                                    .shared_runtime_work_snapshot(None);
                                let reconcile_events = match manager
                                    .shared_session_broker
                                    .reconcile_runtime_snapshot(
                                        &connection_id,
                                        generation,
                                        &driver_incarnation,
                                        &snapshot,
                                    )
                                    .await
                                {
                                    Ok(events) => events,
                                    Err(_) => return,
                                };
                                if manager
                                    .publish_shared_events(&connection_id, reconcile_events)
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                subscription.notify.notify_one();
                                continue;
                            }
                        }
                    }
                };

                if matches!(
                    envelope.payload,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Disconnected | ConnectionStatus::Error,
                    }
                ) {
                    manager
                        .handle_unexpected_shared_driver_exit(
                            &connection_id,
                            generation,
                            &driver_incarnation,
                        )
                        .await;
                    return;
                }

                let broker_events = match &envelope.payload {
                    AcpEvent::TurnComplete { stop_reason, .. } => {
                        manager
                            .shared_session_broker
                            .settle_active_turn_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                stop_reason,
                                envelope.seq,
                            )
                            .await
                    }
                    AcpEvent::PermissionRequest { request_id, .. } => {
                        manager
                            .shared_session_broker
                            .observe_interaction_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                SharedInteractionKind::Permission,
                                request_id,
                                envelope.seq,
                            )
                            .await
                    }
                    AcpEvent::PermissionResolved { request_id } => {
                        manager
                            .shared_session_broker
                            .observe_interaction_resolved_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                SharedInteractionKind::Permission,
                                request_id,
                                envelope.seq,
                            )
                            .await
                    }
                    AcpEvent::QuestionRequest { question_id, .. } => {
                        manager
                            .shared_session_broker
                            .observe_interaction_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                SharedInteractionKind::Question,
                                question_id,
                                envelope.seq,
                            )
                            .await
                    }
                    AcpEvent::QuestionResolved { question_id } => {
                        manager
                            .shared_session_broker
                            .observe_interaction_resolved_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                SharedInteractionKind::Question,
                                question_id,
                                envelope.seq,
                            )
                            .await
                    }
                    AcpEvent::PlanApprovalRequest { approval_id, .. } => {
                        manager
                            .shared_session_broker
                            .observe_interaction_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                SharedInteractionKind::PlanApproval,
                                approval_id,
                                envelope.seq,
                            )
                            .await
                    }
                    AcpEvent::PlanApprovalResolved { approval_id } => {
                        manager
                            .shared_session_broker
                            .observe_interaction_resolved_at_seq(
                                &connection_id,
                                generation,
                                &driver_incarnation,
                                SharedInteractionKind::PlanApproval,
                                approval_id,
                                envelope.seq,
                            )
                            .await
                    }
                    _ => Ok(Vec::new()),
                };
                let broker_events = match broker_events {
                    Ok(events) => events,
                    Err(SharedSessionError::GenerationStale) => return,
                    Err(_) => continue,
                };
                if manager
                    .publish_shared_events(&connection_id, broker_events)
                    .await
                    .is_err()
                {
                    return;
                }

                if matches!(
                    &envelope.payload,
                    AcpEvent::ContinuationWaitingChanged { .. }
                        | AcpEvent::DelegationStarted { .. }
                        | AcpEvent::DelegationCompleted { .. }
                        | AcpEvent::BackgroundActivity { .. }
                        | AcpEvent::PermissionResolved { .. }
                        | AcpEvent::QuestionResolved { .. }
                        | AcpEvent::PlanApprovalResolved { .. }
                        | AcpEvent::StatusChanged { .. }
                        | AcpEvent::TurnComplete { .. }
                ) {
                    let snapshot = state.read().await.shared_runtime_work_snapshot(None);
                    let reconcile_events = match manager
                        .shared_session_broker
                        .reconcile_runtime_snapshot(
                            &connection_id,
                            generation,
                            &driver_incarnation,
                            &snapshot,
                        )
                        .await
                    {
                        Ok(events) => events,
                        Err(_) => return,
                    };
                    if manager
                        .publish_shared_events(&connection_id, reconcile_events)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    subscription.notify.notify_one();
                }
            }
        });
    }

    async fn handle_unexpected_shared_driver_exit(
        &self,
        connection_id: &str,
        generation: u64,
        driver_incarnation: &str,
    ) {
        let events = match self
            .shared_session_broker
            .fail_live_session(
                connection_id,
                generation,
                driver_incarnation,
                "session_unavailable",
            )
            .await
        {
            Ok(events) if !events.is_empty() => events,
            Ok(_) | Err(SharedSessionError::GenerationStale) => return,
            Err(error) => {
                tracing::warn!(
                    connection_id,
                    generation,
                    error_code = error.code(),
                    "[ACP] shared driver exit settlement failed"
                );
                return;
            }
        };
        let _ = self.publish_shared_events(connection_id, events).await;

        let started = tokio::time::Instant::now();
        let cleanup_complete = self
            .teardown_shared_driver(connection_id, true, AcpDisconnectOrigin::AbandonedConnect)
            .await;
        self.shared_session_broker
            .record_cleanup_duration(started.elapsed());
        if cleanup_complete {
            if let Ok((Some((state, emitter)), events)) = self
                .shared_session_broker
                .mark_cleanup_complete(connection_id, generation)
                .await
            {
                for event in events {
                    emit_with_state(&state, &emitter, event).await;
                }
            }
        } else {
            self.shared_session_broker.record_cleanup_incomplete();
        }
        self.shared_launches
            .lock()
            .await
            .remove(&(connection_id.to_string(), generation));
    }

    async fn start_shared_attempt(
        &self,
        connection_id: String,
        launch: SharedConnectLaunch,
        existing_public_state: Option<Arc<RwLock<SessionState>>>,
    ) -> Result<RegisteredSpawnAttempt, AcpError> {
        let _permit = self.admission.admit()?;
        #[cfg(any(test, feature = "test-utils"))]
        self.shared_spawn_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        self.start_registered_shared_root(
            connection_id,
            launch.agent_type,
            launch.working_dir,
            launch.external_session_id,
            launch.launch_inputs,
            launch.emitter,
            launch.preferred_mode_id,
            launch.preferred_config_values,
            launch.launch_context,
            launch.session_attach_mode,
            existing_public_state,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_registered_shared_root(
        &self,
        connection_id: String,
        agent_type: AgentType,
        working_dir: Option<String>,
        session_id: Option<String>,
        launch_inputs: AcpLaunchInputs,
        emitter: EventEmitter,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        launch_context: ConnectionLaunchContext,
        session_attach_mode: crate::acp::session_attach::SessionAttachMode,
        existing_public_state: Option<Arc<RwLock<SessionState>>>,
    ) -> Result<RegisteredSpawnAttempt, AcpError> {
        #[cfg(any(test, feature = "test-utils"))]
        self.shared_registered_root_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let is_registered_replacement = existing_public_state.is_some();
        let expected = self
            .shared_session_broker
            .launch_identity_for_connection(&connection_id)
            .await?;
        let AcpLaunchConfig {
            runtime_env,
            terminal_shell,
            route_plan,
            origin,
            route_preference,
            route_capability,
        } = finalize_acp_launch_config(launch_inputs, agent_type)?;

        let conflict = if agent_type != expected.agent_type {
            Some(SharedConfigConflictKind::AgentType)
        } else if crate::parsers::normalize_path_for_matching(
            working_dir.as_deref().unwrap_or_default(),
        ) != expected.working_dir_fingerprint
        {
            Some(SharedConfigConflictKind::WorkingDirectory)
        } else if session_id != expected.external_session_id {
            Some(SharedConfigConflictKind::ExternalSession)
        } else if session_attach_mode != expected.attach_mode {
            Some(SharedConfigConflictKind::AttachMode)
        } else if route_plan.fingerprint != expected.route_fingerprint
            && !(is_registered_replacement
                && route_plan.source == DelegationRouteSource::SafeFallback)
        {
            Some(SharedConfigConflictKind::DelegationRoute)
        } else if terminal_shell.selection_key != expected.terminal_shell_fingerprint {
            Some(SharedConfigConflictKind::TerminalShell)
        } else if launch_context.purpose != expected.purpose {
            Some(SharedConfigConflictKind::Purpose)
        } else {
            None
        };
        if let Some(conflict_kind) = conflict {
            return Err(SharedSessionError::ConfigConflict {
                connection_id,
                conflict_kind,
            }
            .into());
        }
        route_plan
            .assert_exclusive()
            .map_err(|error| AcpError::protocol(error.to_string()))?;

        #[cfg(any(test, feature = "test-utils"))]
        if let Some(driver) = self.shared_spawn_override.as_ref() {
            let launch = {
                let launches = self.shared_launches.lock().await;
                launches
                    .iter()
                    .find_map(|((registered_connection_id, _), launch)| {
                        (registered_connection_id == &connection_id).then(|| launch.clone())
                    })
                    .ok_or(SharedSessionError::SessionUnavailable)?
            };
            return driver
                .start(connection_id, launch, existing_public_state)
                .await;
        }

        let skip_delegation_injection = launch_context.purpose.is_hidden_generation();
        let injection = if skip_delegation_injection {
            None
        } else {
            self.delegation_snapshot()
        };
        spawn_agent_connection(
            connection_id,
            agent_type,
            working_dir,
            session_id,
            runtime_env,
            terminal_shell,
            route_plan,
            origin,
            route_preference,
            route_capability,
            "shared-server".into(),
            None,
            None,
            emitter,
            self.connections.clone(),
            preferred_mode_id,
            preferred_config_values,
            injection,
            None,
            launch_context,
            session_attach_mode,
            self.tool_lease_registry.clone(),
            self.mcp_cancel_registry.clone(),
            existing_public_state,
        )
        .await
    }

    async fn settle_shared_bootstrap(
        &self,
        connection_id: String,
        generation: u64,
        driver_incarnation: String,
        route_plan: DelegationRoutePlan,
        outcome: RouteBootstrapOutcome,
    ) {
        if !self
            .shared_session_broker
            .is_current_bootstrapping_driver(&connection_id, generation, &driver_incarnation)
            .await
        {
            return;
        }
        let route_specific_reason = match &outcome {
            RouteBootstrapOutcome::RouteSpecific(reason) => Some(*reason),
            RouteBootstrapOutcome::Ready | RouteBootstrapOutcome::Fatal(_) => None,
        };
        let action = shared_bootstrap_action(&route_plan, outcome);
        let shared_agent_type = self
            .shared_launches
            .lock()
            .await
            .get(&(connection_id.clone(), generation))
            .map(|launch| launch.agent_type);
        if let (Some(reason), Some(injection), Some(agent_type)) = (
            route_specific_reason,
            self.delegation_snapshot(),
            shared_agent_type,
        ) {
            match &action {
                SharedBootstrapAction::AllowedFallback(_) => {
                    injection.metrics.record_safe_fallback(agent_type, reason)
                }
                SharedBootstrapAction::Fail(_) => injection
                    .metrics
                    .record_suppression_failure(agent_type, reason),
                SharedBootstrapAction::Ready => {}
            }
        }

        match action {
            SharedBootstrapAction::Ready => {
                match self
                    .shared_session_broker
                    .mark_ready(&connection_id, generation, &driver_incarnation)
                    .await
                {
                    Ok(events) => {
                        // Phase waiters consume the authoritative broker watch;
                        // event publication follows as a separate unlocked step.
                        tokio::task::yield_now().await;
                        for event in events {
                            if self
                                .publish_shared_event(&connection_id, event)
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        let _ = self
                            .shared_session_broker
                            .notify_dispatcher(&connection_id, generation)
                            .await;
                        self.shared_launches
                            .lock()
                            .await
                            .remove(&(connection_id, generation));
                    }
                    Err(SharedSessionError::SessionUnavailable) => {
                        let launch = self
                            .shared_launches
                            .lock()
                            .await
                            .get(&(connection_id.clone(), generation))
                            .cloned();
                        if let Some(launch) = launch {
                            self.fail_shared_generation(
                                connection_id,
                                generation,
                                Some(driver_incarnation),
                                SharedSessionError::SessionUnavailable,
                                launch,
                            )
                            .await;
                        }
                    }
                    Err(_) => {}
                }
            }
            SharedBootstrapAction::Fail(error) => {
                let launch = self
                    .shared_launches
                    .lock()
                    .await
                    .get(&(connection_id.clone(), generation))
                    .cloned();
                if let Some(launch) = launch {
                    self.fail_shared_generation(
                        connection_id,
                        generation,
                        Some(driver_incarnation),
                        error,
                        launch,
                    )
                    .await;
                }
            }
            SharedBootstrapAction::AllowedFallback(reason) => {
                if self
                    .teardown_unexposed_attempt(&connection_id)
                    .await
                    .is_err()
                {
                    let launch = self
                        .shared_launches
                        .lock()
                        .await
                        .get(&(connection_id.clone(), generation))
                        .cloned();
                    if let Some(launch) = launch {
                        self.fail_shared_generation(
                            connection_id,
                            generation,
                            Some(driver_incarnation),
                            SharedSessionError::SessionUnavailable,
                            launch,
                        )
                        .await;
                    }
                    return;
                }
                debug_assert!(!self.connections.lock().await.contains_key(&connection_id));
                #[cfg(any(test, feature = "test-utils"))]
                self.shared_fallback_trace
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push("old_driver_absent");

                let launch = self
                    .shared_launches
                    .lock()
                    .await
                    .get(&(connection_id.clone(), generation))
                    .cloned();
                let Some(mut launch) = launch else {
                    return;
                };
                launch.launch_inputs.route_plan = safe_native_fallback(&route_plan, reason);
                let Some((public_state, _)) = self
                    .shared_session_broker
                    .public_state_and_emitter(&connection_id)
                    .await
                else {
                    return;
                };
                let permit = match self
                    .shared_session_broker
                    .begin_registered_replacement(&connection_id, generation, &driver_incarnation)
                    .await
                {
                    Ok(permit) => permit,
                    Err(_) => return,
                };
                self.shared_launches
                    .lock()
                    .await
                    .insert((connection_id.clone(), generation), launch.clone());
                #[cfg(any(test, feature = "test-utils"))]
                self.shared_fallback_trace
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push("replacement_start");
                match self
                    .start_shared_attempt(
                        connection_id.clone(),
                        launch.clone(),
                        Some(public_state.clone()),
                    )
                    .await
                {
                    Ok(replacement) => {
                        debug_assert!(Arc::ptr_eq(&public_state, &replacement.state));
                        let next_incarnation = replacement.connection_incarnation.clone();
                        if self
                            .shared_session_broker
                            .commit_registered_replacement(
                                &permit,
                                next_incarnation.clone(),
                                replacement.state.clone(),
                                replacement.emitter.clone(),
                                replacement.child_pid.clone(),
                            )
                            .await
                            .is_ok()
                        {
                            let event_rx = {
                                let state = replacement.state.read().await;
                                state.event_stream().subscribe()
                            };
                            self.spawn_shared_runtime_monitor(
                                connection_id.clone(),
                                generation,
                                next_incarnation.clone(),
                                replacement.state.clone(),
                                event_rx,
                            )
                            .await;
                            self.shared_launches
                                .lock()
                                .await
                                .insert((connection_id.clone(), generation), launch);
                            self.spawn_shared_bootstrap_settler(
                                connection_id,
                                generation,
                                next_incarnation,
                                replacement,
                            );
                            #[cfg(test)]
                            if self
                                .shared_settler_panic_after_replacement
                                .swap(false, std::sync::atomic::Ordering::SeqCst)
                            {
                                panic!("intentional old settler panic after replacement");
                            }
                        } else {
                            self.fail_shared_replacement(
                                connection_id,
                                generation,
                                permit,
                                SharedSessionError::SessionUnavailable,
                                launch,
                            )
                            .await;
                        }
                    }
                    Err(_) => {
                        self.fail_shared_replacement(
                            connection_id,
                            generation,
                            permit,
                            SharedSessionError::SessionUnavailable,
                            launch,
                        )
                        .await;
                    }
                }
            }
        }
    }

    async fn fail_shared_replacement(
        &self,
        connection_id: String,
        generation: u64,
        permit: RegisteredReplacementPermit,
        error: SharedSessionError,
        launch: SharedConnectLaunch,
    ) {
        let (state, emitter) = match self
            .shared_session_broker
            .public_state_and_emitter(&connection_id)
            .await
        {
            Some(public) => public,
            None => (
                Arc::new(RwLock::new(self.minimal_failed_shared_state(
                    &connection_id,
                    generation,
                    &launch,
                    &error,
                    false,
                    None,
                ))),
                launch.emitter.clone(),
            ),
        };
        let events = match self
            .shared_session_broker
            .fail_registered_replacement(&permit, error, false, state, emitter)
            .await
        {
            Ok((_, _, events)) => events,
            Err(_) => return,
        };
        for event in events {
            if self
                .publish_shared_event(&connection_id, event)
                .await
                .is_err()
            {
                return;
            }
        }

        let cleanup_complete = self
            .teardown_unexposed_attempt(&connection_id)
            .await
            .is_ok()
            && !self.connections.lock().await.contains_key(&connection_id);
        if cleanup_complete {
            if let Ok((Some((state, emitter)), events)) = self
                .shared_session_broker
                .mark_cleanup_complete(&connection_id, generation)
                .await
            {
                for event in events {
                    emit_with_state(&state, &emitter, event).await;
                }
            }
        }
        self.shared_launches
            .lock()
            .await
            .remove(&(connection_id, generation));
    }

    async fn fail_shared_generation(
        &self,
        connection_id: String,
        generation: u64,
        expected_driver_incarnation: Option<String>,
        error: SharedSessionError,
        launch: SharedConnectLaunch,
    ) {
        let (state, emitter) = match self
            .shared_session_broker
            .public_state_and_emitter(&connection_id)
            .await
        {
            Some(public) => public,
            None => (
                Arc::new(RwLock::new(self.minimal_failed_shared_state(
                    &connection_id,
                    generation,
                    &launch,
                    &error,
                    false,
                    expected_driver_incarnation.as_deref(),
                ))),
                launch.emitter.clone(),
            ),
        };
        let events = match self
            .shared_session_broker
            .fail_registered(
                &connection_id,
                generation,
                expected_driver_incarnation.as_deref(),
                error,
                false,
                state.clone(),
                emitter,
            )
            .await
        {
            Ok((_, _, events)) => events,
            Err(_) => return,
        };
        for event in events {
            if self
                .publish_shared_event(&connection_id, event)
                .await
                .is_err()
            {
                return;
            }
        }

        let cleanup_complete = self
            .teardown_unexposed_attempt(&connection_id)
            .await
            .is_ok()
            && !self.connections.lock().await.contains_key(&connection_id);
        if cleanup_complete {
            if let Ok((Some((state, emitter)), events)) = self
                .shared_session_broker
                .mark_cleanup_complete(&connection_id, generation)
                .await
            {
                for event in events {
                    emit_with_state(&state, &emitter, event).await;
                }
            }
        }
        self.shared_launches
            .lock()
            .await
            .remove(&(connection_id, generation));
    }

    fn minimal_failed_shared_state(
        &self,
        connection_id: &str,
        generation: u64,
        launch: &SharedConnectLaunch,
        error: &SharedSessionError,
        cleanup_complete: bool,
        driver_incarnation: Option<&str>,
    ) -> SessionState {
        let mut state = SessionState::new(
            connection_id.into(),
            launch.agent_type,
            launch.working_dir.clone().map(PathBuf::from),
            "shared-server".into(),
            launch.folder_id,
        );
        state.connection_incarnation = driver_incarnation
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        state.tool_lease_registry = self.tool_lease_registry.clone();
        state.mcp_cancel_registry = self.mcp_cancel_registry.clone();
        state.conversation_id = launch.conversation_id;
        state.external_id = launch.external_session_id.clone();
        state.purpose = launch.launch_context.purpose;
        state.effective_locale = launch
            .launch_context
            .inherited_locale
            .unwrap_or(AppLocale::En);
        state.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
        state.status = ConnectionStatus::Error;
        state.shared_session = Some(shared_projection(
            generation,
            SharedSessionPhase::Failed {
                error_code: error.code().into(),
                cleanup_complete,
            },
            None,
        ));
        state
    }

    pub async fn wait_for_shared_phase(
        &self,
        connection_id: &str,
        generation: u64,
        phase: SharedSessionPhase,
    ) -> Result<(), AcpError> {
        self.shared_session_broker
            .wait_for_phase(connection_id, generation, phase)
            .await
            .map_err(Into::into)
    }

    /// Shared terminal-shell setting consumed by ACP terminal runtimes.
    pub fn terminal_shell_config(
        &self,
    ) -> crate::acp::terminal_runtime::TerminalShellRuntimeConfig {
        self.terminal_shell_config.clone()
    }

    /// Process-scoped tool-execution lease registry.
    pub fn tool_lease_registry(
        &self,
    ) -> Arc<crate::acp::tool_watchdog::ToolExecutionLeaseRegistry> {
        self.tool_lease_registry.clone()
    }

    /// Secret-safe tool-watchdog counters.
    pub fn tool_watchdog_metrics(&self) -> Arc<crate::acp::tool_watchdog::ToolWatchdogMetrics> {
        self.tool_watchdog_metrics.clone()
    }

    /// Wake the production supervisor scan (coalescing).
    pub fn wake_tool_watchdog(&self) {
        self.tool_watchdog_wake.notify_one();
    }

    /// Mutex that serializes settings persist + live apply.
    pub fn tool_watchdog_settings_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.tool_watchdog_settings_gate.clone()
    }

    /// Attribution facade over the shared registry.
    pub fn lease_attribution(&self) -> crate::acp::tool_watchdog::LeaseAttribution {
        crate::acp::tool_watchdog::LeaseAttribution::new(self.tool_lease_registry.clone())
    }

    /// Set the delegation injection context exactly once during bootstrap.
    /// Calling twice is a no-op — protects against accidental re-init in
    /// the unlikely event a second `build_delegation_stack` runs.
    pub fn install_delegation(&self, injection: crate::acp::connection::DelegationInjection) {
        let _ = self.delegation_injection.set(injection);
    }

    pub fn install_recovery_authorization_service(
        &self,
        service: Arc<crate::acp::recovery_authorization::RecoveryAuthorizationService>,
    ) {
        let _ = self.recovery_authorization_service.set(service);
    }

    pub fn recovery_authorization_service(
        &self,
    ) -> Option<Arc<crate::acp::recovery_authorization::RecoveryAuthorizationService>> {
        self.recovery_authorization_service.get().cloned()
    }

    fn delegation_snapshot(&self) -> Option<crate::acp::connection::DelegationInjection> {
        self.delegation_injection.get().cloned()
    }

    pub(crate) fn install_continuation_store(&self, store: Arc<dyn ContinuationStore>) {
        let _ = self.continuation_store.set(store);
    }

    #[allow(dead_code)]
    fn continuation_store(&self) -> Option<Arc<dyn ContinuationStore>> {
        self.continuation_store.get().cloned()
    }

    /// Insert a synthetic `AgentConnection` for tests that need to exercise
    /// downstream code (attach, event broadcast, conversation linking)
    /// without spawning a real agent process. The returned connection is
    /// marked `Connected` and has a dropped `cmd_tx` receiver, so any
    /// attempt to send a prompt resolves to `ProcessExited` — fine for
    /// tests asserting on event-bus or session-state behavior.
    ///
    /// Gated behind `cfg(test)` (in-crate unit tests) and the `test-utils`
    /// feature (integration tests in `tests/*.rs`); the item is physically
    /// uncompiled in release builds so no production caller can reach it.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn insert_test_connection(
        &self,
        id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) {
        use crate::acp::session_state::SessionState;
        let (tx, _rx, _liveness_rx) = connection_channel(1);
        let mut state = SessionState::new(
            id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        state.tool_lease_registry = self.tool_lease_registry.clone();
        state.mcp_cancel_registry = self.mcp_cancel_registry.clone();
        let connection_incarnation = state.connection_incarnation.clone();
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = crate::acp::delegation::route::test_empty_route_plan();
        let (spawn_config, observed_config) = matching_config_pair(
            String::new(),
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );
        let conn = AgentConnection {
            id: id.to_string(),
            agent_type,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation,
            tool_lease_registry: self.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(tokio::sync::RwLock::new(state)),
            emitter,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config,
            observed_config,
            terminal_shell,
            route_plan,
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mut map = self.connections.lock().await;
        map.insert(id.to_string(), conn);
    }

    /// Register a broker-owned public state without adding a manager-map
    /// `AgentConnection`. Integration tests use this to exercise the same
    /// retained-state path that failed shared generations use after cleanup.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn install_test_shared_connection(
        &self,
        attachment: &SharedSessionAttachment,
        conversation_id: Option<i32>,
    ) -> Result<Arc<RwLock<SessionState>>, SharedSessionError> {
        let mut state = SessionState::new(
            attachment.connection_id.clone(),
            AgentType::Codex,
            None,
            "shared-server".into(),
            conversation_id,
        );
        state.status = ConnectionStatus::Connecting;
        state.tool_lease_registry = self.tool_lease_registry.clone();
        state.mcp_cancel_registry = self.mcp_cancel_registry.clone();
        state.shared_session = Some(shared_projection(
            attachment.generation,
            SharedSessionPhase::Bootstrapping,
            None,
        ));
        let state = Arc::new(RwLock::new(state));
        self.shared_session_broker
            .install_registered(
                &attachment.connection_id,
                attachment.generation,
                format!("test-driver-{}", attachment.generation),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await?;
        Ok(state)
    }

    /// Insert a synthetic delegated child that adopts the parent's live
    /// ownership under the connections lock (same fence as production
    /// `spawn_agent_connection` registration). Used by concurrent rebind tests.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn insert_test_child_adopting_parent_ownership(
        &self,
        child_id: &str,
        parent_id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) {
        use crate::acp::connection::resolve_spawn_ownership_under_lock;
        use crate::acp::session_state::SessionState;
        let (tx, _rx, _liveness_rx) = connection_channel(1);
        let mut state = SessionState::new(
            child_id.to_string(),
            agent_type,
            working_dir,
            "pending".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        state.tool_lease_registry = self.tool_lease_registry.clone();
        state.mcp_cancel_registry = self.mcp_cancel_registry.clone();
        let connection_incarnation = state.connection_incarnation.clone();
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = crate::acp::delegation::route::test_empty_route_plan();
        let (spawn_config, observed_config) = matching_config_pair(
            String::new(),
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );
        let session_state = Arc::new(tokio::sync::RwLock::new(state));
        let mut map = self.connections.lock().await;
        let (label, op, gen) =
            resolve_spawn_ownership_under_lock(&map, Some(parent_id), "pending".to_string(), None);
        {
            let mut st = session_state.write().await;
            st.owner_window_label = label.clone();
        }
        map.insert(
            child_id.to_string(),
            AgentConnection {
                id: child_id.to_string(),
                agent_type,
                status: ConnectionStatus::Connected,
                owner_window_label: label,
                owner_operation_id: op,
                ownership_generation: gen,
                connection_incarnation,
                tool_lease_registry: self.tool_lease_registry.clone(),
                parent_connection_id: Some(parent_id.to_string()),
                cmd_tx: tx,
                control_tx: test_control_sender(),
                task_abort: None,
                state: session_state,
                emitter,
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell,
                route_plan,
                origin: crate::acp::delegation::route::DelegationConnectionOrigin::CodegChild,
                route_preference: None,
                route_capability:
                    crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            },
        );
    }

    /// As [`insert_test_connection`], but keeps the command receiver ALIVE and
    /// returns it, so `send_prompt` can reach the concurrency gate (a dropped
    /// receiver fails `reserve()` with `ProcessExited` BEFORE the gate check,
    /// making the `TurnInProgress` branch untestable). Hold the returned
    /// receiver for the test's duration; drop it to simulate the process dying.
    ///
    /// Gated identically to [`insert_test_connection`] so it never compiles into
    /// a release build.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn insert_test_connection_live(
        &self,
        id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) -> tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand> {
        use crate::acp::session_state::SessionState;
        let (tx, rx, _liveness_rx) = connection_channel(4);
        let mut state = SessionState::new(
            id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        state.tool_lease_registry = self.tool_lease_registry.clone();
        state.mcp_cancel_registry = self.mcp_cancel_registry.clone();
        let connection_incarnation = state.connection_incarnation.clone();
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = crate::acp::delegation::route::test_empty_route_plan();
        let (spawn_config, observed_config) = matching_config_pair(
            String::new(),
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );
        let conn = AgentConnection {
            id: id.to_string(),
            agent_type,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation,
            tool_lease_registry: self.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(tokio::sync::RwLock::new(state)),
            emitter,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config,
            observed_config,
            terminal_shell,
            route_plan,
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        self.connections.lock().await.insert(id.to_string(), conn);
        rx
    }

    /// Bind controllable command/control lanes to the exact public state owned
    /// by a process-free shared spawn fixture. This keeps integration tests on
    /// the production dispatcher and control admission paths without launching
    /// an external ACP process.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn install_test_shared_connection_lanes(
        &self,
        id: &str,
    ) -> Option<(
        tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
        tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionControl>,
    )> {
        let (state, emitter) = self
            .shared_session_broker
            .public_state_and_emitter(id)
            .await?;
        let (agent_type, connection_incarnation) = {
            let state = state.read().await;
            (state.agent_type, state.connection_incarnation.clone())
        };
        let (cmd_tx, cmd_rx, _cmd_liveness_rx) = connection_channel(128);
        let (control_tx, control_rx, _control_liveness_rx) = connection_channel(32);
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = crate::acp::delegation::route::test_empty_route_plan();
        let (spawn_config, observed_config) = matching_config_pair(
            String::new(),
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );
        self.connections.lock().await.insert(
            id.to_string(),
            AgentConnection {
                id: id.to_string(),
                agent_type,
                status: ConnectionStatus::Connected,
                owner_window_label: "shared-http-test".into(),
                owner_operation_id: None,
                ownership_generation: 0,
                connection_incarnation,
                tool_lease_registry: self.tool_lease_registry.clone(),
                parent_connection_id: None,
                cmd_tx,
                control_tx,
                task_abort: None,
                state,
                emitter,
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell,
                route_plan,
                origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
                route_preference: None,
                route_capability:
                    crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            },
        );
        Some((cmd_rx, control_rx))
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn emit_test_shared_driver_event(&self, id: &str, event: AcpEvent) -> bool {
        let Some((state, emitter)) = self
            .shared_session_broker
            .public_state_and_emitter(id)
            .await
        else {
            return false;
        };
        emit_with_state(&state, &emitter, event).await;
        true
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn has_pending_test_shared_interaction(
        &self,
        id: &str,
        interaction_id: &str,
    ) -> bool {
        self.shared_session_broker
            .has_pending_interaction_for_test(id, interaction_id)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_agent(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        session_id: Option<String>,
        launch_inputs: AcpLaunchInputs,
        owner_window_label: String,
        emitter: EventEmitter,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        // Purpose + inherited locale from the caller's launch policy (UI system
        // language, parent effective locale for delegation, channel locale in
        // Task 4C2, or probe/test defaults).
        launch_context: ConnectionLaunchContext,
        // Detached pop-out incarnation id. When set, the connection is stamped
        // so window-close cleanup can reap by `(label, operation_id)`.
        owner_operation_id: Option<String>,
        // Delegated child: re-read parent ownership under lock at registration.
        parent_connection_id: Option<String>,
    ) -> Result<String, AcpError> {
        self.spawn_agent_with_attach_mode(
            agent_type,
            working_dir,
            session_id,
            launch_inputs,
            owner_window_label,
            emitter,
            preferred_mode_id,
            preferred_config_values,
            launch_context,
            owner_operation_id,
            parent_connection_id,
            crate::acp::session_attach::SessionAttachMode::Default,
            None,
        )
        .await
    }

    /// Like [`Self::spawn_agent`] but with an explicit session attach mode.
    ///
    /// `ResumeExistingOnly` never falls through to `session/new`, never reuses a
    /// still-retiring prior connection (always a new incarnation id), and
    /// verifies the returned external session id before SessionStarted.
    ///
    /// `preallocated_connection_id`: when set (continue-style admission handoff
    /// via [`crate::acp::delegation::broker::DelegationBroker::begin_run_admission`]),
    /// the first spawn attempt uses that incarnation id so bootstrap refuse can
    /// settle the reserving run before this method returns. Retry attempts mint
    /// a fresh id.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_agent_with_attach_mode(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        session_id: Option<String>,
        launch_inputs: AcpLaunchInputs,
        owner_window_label: String,
        emitter: EventEmitter,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        launch_context: ConnectionLaunchContext,
        owner_operation_id: Option<String>,
        parent_connection_id: Option<String>,
        session_attach_mode: crate::acp::session_attach::SessionAttachMode,
        preallocated_connection_id: Option<String>,
    ) -> Result<String, AcpError> {
        self.spawn_agent_with_attach_mode_and_workflow_binding(
            agent_type,
            working_dir,
            session_id,
            launch_inputs,
            owner_window_label,
            emitter,
            preferred_mode_id,
            preferred_config_values,
            launch_context,
            owner_operation_id,
            parent_connection_id,
            session_attach_mode,
            preallocated_connection_id,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_agent_with_attach_mode_and_workflow_binding(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        session_id: Option<String>,
        launch_inputs: AcpLaunchInputs,
        owner_window_label: String,
        emitter: EventEmitter,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        launch_context: ConnectionLaunchContext,
        owner_operation_id: Option<String>,
        parent_connection_id: Option<String>,
        session_attach_mode: crate::acp::session_attach::SessionAttachMode,
        preallocated_connection_id: Option<String>,
        workflow_child_mcp_binding: Option<
            crate::acp::delegation::workflow::WorkflowChildMcpBinding,
        >,
    ) -> Result<String, AcpError> {
        let _permit = self.admission.admit()?;
        #[cfg(test)]
        self.hold_after_admission_for_test().await;
        #[cfg(test)]
        if self
            .stub_direct_spawn
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let connection_id = preallocated_connection_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            self.insert_test_connection(
                &connection_id,
                agent_type,
                working_dir.as_ref().map(PathBuf::from),
                emitter,
            )
            .await;
            return Ok(connection_id);
        }

        // Connection dedup: when resuming an agent session (session_id is
        // Some), look for a live AgentConnection that already represents
        // the same external session in the same working_dir for the same
        // agent_type and is not torn down. If found, reuse it instead of
        // spawning a fresh process — this is what makes a browser refresh
        // mid-turn re-attach to the existing live state rather than orphan it.
        //
        // ResumeExistingOnly must NOT reuse a still-retiring prior connection:
        // always mint a new connection incarnation id.
        let working_dir_path = working_dir.as_ref().map(PathBuf::from);
        let skip_dedup = session_attach_mode.is_resume_existing_only();

        // Acquire a per-(agent, working_dir, session_id) async mutex so two
        // concurrent connects for the same logical session can't both miss
        // dedup during the handshake window. The lookup → spawn → wait-for-
        // SessionStarted critical section runs under this lock; the second
        // waiter, on entry, observes the first call's connection with
        // `state.external_id` already populated and returns its id via
        // `find_connection_for_reuse`. Skipped entirely when `session_id`
        // is None (fresh sessions can't dedup — by design — since the
        // agent assigns the id).
        let session_id_for_log = session_id.clone();
        let dedup_lock = if !skip_dedup {
            if let Some(sid) = session_id.as_deref() {
                let key = SpawnDedupKey {
                    agent_type,
                    working_dir: working_dir_path.clone(),
                    session_id: sid.to_string(),
                };
                let mu = {
                    let mut locks = self.spawn_locks.lock().await;
                    locks
                        .entry(key)
                        .or_insert_with(|| Arc::new(Mutex::new(())))
                        .clone()
                };
                Some(mu.lock_owned().await)
            } else {
                None
            }
        } else {
            None
        };

        if !skip_dedup {
            if let Some(existing) = self
                .find_connection_for_reuse(
                    agent_type,
                    working_dir_path.as_ref(),
                    session_id.as_deref(),
                )
                .await
            {
                let existing_fp = {
                    let map = self.connections.lock().await;
                    map.get(&existing)
                        .map(|c| c.spawn_config.delegation_route.clone())
                        .unwrap_or_default()
                };
                match route_reuse_decision(
                    &existing_fp,
                    &launch_inputs.route_plan.fingerprint,
                    &existing,
                ) {
                    RouteReuseDecision::Reuse => {
                        // Detached cold connect supplies owner_operation_id. Only
                        // reuse a connection already stamped for this incarnation
                        // (matching label + op). Never return a main-owned (or
                        // other-owner) connection as a newly stamped cold lease —
                        // that would leave FE with a fake lease and bare abort
                        // cleanup could kill the prior owner.
                        if let Some(ref want_op) = owner_operation_id {
                            let (label, op) = {
                                let map = self.connections.lock().await;
                                match map.get(&existing) {
                                    Some(c) => {
                                        (c.owner_window_label.clone(), c.owner_operation_id.clone())
                                    }
                                    None => {
                                        // Raced out of the map; treat as missing.
                                        return Err(AcpError::ConnectionNotFound(existing));
                                    }
                                }
                            };
                            if cold_connect_reuse_allowed(
                                &label,
                                op.as_deref(),
                                &owner_window_label,
                                want_op,
                            ) {
                                tracing::info!(
                                    "[ACP] reusing same-incarnation connection id={} \
                                 session_id={} op={}",
                                    existing,
                                    session_id.as_deref().unwrap_or(""),
                                    want_op
                                );
                                return Ok(existing);
                            }
                            tracing::info!(
                                "[ACP] refuse cold dedup: existing={} window={} op={:?} \
                             want_window={} want_op={}",
                                existing,
                                label,
                                op,
                                owner_window_label,
                                want_op
                            );
                            return Err(AcpError::protocol(format!(
                                "existing connection {existing} is not owned by this \
                             pop-out incarnation (window={label}, op={op:?}); \
                             refuse cold connect reuse without ownership stamp"
                            )));
                        }
                        tracing::info!(
                            "[ACP] reusing connection id={} for session_id={}",
                            existing,
                            session_id.as_deref().unwrap_or("")
                        );
                        // Reuse must not resolve, validate, or apply newly loaded
                        // terminal/route settings — the live connection keeps its
                        // launch-time snapshot.
                        return Ok(existing);
                    }
                    RouteReuseDecision::Conflict {
                        existing_connection_id,
                    } => {
                        tracing::info!(
                            "[ACP] session route conflict existing={} requested_fp={}",
                            existing_connection_id,
                            launch_inputs.route_plan.fingerprint
                        );
                        return Err(AcpError::SessionRouteConflict {
                            existing_connection_id,
                        });
                    }
                }
            }
        } // !skip_dedup

        // Only the no-reuse branch finalizes an immutable shell snapshot.
        // The route plan is already resolved and passes through unchanged.
        let AcpLaunchConfig {
            runtime_env,
            terminal_shell,
            route_plan,
            origin,
            route_preference,
            route_capability,
        } = finalize_acp_launch_config(launch_inputs, agent_type)?;

        // Explicit max-two-attempt root state machine: Codeg request, then one
        // safe-native fallback only for typed RouteSpecific bootstrap failures.
        // Child and Fatal never retry.
        let mut attempt_plan = route_plan;
        // Hidden generation (title/translate) never carries Codeg MCP / delegation injection.
        let skip_delegation_injection = launch_context.purpose.is_hidden_generation();
        // Authoritative route record after exclusivity validation (Task 13).
        if !skip_delegation_injection {
            if let Some(inj) = self.delegation_snapshot() {
                if let Err(e) = inj
                    .metrics
                    .validate_and_record_route(agent_type, &attempt_plan)
                {
                    return Err(AcpError::protocol(format!(
                        "managed plan violates exclusive route surfaces: {}",
                        e.stable_code()
                    )));
                }
                let suppression =
                    crate::acp::connection::suppression_application_for_plan(&attempt_plan);
                crate::acp::delegation::metrics::DelegationAuditRecord::route(
                    "pending-spawn",
                    None,
                    agent_type,
                    &attempt_plan,
                    suppression,
                )
                .emit_route_resolved();
            } else {
                // Tests without injection still enforce exclusivity.
                attempt_plan
                    .assert_exclusive()
                    .map_err(|e| AcpError::protocol(e.to_string()))?;
            }
        } else {
            attempt_plan
                .assert_exclusive()
                .map_err(|e| AcpError::protocol(e.to_string()))?;
        }
        let mut attempt = 0u8;
        let connection_id = loop {
            attempt += 1;
            // Attempt 1 may reuse a pre-bootstrap admission incarnation so
            // ResumeExistingOnly refuse can settle before this method returns.
            // Later attempts always mint a new id (new incarnation).
            let connection_id = if attempt == 1 {
                preallocated_connection_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            tracing::info!(
                "[ACP] spawning connection id={} owner_window={} agent={:?} \
                 attempt={} effective={:?} preallocated={}",
                connection_id,
                owner_window_label,
                agent_type,
                attempt,
                attempt_plan.effective,
                attempt == 1 && preallocated_connection_id.is_some()
            );

            let injection = if skip_delegation_injection {
                None
            } else {
                self.delegation_snapshot()
            };

            let mut registered = match spawn_agent_connection(
                connection_id.clone(),
                agent_type,
                working_dir.clone(),
                session_id.clone(),
                runtime_env.clone(),
                terminal_shell.clone(),
                attempt_plan.clone(),
                origin,
                route_preference,
                route_capability.clone(),
                owner_window_label.clone(),
                owner_operation_id.clone(),
                parent_connection_id.clone(),
                emitter.clone(),
                self.connections.clone(),
                preferred_mode_id.clone(),
                preferred_config_values.clone(),
                injection,
                workflow_child_mcp_binding.clone(),
                launch_context.clone(),
                session_attach_mode,
                self.tool_lease_registry.clone(),
                self.mcp_cancel_registry.clone(),
                None,
            )
            .await
            {
                Ok(registered) => registered,
                Err(e) => {
                    // Spawn-time failures (SDK missing, shell, etc.) are Fatal —
                    // never route-specific fallback.
                    return Err(e);
                }
            };
            registered.activate_driver();
            let SpawnHandshake {
                session_started_rx,
                route_bootstrap_rx,
            } = registered.handshake;

            // A post-registration Fatal/RouteSpecific bootstrap outcome must
            // break the session-id dedup wait immediately. Ready is retained
            // until SessionStarted settles so reuse keeps its prior fence.
            let route_outcome = if dedup_lock.is_some() {
                let timeout = self.spawn_handshake_timeout;
                let started_at = std::time::Instant::now();
                let mut route_bootstrap_rx = route_bootstrap_rx;
                let mut session_wait =
                    Box::pin(wait_for_session_started(session_started_rx, timeout));
                tokio::select! {
                    (outcome, elapsed) = &mut session_wait => {
                        tracing::info!(
                            "[ACP] dedup_wait connection_id={} session_id={} outcome={} \
                             elapsed_ms={} timeout_ms={}",
                            connection_id,
                            session_id_for_log.as_deref().unwrap_or(""),
                            outcome.as_str(),
                            elapsed.as_millis(),
                            timeout.as_millis(),
                        );
                        route_bootstrap_rx.await
                    }
                    route = &mut route_bootstrap_rx => {
                        match route {
                            Ok(RouteBootstrapOutcome::Ready) => {
                                let (outcome, elapsed) = session_wait.await;
                                tracing::info!(
                                    "[ACP] dedup_wait connection_id={} session_id={} outcome={} \
                                     elapsed_ms={} timeout_ms={}",
                                    connection_id,
                                    session_id_for_log.as_deref().unwrap_or(""),
                                    outcome.as_str(),
                                    elapsed.as_millis(),
                                    timeout.as_millis(),
                                );
                                Ok(RouteBootstrapOutcome::Ready)
                            }
                            other => {
                                tracing::info!(
                                    "[ACP] dedup_wait connection_id={} session_id={} outcome=bootstrap_failed \
                                     elapsed_ms={} timeout_ms={}",
                                    connection_id,
                                    session_id_for_log.as_deref().unwrap_or(""),
                                    started_at.elapsed().as_millis(),
                                    timeout.as_millis(),
                                );
                                other
                            }
                        }
                    }
                }
            } else {
                route_bootstrap_rx.await
            };

            match route_outcome {
                Ok(RouteBootstrapOutcome::Ready) => break connection_id,
                Ok(RouteBootstrapOutcome::RouteSpecific(reason))
                    if origin == DelegationConnectionOrigin::Root && attempt == 1 =>
                {
                    tracing::warn!(
                        "[ACP] route bootstrap RouteSpecific ({reason:?}); \
                         tearing down unexposed attempt and safe-native fallback"
                    );
                    // Attempt 2 only after teardown observes map absence.
                    self.teardown_unexposed_attempt(&connection_id).await?;
                    attempt_plan = safe_native_fallback(&attempt_plan, reason);
                    // Count safe fallback once at the actual decision boundary.
                    if let Some(inj) = self.delegation_snapshot() {
                        inj.metrics.record_route(agent_type, &attempt_plan);
                        let suppression =
                            crate::acp::connection::suppression_application_for_plan(&attempt_plan);
                        crate::acp::delegation::metrics::DelegationAuditRecord::route(
                            &connection_id,
                            None,
                            agent_type,
                            &attempt_plan,
                            suppression,
                        )
                        .emit_route_resolved();
                    }
                    // Second attempt cannot recurse/retry again (attempt==2).
                    continue;
                }
                Ok(RouteBootstrapOutcome::RouteSpecific(reason)) => {
                    self.teardown_unexposed_attempt(&connection_id).await?;
                    return Err(AcpError::RouteUnavailable { reason });
                }
                Ok(RouteBootstrapOutcome::Fatal(error)) => {
                    self.teardown_unexposed_attempt(&connection_id).await?;
                    return Err(error);
                }
                Err(_) => {
                    // Connection task dropped without bootstrap (process died).
                    self.teardown_unexposed_attempt(&connection_id).await?;
                    return Err(AcpError::ProcessExited);
                }
            }
        };

        drop(dedup_lock);

        Ok(connection_id)
    }

    /// Tear down a partial spawn that never exposed Connected: terminate the
    /// connection task, revoke companion token/lease with awaited locks, and
    /// observe actual map removal before returning so the partial process
    /// cannot race a replacement or win session-id dedup.
    ///
    /// A queued `Disconnect` alone is insufficient when bootstrap fails before
    /// `run_conversation_loop` (the command is never drained). Abort the task
    /// instead, then wait for [`ConnectionCleanupGuard`] to remove the entry.
    /// Does **not** force-remove a still-live map entry. Success requires
    /// observed map absence after revoke + terminate request; timeout is
    /// fail-closed ([`AcpError::ProcessExited`]) so root fallback never starts
    /// attempt 2 against a still-mapped partial connection.
    async fn teardown_unexposed_attempt(&self, connection_id: &str) -> Result<(), AcpError> {
        self.teardown_unexposed_attempt_with_waits(
            connection_id,
            TEARDOWN_MAP_WAIT_PRIMARY,
            TEARDOWN_MAP_WAIT_EXTENDED,
        )
        .await
    }

    async fn teardown_unexposed_attempt_with_waits(
        &self,
        connection_id: &str,
        primary: Duration,
        extended: Duration,
    ) -> Result<(), AcpError> {
        // Snapshot handles under the map lock, then release before awaiting
        // state/token locks so we never hold connections + state together.
        let retained_child_pid = self
            .shared_session_broker
            .driver_child_pid_for_connection(connection_id)
            .await;
        let (task_abort, state, control_tx, child_pid) = {
            let map = self.connections.lock().await;
            match map.get(connection_id) {
                Some(conn) => (
                    conn.task_abort.clone(),
                    Some(Arc::clone(&conn.state)),
                    Some(conn.control_tx.clone()),
                    Some(Arc::clone(&conn.child_pid)),
                ),
                None => (None, None, None, retained_child_pid),
            }
        };

        // Already absent: nothing to clean up (success).
        if task_abort.is_none()
            && state.is_none()
            && child_pid
                .as_ref()
                .is_none_or(|pid| pid.load(std::sync::atomic::Ordering::SeqCst) == 0)
        {
            return Ok(());
        }

        self.record_disconnect_intent(connection_id, AcpDisconnectOrigin::AbandonedConnect);

        // 1) Awaited token revoke (never try_read — must not skip under contention).
        if let Some(state) = state {
            let token = state.read().await.delegation_token.clone();
            if let (Some(tok), Some(inj)) = (token, self.delegation_snapshot()) {
                inj.leases.revoke(&tok).await;
                inj.tokens.revoke(&tok).await;
            }
        }

        // 2) Terminate the unexposed attempt: abort first (works pre-loop),
        //    then best-effort Disconnect if the task already reached the loop.
        if let Some(abort) = task_abort {
            abort.abort();
        }
        if let Some(tx) = control_tx {
            let _ = tx.try_send(ConnectionControl::Disconnect);
        }

        // 3) Observe actual map removal before Ok (no force-remove race).
        let deadline = tokio::time::Instant::now() + primary;
        loop {
            {
                let map = self.connections.lock().await;
                if !map.contains_key(connection_id)
                    && child_pid
                        .as_ref()
                        .is_none_or(|pid| pid.load(std::sync::atomic::Ordering::SeqCst) == 0)
                {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::error!(
                    "[ACP] teardown_unexposed_attempt timed out waiting for \
                     removal of {connection_id} after abort+revoke; \
                     not force-removing (would race a live SessionStarted entry)"
                );
                // Keep waiting briefly for delayed cleanup-guard spawn path.
                let extended_deadline = tokio::time::Instant::now() + extended;
                while tokio::time::Instant::now() < extended_deadline {
                    {
                        let map = self.connections.lock().await;
                        if !map.contains_key(connection_id)
                            && child_pid.as_ref().is_none_or(|pid| {
                                pid.load(std::sync::atomic::Ordering::SeqCst) == 0
                            })
                        {
                            return Ok(());
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                tracing::error!(
                    "[ACP] teardown_unexposed_attempt: {connection_id} still present \
                     after extended wait; fail closed (no native fallback)"
                );
                return Err(AcpError::ProcessExited);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Test/harness surface for [`Self::teardown_unexposed_attempt`].
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn teardown_unexposed_for_test(&self, connection_id: &str) -> Result<(), AcpError> {
        self.teardown_unexposed_attempt(connection_id).await
    }

    /// Like [`Self::teardown_unexposed_for_test`] with explicit wait bounds
    /// (deterministic stuck-cleanup tests; no global timeout override).
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn teardown_unexposed_for_test_with_waits(
        &self,
        connection_id: &str,
        primary: Duration,
        extended: Duration,
    ) -> Result<(), AcpError> {
        self.teardown_unexposed_attempt_with_waits(connection_id, primary, extended)
            .await
    }

    pub fn configure_shared_client_lease_ttl(&self, lease_ttl: Duration) {
        self.shared_session_broker
            .configure_client_lease_ttl(lease_ttl);
    }

    pub async fn sweep_shared_sessions(
        &self,
        idle_timeout: Option<Duration>,
        client_lease_ttl: Duration,
    ) -> SharedSweepReport {
        let candidates = self
            .shared_session_broker
            .evaluate_idle(idle_timeout, client_lease_ttl)
            .await;
        let mut report = SharedSweepReport::default();
        for candidate in candidates {
            match candidate.kind {
                SharedSweepCandidateKind::Failed => {
                    if self
                        .shared_session_broker
                        .remove_sweep_candidate(&candidate)
                        .await
                    {
                        report.removed = true;
                        report.removed_count += 1;
                    }
                }
                SharedSweepCandidateKind::Ready => {
                    let Some(idle_timeout) = idle_timeout else {
                        continue;
                    };
                    let Some(transition) = self
                        .shared_session_broker
                        .begin_idle_reclaim(candidate, idle_timeout)
                        .await
                    else {
                        continue;
                    };
                    let connection_id = transition.candidate.connection_id.clone();
                    let generation = transition.candidate.generation;
                    let _ = self
                        .publish_shared_events(&connection_id, transition.events)
                        .await;
                    let started = tokio::time::Instant::now();
                    let cleanup_complete = self
                        .teardown_shared_driver(
                            &connection_id,
                            transition.force_abort,
                            AcpDisconnectOrigin::IdleTimeout,
                        )
                        .await;
                    self.shared_session_broker
                        .record_cleanup_duration(started.elapsed());
                    if cleanup_complete {
                        if self
                            .shared_session_broker
                            .remove_sweep_candidate(&transition.candidate)
                            .await
                        {
                            self.shared_launches
                                .lock()
                                .await
                                .remove(&(connection_id, generation));
                            report.removed = true;
                            report.removed_count += 1;
                        }
                    } else {
                        self.shared_session_broker.record_cleanup_incomplete();
                        report.cleanup_incomplete += 1;
                    }
                }
                SharedSweepCandidateKind::AbandonedEphemeral => {
                    let Some(transition) = self
                        .shared_session_broker
                        .begin_abandoned_ephemeral_reclaim(candidate, client_lease_ttl)
                        .await
                    else {
                        continue;
                    };
                    let connection_id = transition.candidate.connection_id.clone();
                    let generation = transition.candidate.generation;
                    let _ = self
                        .publish_shared_events(&connection_id, transition.events)
                        .await;
                    let started = tokio::time::Instant::now();
                    let cleanup_complete = self
                        .teardown_shared_driver(
                            &connection_id,
                            transition.force_abort,
                            AcpDisconnectOrigin::IdleTimeout,
                        )
                        .await;
                    self.shared_session_broker
                        .record_cleanup_duration(started.elapsed());
                    if cleanup_complete {
                        if self
                            .shared_session_broker
                            .remove_sweep_candidate(&transition.candidate)
                            .await
                        {
                            self.shared_launches
                                .lock()
                                .await
                                .remove(&(connection_id, generation));
                            report.removed = true;
                            report.removed_count += 1;
                        }
                    } else {
                        self.shared_session_broker.record_cleanup_incomplete();
                        report.cleanup_incomplete += 1;
                    }
                }
            }
        }
        report
    }

    pub async fn terminate_shared_session(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<(), AcpError> {
        let transition = self
            .shared_session_broker
            .begin_termination(connection_id, generation)
            .await?;
        if let Err(error) = self
            .publish_shared_events(connection_id, transition.events)
            .await
        {
            tracing::warn!(
                "[ACP] shared termination event publication failed connection={} generation={} code={:?}",
                connection_id,
                generation,
                error.code()
            );
        }
        let started = tokio::time::Instant::now();
        let cleanup_complete = self
            .teardown_shared_driver(
                connection_id,
                transition.force_abort,
                AcpDisconnectOrigin::ExplicitUser,
            )
            .await;
        self.shared_session_broker
            .record_cleanup_duration(started.elapsed());
        if !cleanup_complete {
            self.shared_session_broker.record_cleanup_incomplete();
            return Err(AcpError::ProcessExited);
        }
        if !self
            .shared_session_broker
            .remove_sweep_candidate(&transition.candidate)
            .await
        {
            return Err(SharedSessionError::GenerationStale.into());
        }
        self.shared_launches
            .lock()
            .await
            .remove(&(connection_id.to_string(), generation));
        Ok(())
    }

    async fn teardown_shared_driver(
        &self,
        connection_id: &str,
        force_abort: bool,
        origin: AcpDisconnectOrigin,
    ) -> bool {
        #[cfg(any(test, feature = "test-utils"))]
        self.shared_teardown_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if force_abort {
            return self.teardown_unexposed_attempt(connection_id).await.is_ok()
                && !self.connections.lock().await.contains_key(connection_id);
        }

        let child_pid = self
            .shared_session_broker
            .driver_child_pid_for_connection(connection_id)
            .await;
        if self.connections.lock().await.contains_key(connection_id)
            && self
                .disconnect_with_origin(connection_id, origin)
                .await
                .is_err()
        {
            return false;
        }
        let deadline =
            tokio::time::Instant::now() + TEARDOWN_MAP_WAIT_PRIMARY + TEARDOWN_MAP_WAIT_EXTENDED;
        loop {
            let absent = !self.connections.lock().await.contains_key(connection_id);
            let process_absent = child_pid
                .as_ref()
                .is_none_or(|pid| pid.load(std::sync::atomic::Ordering::SeqCst) == 0);
            if absent && process_absent {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::error!(
                    "[ACP] shared teardown incomplete connection={} map_absent={} process_absent={}",
                    connection_id,
                    absent,
                    process_absent
                );
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Bump `last_activity_at` for a live connection so the idle sweep
    /// won't reap it. Used by the frontend keepalive loop to protect
    /// connections backing currently-open conversation tabs (the
    /// frontend is the only side that knows which tabs the user has
    /// open). Silently no-ops if the connection is missing or already
    /// in a terminal state — touch must never resurrect a dead
    /// connection or contend with the spawn/disconnect paths.
    pub async fn touch(&self, conn_id: &str) -> bool {
        if self
            .shared_session_broker
            .is_managed_connection(conn_id)
            .await
        {
            return false;
        }
        let state_arc = {
            let connections = self.connections.lock().await;
            match connections.get(conn_id) {
                Some(conn) => conn.state.clone(),
                None => return false,
            }
        };
        let mut state = state_arc.write().await;
        if matches!(
            state.status,
            ConnectionStatus::Disconnected | ConnectionStatus::Error
        ) {
            return false;
        }
        state.last_activity_at = chrono::Utc::now();
        true
    }

    /// Disconnect connections that have been idle longer than `idle_timeout`.
    /// "Idle" means: status is `Connected`, no `pending_permission`, no
    /// launched-but-unresolved background work (async sub-agent / background
    /// shell — disconnecting kills the agent CLI and the background work with
    /// it), and no activity (no events, no commands) for at least
    /// `idle_timeout`. `Prompting` connections are always preserved (a turn is
    /// in flight). Returns the number of connections that were disconnected.
    pub async fn sweep_idle(&self, idle_timeout: Duration) -> usize {
        let now = chrono::Utc::now();
        let timeout = match chrono::Duration::from_std(idle_timeout) {
            Ok(d) => d,
            Err(_) => return 0,
        };
        // Snapshot lease (id, owner_label, operation_id, generation, incarnation) then
        // re-validate under the same lock before remove so a concurrent rebind
        // cannot be killed by a stale idle selection.
        let shared_connections = self.shared_session_broker.managed_connection_ids().await;
        let candidates: Vec<(String, String, Option<String>, u64, String)> = {
            let connections = self.connections.lock().await;
            let mut victims = Vec::new();
            for (id, conn) in connections.iter() {
                if shared_connections.contains(id) {
                    continue;
                }
                let Ok(state) = conn.state.try_read() else {
                    // Per-state writer holds the lock; a future tick will
                    // re-evaluate this entry. Don't block the connections
                    // mutex on it.
                    continue;
                };
                if state.status != ConnectionStatus::Connected {
                    continue;
                }
                if state.pending_permission.is_some() {
                    continue;
                }
                if state.has_active_background_work(now) {
                    continue;
                }
                let elapsed = now.signed_duration_since(state.last_activity_at);
                if elapsed >= timeout {
                    victims.push((
                        id.clone(),
                        conn.owner_window_label.clone(),
                        conn.owner_operation_id.clone(),
                        conn.ownership_generation,
                        conn.connection_incarnation.clone(),
                    ));
                }
            }
            victims
        };
        let mut disconnected = 0;
        for (id, expected_owner, expected_op, expected_gen, expected_incarnation) in candidates {
            #[cfg(test)]
            {
                let hook = self
                    .disconnect_final_cas_hook
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if let Some(hook) = hook {
                    hook.reached.notify_one();
                    hook.resume.notified().await;
                }
            }
            // Final ownership and idle validation is exclusive with removal.
            // A survivor must never be fenced because it lost this final CAS.
            let removed = {
                let mut connections = self.connections.lock().await;
                let Some(conn) = connections.get(&id) else {
                    continue;
                };
                if conn.owner_window_label != expected_owner
                    || conn.owner_operation_id != expected_op
                    || conn.ownership_generation != expected_gen
                    || conn.connection_incarnation != expected_incarnation
                {
                    continue;
                }
                let state_arc = Arc::clone(&conn.state);
                let Ok(state) = state_arc.try_write() else {
                    continue;
                };
                if state.status != ConnectionStatus::Connected
                    || state.pending_permission.is_some()
                    || state.has_active_background_work(now)
                    || now.signed_duration_since(state.last_activity_at) < timeout
                {
                    continue;
                }
                self.record_disconnect_intent(&id, AcpDisconnectOrigin::IdleTimeout);
                let removed = connections.remove(&id);
                drop(state);
                removed
            };
            if let Some(conn) = removed {
                self.clear_tool_leases(&id, &conn.connection_incarnation)
                    .await;
                tracing::info!("[ACP] idle sweep disconnecting connection={}", id);
                let _ = conn.control_tx.send(ConnectionControl::Disconnect).await;
                disconnected += 1;
            }
        }
        disconnected
    }

    /// Compare each running connection's spawn-time **agent** config fingerprint
    /// against a freshly recomputed one (keyed by agent type in `fresh`) and
    /// notify those that drifted. Shell drift is tracked separately via
    /// [`Self::refresh_terminal_shell_staleness`].
    ///
    /// Emit policy, per connection:
    /// - updates only the agent component of `observed_config` (plus `agent_kind`);
    /// - emits `SessionConfigStale` only when that observed component **or** the
    ///   effective stale kind changes — a no-op save stays silent, a second real
    ///   change re-emits so a dismissed banner reappears;
    /// - effective kind prefers shell drift over agent drift (see
    ///   [`effective_stale_kind`]).
    ///
    /// Returns the count of affected connections whose **agent** component is
    /// currently stale (for the settings-side "N sessions need restart" toast).
    /// Connections whose agent type isn't in `fresh` are left untouched.
    ///
    /// `emit_with_state` is deferred until AFTER the connections-map lock is
    /// released (we collect targets first) so the SessionState write lock is
    /// never taken while holding the map lock.
    pub async fn refresh_connection_staleness(
        &self,
        fresh: &HashMap<AgentType, String>,
        kind: ConfigStaleKind,
    ) -> usize {
        let mut targets = Vec::new();
        let mut stale_count = 0usize;
        {
            let mut connections = self.connections.lock().await;
            for conn in connections.values_mut() {
                let Some(current) = fresh.get(&conn.agent_type) else {
                    continue;
                };
                let prev_agent = conn.observed_config.fingerprint.agent_config.clone();
                let prev_kind = conn.observed_config.agent_kind;
                let prev_effective = effective_stale_kind(conn);

                conn.observed_config.fingerprint.agent_config = current.clone();
                conn.observed_config.agent_kind = kind;

                let agent_stale =
                    conn.observed_config.fingerprint.agent_config != conn.spawn_config.agent_config;
                if agent_stale {
                    stale_count += 1;
                }

                let new_effective = effective_stale_kind(conn);
                let observed_changed = prev_agent != conn.observed_config.fingerprint.agent_config
                    || prev_kind != conn.observed_config.agent_kind;
                if observed_changed || prev_effective != new_effective {
                    let stale = new_effective.is_some();
                    let emit_kind = new_effective.unwrap_or(kind);
                    targets.push((
                        Arc::clone(&conn.state),
                        conn.emitter.clone(),
                        stale,
                        emit_kind,
                    ));
                }
            }
        }
        for (state, emitter, stale, kind) in targets {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::SessionConfigStale { stale, kind },
            )
            .await;
        }
        stale_count
    }

    /// Compare every running connection's spawn-time terminal-shell selection
    /// key against the freshly saved global setting and notify those that
    /// drifted. Agent-config drift is tracked separately via
    /// [`Self::refresh_connection_staleness`].
    ///
    /// Updates only the `terminal_shell` observed component. Emits after the
    /// connections-map lock is released, and only when that component or the
    /// effective stale kind changes. Returns the count of connections whose
    /// **shell** component is currently stale.
    pub async fn refresh_terminal_shell_staleness(&self, selection_key: &str) -> usize {
        let mut targets = Vec::new();
        let mut stale_count = 0usize;
        {
            let mut connections = self.connections.lock().await;
            for conn in connections.values_mut() {
                let prev_shell = conn.observed_config.fingerprint.terminal_shell.clone();
                let prev_effective = effective_stale_kind(conn);

                if prev_shell != selection_key {
                    conn.observed_config.fingerprint.terminal_shell = selection_key.to_string();
                }

                let shell_stale = conn.observed_config.fingerprint.terminal_shell
                    != conn.spawn_config.terminal_shell;
                if shell_stale {
                    stale_count += 1;
                }

                let new_effective = effective_stale_kind(conn);
                let observed_changed =
                    prev_shell != conn.observed_config.fingerprint.terminal_shell;
                if observed_changed || prev_effective != new_effective {
                    let stale = new_effective.is_some();
                    let emit_kind = new_effective.unwrap_or(ConfigStaleKind::TerminalShell);
                    targets.push((
                        Arc::clone(&conn.state),
                        conn.emitter.clone(),
                        stale,
                        emit_kind,
                    ));
                }
            }
        }
        for (state, emitter, stale, kind) in targets {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::SessionConfigStale { stale, kind },
            )
            .await;
        }
        stale_count
    }

    /// Recompute the observed route fingerprint for managed root connections
    /// against a new global policy/enabled pair. Forced children and unmanaged
    /// agents are skipped. Never mutates `route_plan` / argv / env.
    pub async fn refresh_delegation_route_staleness(
        &self,
        global_policy: crate::acp::delegation::route::DelegationRoutePolicy,
        delegation_enabled: bool,
    ) -> usize {
        self.refresh_delegation_route_staleness_filtered(global_policy, delegation_enabled, None)
            .await
    }

    /// Like [`Self::refresh_delegation_route_staleness`] but only connections
    /// bound to `conversation_id`.
    pub async fn refresh_delegation_route_staleness_for_conversation(
        &self,
        conversation_id: i32,
        global_policy: crate::acp::delegation::route::DelegationRoutePolicy,
        delegation_enabled: bool,
    ) -> usize {
        self.refresh_delegation_route_staleness_filtered(
            global_policy,
            delegation_enabled,
            Some(conversation_id),
        )
        .await
    }

    async fn refresh_delegation_route_staleness_filtered(
        &self,
        global_policy: crate::acp::delegation::route::DelegationRoutePolicy,
        delegation_enabled: bool,
        only_conversation_id: Option<i32>,
    ) -> usize {
        use crate::acp::delegation::route::{
            comparison_route_fingerprint, is_managed_agent, DelegationConnectionOrigin,
        };

        let mut targets = Vec::new();
        let mut stale_count = 0usize;
        {
            let mut connections = self.connections.lock().await;
            for conn in connections.values_mut() {
                if !is_managed_agent(conn.agent_type) {
                    continue;
                }
                if conn.origin == DelegationConnectionOrigin::CodegChild {
                    continue;
                }
                if let Some(only_cid) = only_conversation_id {
                    let cid = conn.state.try_read().ok().and_then(|s| s.conversation_id);
                    if cid != Some(only_cid) {
                        continue;
                    }
                }

                let prev_route = conn.observed_config.fingerprint.delegation_route.clone();
                let prev_effective = effective_stale_kind(conn);

                let new_fp = comparison_route_fingerprint(
                    conn.agent_type,
                    conn.origin,
                    conn.route_preference,
                    global_policy,
                    delegation_enabled,
                    &conn.route_capability,
                );
                conn.observed_config.fingerprint.delegation_route = new_fp;

                let route_stale = conn.observed_config.fingerprint.delegation_route
                    != conn.spawn_config.delegation_route;
                if route_stale {
                    stale_count += 1;
                }

                let new_effective = effective_stale_kind(conn);
                let observed_changed =
                    prev_route != conn.observed_config.fingerprint.delegation_route;
                if observed_changed || prev_effective != new_effective {
                    let stale = new_effective.is_some();
                    let emit_kind = new_effective.unwrap_or(ConfigStaleKind::DelegationRoute);
                    targets.push((
                        Arc::clone(&conn.state),
                        conn.emitter.clone(),
                        stale,
                        emit_kind,
                    ));
                }
            }
        }
        for (state, emitter, stale, kind) in targets {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::SessionConfigStale { stale, kind },
            )
            .await;
        }
        stale_count
    }

    /// Update a row-less connected draft's observed route preference.
    /// Rejects persisted roots, forced children, and unmanaged agents.
    /// Never mutates `route_plan`, process argv/env, or session metadata.
    pub async fn set_draft_delegation_route_preference(
        &self,
        connection_id: &str,
        route_override: Option<crate::acp::delegation::route::DelegationRoutePolicy>,
        global_policy: crate::acp::delegation::route::DelegationRoutePolicy,
        delegation_enabled: bool,
    ) -> Result<(), AcpError> {
        use crate::acp::delegation::route::{
            comparison_route_fingerprint, is_managed_agent, DelegationConnectionOrigin,
        };

        let mut targets = Vec::new();
        {
            let mut connections = self.connections.lock().await;
            let conn = connections
                .get_mut(connection_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(connection_id.to_string()))?;

            if !is_managed_agent(conn.agent_type) {
                return Err(AcpError::protocol(
                    "draft route preference is only valid for managed agents",
                ));
            }
            if conn.origin == DelegationConnectionOrigin::CodegChild {
                return Err(AcpError::protocol(
                    "draft route preference is not allowed on forced Codeg children",
                ));
            }
            let conversation_id = {
                let state = conn.state.read().await;
                state.conversation_id
            };
            if conversation_id.is_some() {
                return Err(AcpError::protocol(
                    "draft route preference is only allowed on row-less draft connections",
                ));
            }

            let prev_route = conn.observed_config.fingerprint.delegation_route.clone();
            let prev_effective = effective_stale_kind(conn);

            conn.route_preference = route_override;
            conn.observed_config.fingerprint.delegation_route = comparison_route_fingerprint(
                conn.agent_type,
                conn.origin,
                conn.route_preference,
                global_policy,
                delegation_enabled,
                &conn.route_capability,
            );

            let new_effective = effective_stale_kind(conn);
            let observed_changed = prev_route != conn.observed_config.fingerprint.delegation_route;
            if observed_changed || prev_effective != new_effective {
                let stale = new_effective.is_some();
                let emit_kind = new_effective.unwrap_or(ConfigStaleKind::DelegationRoute);
                targets.push((
                    Arc::clone(&conn.state),
                    conn.emitter.clone(),
                    stale,
                    emit_kind,
                ));
            }
        }
        for (state, emitter, stale, kind) in targets {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::SessionConfigStale { stale, kind },
            )
            .await;
        }
        Ok(())
    }

    /// Look up an existing live connection that we can reuse instead of
    /// spawning a new process. Reuse criteria, ALL must hold:
    /// - `session_id` is Some (we never dedup speculative / fresh connects)
    /// - the connection's `state.external_id` equals `session_id`
    /// - the connection's `agent_type` equals the requested one
    /// - the connection's `working_dir` equals the requested one (compared as
    ///   `Option<PathBuf>` so canonicalization is the caller's concern)
    /// - the connection's `state.status` is neither `Disconnected` nor `Error`
    ///
    /// Per-session state is acquired via `read().await` rather than `try_read`:
    /// the only writer is `emit_with_state`, whose critical section is
    /// microseconds (apply_event + seq++ + broadcast::send), so contention
    /// resolves quickly and the previous "skip on writer" behavior was just
    /// trading correctness (false-negative dedup → duplicate process spawn)
    /// for an imperceptible latency win. The connections-map mutex is held
    /// across the awaits — fine because no path takes `state.write()` while
    /// holding the connections mutex (no lock-cycle).
    pub(crate) async fn find_connection_for_reuse(
        &self,
        agent_type: AgentType,
        working_dir: Option<&PathBuf>,
        session_id: Option<&str>,
    ) -> Option<String> {
        // No session_id → caller is opening a fresh session; never dedup.
        let session_id = session_id?;
        let connections = self.connections.lock().await;
        for (id, conn) in connections.iter() {
            if conn.agent_type != agent_type {
                continue;
            }
            let state = conn.state.read().await;
            if state.external_id.as_deref() != Some(session_id) {
                continue;
            }
            if state.working_dir.as_ref() != working_dir {
                continue;
            }
            if matches!(
                state.status,
                ConnectionStatus::Disconnected | ConnectionStatus::Error
            ) {
                continue;
            }
            return Some(id.clone());
        }
        None
    }

    /// Reject an external prompt if a durable continuation owns its conversation.
    /// Called while the connection's `prompt_lock` is held, before prompt side
    /// effects. Store failures remain protocol failures so admission never fails
    /// open when the continuation state is unavailable.
    async fn admit_external_prompt(
        &self,
        state_arc: &Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
        caller_conversation_id: Option<i32>,
        source: PromptAdmissionSource,
    ) -> Result<(), AcpError> {
        let conversation_id = state_arc
            .read()
            .await
            .conversation_id
            .or(caller_conversation_id);
        if let (Some(conversation_id), Some(store)) = (conversation_id, self.continuation_store()) {
            let active = store
                .load_active_for_conversation(conversation_id)
                .await
                .map_err(|error| {
                    AcpError::protocol(format!(
                        "failed to load active continuation for conversation {conversation_id}: {error}"
                    ))
                })?;
            if let Some(active) = active {
                if let Some(injection) = self.delegation_snapshot() {
                    injection.metrics.record_prompt_rejected_waiting(source);
                }
                return Err(AcpError::ContinuationInProgress {
                    conversation_id,
                    state: active.state,
                });
            }
        }

        if state_arc.read().await.turn_in_flight {
            return Err(AcpError::TurnInProgress);
        }
        Ok(())
    }

    /// Forwards a prompt to the connection's command channel without
    /// touching `prompt_lock`. Internal helper — both `send_prompt` and
    /// `send_prompt_linked` acquire the lock externally and then call
    /// this. Re-entering through `send_prompt` from `send_prompt_linked`
    /// while holding the lock would deadlock, hence the split.
    ///
    /// Its public caller has already completed external continuation admission
    /// under the same prompt lock. Local enqueue order is:
    /// 1. `reserve()` — only cancellable/blocking point before capture
    /// 2. state write guard; defensive re-check for an in-flight turn
    /// 3. linked + non-internal: `capture_prompt_context` while holding the
    ///    write guard (serializes write-once first text)
    /// 4. set `active_turn` / `effective_locale` / `turn_in_flight`
    /// 5. mandatory-route sync tail (no `.await`)
    /// 6. `permit.send` — no `.await` after successful capture
    #[allow(clippy::too_many_arguments)]
    async fn send_prompt_inner(
        &self,
        db: Option<&AppDatabase>,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
        user_message: Option<(String, Vec<crate::acp::UserMessageBlock>)>,
        // True only for broker-generated delegation kickoffs. They must reach
        // the child before pre-kickoff telemetry can hold the foreground prompt.
        bypass_autonomous_hold: bool,
        // When true, scan the prompt for composer-emitted profile mentions and
        // register them as mandatory routes for this connection. Must be false
        // for broker-generated child/delegation tasks so nested task text
        // cannot install routes on the child connection.
        register_mandatory_routes: bool,
        // Per-turn awaiting-reply eligibility carried onto TurnComplete.
        // Independent of route registration: chat-channel keeps routes on
        // while setting this false.
        mark_awaiting_reply: bool,
        capture: Option<PromptCaptureContext>,
    ) -> Result<(), AcpError> {
        // Reject an empty prompt BEFORE touching the concurrency gate. An empty
        // prompt produces no turn — and thus no `TurnComplete` to clear the gate
        // — so enqueuing one with the gate set would wedge the connection into
        // rejecting every future send. `map_prompt_blocks` is 1:1, so empty
        // input blocks is the only way the loop could see an empty prompt; we
        // stop it here at the single shared enqueue path.
        if blocks.is_empty() {
            return Err(AcpError::protocol(
                "prompt must contain at least one content block".to_string(),
            ));
        }
        // Precompute mandatory ids only if this is a root user prompt. Applied
        // AFTER the turn is admitted (below) so a rejected concurrent send
        // cannot overwrite the live turn's routes. Must be ready before the
        // post-capture synchronous tail (no await after capture).
        let pending_mandatory_ids = if register_mandatory_routes {
            let mut joined = String::new();
            for block in &blocks {
                if let PromptInputBlock::Text { text } = block {
                    if !joined.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(text);
                }
            }
            Some(crate::acp::delegation::types::extract_mandatory_profile_ids(&joined))
        } else {
            None
        };
        let (cmd_tx, state_arc) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (conn.cmd_tx.clone(), conn.state.clone())
        };
        // Concurrency gate: reject a second prompt while a turn is already in
        // flight on this connection. Reserve channel capacity FIRST — that
        // `reserve().await` is the only point that can block or be cancelled
        // before capture. Cancellation while waiting here writes no capture and
        // no active-turn state.
        let permit = cmd_tx
            .reserve()
            .await
            .map_err(|_| AcpError::ProcessExited)?;

        // Hold the write guard across capture so write-once first_user_text is
        // serialized with turn admission. After successful capture there is no
        // `.await` before `permit.send`.
        let mut state = state_arc.write().await;
        if state.turn_in_flight {
            return Err(AcpError::TurnInProgress);
        }

        let is_internal = matches!(state.purpose, ConnectionPurpose::InternalProbe)
            || state.purpose.is_hidden_generation();
        // Unlinked and internal-purpose sends bypass capture entirely.
        if let (Some(db), Some(conversation_id)) = (db, state.conversation_id) {
            if !is_internal {
                let captured = capture_prompt_context(
                    &db.conn,
                    conversation_id,
                    &blocks,
                    capture.as_ref(),
                    state.effective_locale,
                )
                .await
                .map_err(|error| AcpError::protocol(error.to_string()))?;
                let token = uuid::Uuid::new_v4().to_string();
                state.effective_locale = captured.locale;
                state.active_turn = Some(ActiveTurnContext {
                    token,
                    locale: captured.locale,
                });
            }
        }

        // Synchronous tail: mandatory routes then permit.send. No await.
        if let Some(ids) = pending_mandatory_ids {
            if let Some(injection) = self.delegation_snapshot() {
                injection.broker.set_mandatory_profile_routes(conn_id, ids);
            }
        }
        let turn_generation = match state.parent_turn_generation.checked_add(1) {
            Some(generation) => generation,
            None => {
                state.active_turn = None;
                return Err(AcpError::protocol("parent_turn_generation_overflow"));
            }
        };
        state.parent_turn_generation = turn_generation;
        state.active_turn_generation = Some(turn_generation);
        state.turn_in_flight = true;
        // New prompt must not inherit a retained provider turn id (e.g. after
        // DelegationSuspended) or a stale id from a prior turn.
        state.active_provider_turn_id = None;
        permit.send(ConnectionCommand::Prompt {
            blocks,
            user_message,
            mark_awaiting_reply,
            bypass_autonomous_hold,
            turn_generation,
        });
        Ok(())
    }

    #[allow(dead_code)] // Task 6 wires the continuation port to this narrow entry point.
    pub(crate) async fn suspend_for_delegation(
        &self,
        conn_id: &str,
        continuation_id: String,
        parent_turn_generation: u64,
    ) -> Result<SuspensionAck, ContinuationError> {
        let control_tx = {
            let connections = self.connections.lock().await;
            connections
                .get(conn_id)
                .ok_or_else(|| {
                    ContinuationError::SuspendDispatch(AcpError::ConnectionNotFound(conn_id.into()))
                })?
                .control_tx
                .clone()
        };
        dispatch_suspension_control(control_tx, continuation_id, parent_turn_generation).await
    }

    #[allow(
        dead_code,
        reason = "Task 7 reaches this sealed coordinator admission path"
    )]
    pub(crate) async fn admit_delegation_continuation(
        &self,
        request: ContinuationPromptRequest,
    ) -> Result<PromptAdmissionResult, ContinuationError> {
        let (prompt_lock, cmd_tx, state_arc) = {
            let connections = self.connections.lock().await;
            let connection = connections
                .get(&request.parent_connection_id)
                .ok_or(ContinuationError::ParentUnavailable)?;
            (
                connection.prompt_lock.clone(),
                connection.cmd_tx.clone(),
                connection.state.clone(),
            )
        };
        let _prompt_guard = prompt_lock.lock().await;
        let store = self
            .continuation_store()
            .ok_or(ContinuationError::StateConflict)?;
        let row = store
            .load(request.origin.continuation_id())
            .await?
            .ok_or(ContinuationError::StateConflict)?;
        if row.generation != request.continuation_generation
            || row.state != ContinuationState::Resuming
            || row.parent_connection_id.as_deref() != Some(request.parent_connection_id.as_str())
            || row.parent_conversation_id != request.parent_conversation_id
            || row.parent_session_id != request.parent_session_id
            || row.parent_turn_generation != request.suspended_turn_generation
            || request.origin.generation() != request.continuation_generation
            || row.internal_prompt_id != request.origin.internal_prompt_id()
            || row.internal_prompt_marker != request.origin.internal_prompt_marker()
            || row.wake_reason != Some(request.origin.wake_reason())
        {
            return Err(ContinuationError::StateConflict);
        }

        {
            let state = state_arc.read().await;
            let identity_matches = state.connection_id == request.parent_connection_id
                && state.conversation_id == Some(request.parent_conversation_id)
                && state.external_id.as_deref() == Some(request.parent_session_id.as_str())
                && state.last_suspended_turn_generation == Some(request.suspended_turn_generation);
            if !identity_matches {
                return Err(ContinuationError::ParentIdentityChanged);
            }
            if row.prompt_admitted_at.is_some() {
                let same_admission =
                    state
                        .last_internal_prompt_admission
                        .as_ref()
                        .is_some_and(|admission| {
                            admission.continuation_id == request.origin.continuation_id()
                                && admission.continuation_generation
                                    == request.continuation_generation
                                && admission.internal_prompt_id
                                    == request.origin.internal_prompt_id()
                        });
                return if same_admission {
                    Ok(PromptAdmissionResult::AlreadyAdmitted)
                } else {
                    Err(ContinuationError::StateConflict)
                };
            }
            if state.parent_turn_generation != request.suspended_turn_generation {
                return Err(ContinuationError::StateConflict);
            }
            if state.turn_in_flight {
                return Err(ContinuationError::PromptBusy);
            }
        }

        let prompt_text = build_continuation_prompt_text(&request.origin, &request.snapshot)
            .map_err(|_| {
                ContinuationError::PromptDelivery(AcpError::protocol(
                    "continuation prompt serialization failed",
                ))
            })?;
        let permit = cmd_tx
            .reserve()
            .await
            .map_err(|_| ContinuationError::PromptDelivery(AcpError::ProcessExited))?;

        // Crash/race fence: keep the session write guard across the durable
        // admission CAS. After CAS completes, InternalPromptAdmission,
        // turn-generation mutation, turn_in_flight, and reserved Permit::send
        // form one no-await tail. Releasing/reacquiring the write lock between
        // CAS and enqueue would open a durable-admitted / not-enqueued window
        // on crash and race with Stop/prompt admission (plan:1091-1105).
        let mut state = state_arc.write().await;
        if state.connection_id != request.parent_connection_id
            || state.conversation_id != Some(request.parent_conversation_id)
            || state.external_id.as_deref() != Some(request.parent_session_id.as_str())
            || state.last_suspended_turn_generation != Some(request.suspended_turn_generation)
            || state.parent_turn_generation != request.suspended_turn_generation
        {
            return Err(ContinuationError::StateConflict);
        }
        if state.turn_in_flight {
            return Err(ContinuationError::PromptBusy);
        }
        let turn_generation = state
            .parent_turn_generation
            .checked_add(1)
            .ok_or(ContinuationError::StateConflict)?;
        let admitted = store
            .cas_transition(
                request.origin.continuation_id(),
                request.continuation_generation,
                request.expected_version,
                ContinuationState::Resuming,
                ContinuationPatch {
                    state: ContinuationState::Resuming,
                    wake_reason: FieldPatch::Keep,
                    suspend_requested_at: FieldPatch::Keep,
                    suspended_at: FieldPatch::Keep,
                    wake_claimed_at: FieldPatch::Keep,
                    prompt_admitted_at: FieldPatch::Set(request.admitted_at),
                    finished_at: FieldPatch::Keep,
                    failure_code: FieldPatch::Keep,
                },
            )
            .await?
            .ok_or(ContinuationError::StateConflict)?;
        state.last_internal_prompt_admission = Some(InternalPromptAdmission {
            continuation_id: admitted.continuation_id,
            continuation_generation: admitted.generation,
            internal_prompt_id: request.origin.internal_prompt_id().to_string(),
            admitted_turn_generation: turn_generation,
        });
        state.parent_turn_generation = turn_generation;
        state.active_turn_generation = Some(turn_generation);
        state.turn_in_flight = true;
        // New prompt must not inherit a retained/stale provider turn id.
        state.active_provider_turn_id = None;
        permit.send(ConnectionCommand::Prompt {
            blocks: vec![PromptInputBlock::Text { text: prompt_text }],
            user_message: None,
            mark_awaiting_reply: true,
            bypass_autonomous_hold: false,
            turn_generation,
        });
        Ok(PromptAdmissionResult::Admitted)
    }

    /// Clone the connection's `prompt_lock` under a short connections-map lock.
    /// Returned Arc allows the caller to hold the prompt lock without
    /// keeping the connections map locked.
    async fn clone_prompt_lock(
        &self,
        conn_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, AcpError> {
        let connections = self.connections.lock().await;
        let conn = connections
            .get(conn_id)
            .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
        Ok(conn.prompt_lock.clone())
    }

    pub async fn send_prompt(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
        capture: Option<PromptCaptureContext>,
    ) -> Result<(), AcpError> {
        // Ordinary DB-aware UI path never drives hidden generation connections.
        {
            let state_arc = self
                .get_state(conn_id)
                .await
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            let purpose = state_arc.read().await.purpose;
            if purpose.is_hidden_generation() {
                return Err(AcpError::protocol(
                    "send_prompt rejects hidden generation purpose; use send_prompt_unlinked_internal",
                ));
            }
        }
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        let state_arc = self
            .get_state(conn_id)
            .await
            .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
        if let Some(conversation_id) = state_arc.read().await.conversation_id {
            require_writable_conversation_workflow(&db.conn, conversation_id)
                .await
                .map_err(AcpError::from)?;
        }
        self.admit_external_prompt(&state_arc, None, PromptAdmissionSource::Foreground)
            .await?;
        // Non-linked UI sends: register mandatory routes + mark attention.
        // Capture runs only when the connection is already linked (and not
        // internal); unlinked paths bypass capture by design.
        self.send_prompt_inner(Some(db), conn_id, blocks, None, false, true, true, capture)
            .await
    }

    /// Background (non-UI) prompt: keeps mandatory profile-route registration
    /// but does not mark the turn as awaiting-reply eligible. Unlinked path —
    /// no title capture (Task 4C may convert chat kickoffs to linked sends).
    pub async fn send_prompt_background(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError> {
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        let state_arc = self
            .get_state(conn_id)
            .await
            .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
        if let Some(conversation_id) = state_arc.read().await.conversation_id {
            require_writable_conversation_workflow(&db.conn, conversation_id)
                .await
                .map_err(AcpError::from)?;
        }
        self.admit_external_prompt(&state_arc, None, PromptAdmissionSource::Background)
            .await?;
        self.send_prompt_inner(None, conn_id, blocks, None, false, true, false, None)
            .await
    }

    /// Unlinked internal enqueue for probe/title/translate workers. Rejects
    /// every purpose except `InternalProbe` and hidden generation
    /// (`InternalTitle` / `InternalTranslate`), remains unlinked, and bypasses
    /// title capture. Crate-visible for Task 7's runner outside `acp::manager`.
    // No production Task 4B caller yet — Task 7 owns the first real consumer.
    // Keep crate-visible without inventing a fake call solely to silence lint.
    #[allow(dead_code)]
    pub(crate) async fn send_prompt_unlinked_internal(
        &self,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError> {
        {
            let state_arc = self
                .get_state(conn_id)
                .await
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            let purpose = state_arc.read().await.purpose;
            let admitted = matches!(purpose, ConnectionPurpose::InternalProbe)
                || purpose.is_hidden_generation();
            if !admitted {
                return Err(AcpError::protocol(format!(
                    "send_prompt_unlinked_internal requires InternalProbe or hidden generation purpose, got {purpose:?}"
                )));
            }
        }
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        // No db / capture: internal purposes always bypass title capture.
        self.send_prompt_inner(None, conn_id, blocks, None, false, false, false, None)
            .await
    }

    /// Send a prompt while ensuring a `Conversation` DB row is bound to this
    /// connection. On the first call (when `state.conversation_id` is None),
    /// either:
    /// - **Caller-supplied path** — if `conversation_id` is `Some(id)`, the
    ///   caller (the frontend) has already created the row and we adopt it via
    ///   `ConversationLinked`. Requires `folder_id` to be `Some` so the event
    ///   carries both ids without forcing subscribers to re-query the DB.
    /// - **Backend-creates path** — if `conversation_id` is `None`, we create
    ///   the row from `folder_id` (required) and emit `ConversationLinked`.
    ///   Returns an error if `folder_id` is also `None`.
    ///
    /// Subsequent calls (when state is already linked) ignore both
    /// `folder_id` and `conversation_id` and just forward the prompt.
    ///
    /// Back-compat wrapper for callers that don't supply a client message id
    /// (the delegation broker, internal/test paths). The UI send path uses
    /// [`send_prompt_linked_with_message_id`] so the sender's optimistic turn
    /// dedups against the broadcast `UserMessage` echo by exact id.
    // Plan-required public signature includes `capture`; argument count is
    // intentional and shared with the message-id variant below.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_prompt_linked(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
        capture: Option<PromptCaptureContext>,
    ) -> Result<Option<i32>, AcpError> {
        self.send_prompt_linked_with_message_id(
            db,
            conn_id,
            blocks,
            folder_id,
            conversation_id,
            delegation,
            None,
            capture,
        )
        .await
    }

    /// As [`send_prompt_linked`], plus an optional `client_message_id`: the
    /// id the sending UI assigned to its own optimistic user turn. When the
    /// user prompt is broadcast as [`AcpEvent::UserMessage`] (for cross-client
    /// viewers), this id becomes the event's `message_id`, so the sender's
    /// runtime dedups the echo against its optimistic turn by EXACT id rather
    /// than a heuristic — and an unrelated optimistic turn on another client
    /// never suppresses a different sender's prompt. `None` falls back to a
    /// connection-scoped id for non-UI senders.
    ///
    /// Awaiting-reply eligibility is `delegation.is_none()` (UI root true;
    /// delegation children false). Background automation uses
    /// [`send_prompt_linked_background`] instead.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_prompt_linked_with_message_id(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
        client_message_id: Option<String>,
        capture: Option<PromptCaptureContext>,
    ) -> Result<Option<i32>, AcpError> {
        let mark_awaiting_reply = delegation.is_none();
        self.send_prompt_linked_impl(
            db,
            conn_id,
            blocks,
            folder_id,
            conversation_id,
            delegation,
            client_message_id,
            mark_awaiting_reply,
            capture,
            PromptAdmissionSource::LinkedForeground,
        )
        .await
    }

    /// Linked prompt for automation / non-UI producers: root mandatory-route
    /// registration is preserved, but the turn is not awaiting-reply eligible.
    pub async fn send_prompt_linked_background(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        capture: Option<PromptCaptureContext>,
    ) -> Result<Option<i32>, AcpError> {
        self.send_prompt_linked_impl(
            db,
            conn_id,
            blocks,
            folder_id,
            conversation_id,
            None,
            None,
            false,
            capture,
            PromptAdmissionSource::LinkedBackground,
        )
        .await
    }

    /// Shared linked-prompt implementation. `mark_awaiting_reply` is independent
    /// of mandatory profile-route registration (`delegation.is_none()`).
    /// Linked first-send and already-linked paths both call the shared
    /// admission hook (`send_prompt_inner`) exactly once.
    #[allow(clippy::too_many_arguments)]
    async fn send_prompt_linked_impl(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        mut blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
        client_message_id: Option<String>,
        mark_awaiting_reply: bool,
        capture: Option<PromptCaptureContext>,
        admission_source: PromptAdmissionSource,
    ) -> Result<Option<i32>, AcpError> {
        // Reject an empty prompt up front, BEFORE any side effects: linking /
        // creating the conversation row, flipping it to InProgress, or emitting
        // events. An empty prompt is never accepted, so it must not mutate
        // persisted state (create a row, or flip an existing one — which would
        // then be rolled back to Cancelled). `send_prompt_inner` keeps a
        // defensive copy of this guard for the non-linked `send_prompt` path.
        if blocks.is_empty() {
            return Err(AcpError::protocol(
                "prompt must contain at least one content block".to_string(),
            ));
        }
        // Caller-supplied conversation_id requires folder_id (we include it in
        // the emitted ConversationLinked event so subscribers don't have to
        // re-query the DB). Validate before touching any state.
        if conversation_id.is_some() && folder_id.is_none() {
            return Err(AcpError::protocol(
                "conversation_id provided without folder_id".to_string(),
            ));
        }
        // Prebound gen-1 children pass both `conversation_id` (row already
        // created for the durable fence) and `delegation` (mode flag). The link
        // was already written at create time; here it only preserves child
        // prompt semantics (no user-message / awaiting-reply / mandatory routes)
        // and parent ids on ConversationLinked. Never re-create or re-apply the
        // FK linkage when both are present.

        // Acquire the per-connection prompt lock for the entire link-check
        // + DB write + emit + cmd_tx.send sequence. Two concurrent prompts
        // (multiple browser tabs of the same conversation; chat-channel
        // racing the UI) are now strictly serialized — the second waiter
        // observes `already_linked == true` after the first commits, so
        // it can't double-create a conversation row.
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _prompt_guard = prompt_lock.lock_owned().await;

        // Snapshot what we need from the connection map under one short lock.
        // The conversation-linked check happens INSIDE the prompt lock so
        // any racing send sees a consistent post-link state.
        let (state_arc, emitter, agent_type, already_linked) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            let already = {
                let s = conn.state.read().await;
                s.conversation_id.is_some()
            };
            (
                conn.state.clone(),
                conn.emitter.clone(),
                conn.agent_type,
                already,
            )
        };

        // Any linked conversation with durable v2 identity is archived. A new
        // prebound child has no run binding yet and therefore remains eligible
        // for its first prompt.
        let effective_conversation_id = conversation_id.or({
            let state = state_arc.read().await;
            state.conversation_id
        });
        if let Some(conversation_id) = effective_conversation_id {
            require_writable_conversation_workflow(&db.conn, conversation_id)
                .await
                .map_err(AcpError::from)?;
        }

        self.admit_external_prompt(&state_arc, conversation_id, admission_source)
            .await?;

        // Re-hydrate uploaded image attachments (web / remote-workspace mode
        // sends empty-payload marker blocks with a `file://` uri into the
        // uploads root; see `prompt_hydration`). Deliberately placed AFTER
        // admission — the connection-exists check (`clone_prompt_lock` above)
        // and the busy reject — so garbage or concurrent prompts never
        // trigger file reads, and the prompt lock we hold serializes
        // hydration per connection (a natural concurrency bound). Still
        // BEFORE any side effect (linking / row creation / status flip) and
        // before the `user_blocks_from_prompt` projection below, so a failure
        // aborts cleanly and the viewer broadcast, the sender echo, and the
        // agent all see the full bytes.
        crate::acp::prompt_hydration::hydrate_prompt_blocks(
            &mut blocks,
            &crate::paths::codeg_uploads_root(),
        )
        .await?;

        if !already_linked {
            match (conversation_id, folder_id) {
                // Branch A: caller already owns a row — adopt it. No DB write.
                // Prebound delegation children still carry the link for event
                // parent ids + child prompt policy (`delegation.is_some()`).
                (Some(caller_conv_id), Some(caller_folder_id)) => {
                    self.bind_shared_conversation_if_present(conn_id, caller_conv_id)
                        .await?;
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::ConversationLinked {
                            conversation_id: caller_conv_id,
                            folder_id: caller_folder_id,
                            parent_conversation_id: delegation
                                .as_ref()
                                .map(|d| d.parent_conversation_id),
                            parent_tool_use_id: delegation
                                .as_ref()
                                .map(|d| d.parent_tool_use_id.clone()),
                        },
                    )
                    .await;
                }
                // Function-entry guard rejects this combination.
                (Some(_), None) => unreachable!(
                    "conversation_id without folder_id should have been rejected at function entry"
                ),
                // Branch B: backend creates the row from caller-supplied
                // folder_id. Phase 3c-1 made folder_id required here — every
                // production caller that reaches this branch passes one, and
                // silent fallback to working_dir-based find-or-create masked
                // contract violations.
                (None, Some(folder_id)) => {
                    // Snapshot the delegation link before move-into-create: we
                    // still need the parent ids for the ConversationLinked
                    // event payload.
                    let parent_conversation_id_for_event =
                        delegation.as_ref().map(|d| d.parent_conversation_id);
                    let parent_tool_use_id_for_event =
                        delegation.as_ref().map(|d| d.parent_tool_use_id.clone());
                    // Seed a delegation child's title from the task prompt so the
                    // sidebar shows a meaningful label immediately. `list_children`
                    // returns the raw DB title, so a child born with NULL reads
                    // "Untitled" until the first detail load backfills it. Roots
                    // (no delegation) keep `None` and follow the existing backfill.
                    let seed_title = if delegation.is_some() {
                        delegation_child_title_seed(&blocks)
                    } else {
                        None
                    };
                    let row = conversation_service::create_with_delegation(
                        &db.conn,
                        folder_id,
                        agent_type,
                        seed_title,
                        None,
                        delegation.clone(),
                    )
                    .await
                    .map_err(|e| AcpError::protocol(e.to_string()))?;
                    self.bind_shared_conversation_if_present(conn_id, row.id)
                        .await?;
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::ConversationLinked {
                            conversation_id: row.id,
                            folder_id,
                            parent_conversation_id: parent_conversation_id_for_event,
                            parent_tool_use_id: parent_tool_use_id_for_event,
                        },
                    )
                    .await;
                    // Sidebar sync: a conversation born here (agent path — a
                    // prompt sent without a pre-created row, not the create
                    // button) must reach every client immediately via the global
                    // `conversation://changed` channel. Roots land in the sidebar
                    // list; delegation children (parent set) are routed into their
                    // parent's expanded sub-session subtree and bump its chevron.
                    // Both carry `external_id: null` here (no session yet) — the
                    // external_id write below re-broadcasts the full summary.
                    crate::commands::conversations::emit_conversation_upsert(
                        &emitter, &db.conn, row.id,
                    )
                    .await;
                    // A new delegation child changes its parent's child_count
                    // (0 → >0 makes the parent's expand chevron appear). Re-emit
                    // the parent so every client converges its count from the
                    // authoritative DB aggregate rather than a drift-prone
                    // per-client increment. The parent may itself be a root or a
                    // nested child — the upsert routes correctly either way by its
                    // own parent_id.
                    if let Some(parent_id) = parent_conversation_id_for_event {
                        crate::commands::conversations::emit_conversation_upsert(
                            &emitter, &db.conn, parent_id,
                        )
                        .await;
                    }
                }
                (None, None) => {
                    return Err(AcpError::protocol(
                        "folder_id required for new conversation row".to_string(),
                    ));
                }
            }

            // UI new-conversation path: SessionStarted applied state.external_id
            // back during acp_connect, but conversation_id was None then so the
            // lifecycle subscriber's SessionStarted handler skipped the DB write.
            // Now that we just linked the row in the same prompt_lock critical
            // section, snapshot external_id and persist it synchronously — no
            // dependence on broadcaster eventual consistency. The chat_channel
            // reverse-order path (link before SessionStarted) is unaffected and
            // continues to be handled by the lifecycle subscriber.
            let (cid_opt, eid_opt) = {
                let s = state_arc.read().await;
                (s.conversation_id, s.external_id.clone())
            };
            if let (Some(cid), Some(eid)) = (cid_opt, eid_opt) {
                conversation_service::update_external_id(&db.conn, cid, eid)
                    .await
                    .map_err(|e| AcpError::protocol(e.to_string()))?;
                // SessionStarted arrived BEFORE this link, so the lifecycle
                // subscriber skipped its broadcast (no conversation_id then).
                // Now that external_id is persisted, converge every client's
                // sidebar with the complete summary — this also corrects a
                // Branch B upsert above that necessarily carried
                // `external_id: null`. Root-only via the helper.
                crate::commands::conversations::emit_conversation_upsert(&emitter, &db.conn, cid)
                    .await;
            } else if cid_opt.is_some() {
                tracing::info!(
                    "[manager] send_prompt_linked: conversation linked but \
                     external_id not yet on state (conn={conn_id}); lifecycle \
                     subscriber will catch up when SessionStarted arrives"
                );
            }
        }

        // Centralized status transition: every prompt send flips the
        // conversation row to InProgress. This MUST happen on every call
        // (including the already-linked path) so that a follow-up turn whose
        // row is currently `pending_review` correctly transitions back. The
        // DB write precedes the event emit so any subscriber observing
        // `ConversationStatusChanged` can assume the row is consistent.
        // `update_status_with_patch` is a single UPDATE — idempotent with
        // respect to the same status value, so re-writing `InProgress` is a
        // benign no-op on the row (touches `updated_at` only) and returns the
        // patch for the global state broadcast.
        let conversation_id_for_status = state_arc.read().await.conversation_id;
        if let Some(cid) = conversation_id_for_status {
            let patch = conversation_service::update_status_with_patch(
                &db.conn,
                cid,
                ConversationStatus::InProgress,
            )
            .await
            .map_err(|e| AcpError::protocol(e.to_string()))?;
            emit_with_state(
                &state_arc,
                &emitter,
                AcpEvent::ConversationStatusChanged {
                    conversation_id: cid,
                    status: ConversationStatus::InProgress,
                },
            )
            .await;
            crate::commands::conversations::emit_conversation_state(&emitter, patch);
        }

        // Capture a bounded preview of the user's message BEFORE `blocks` is
        // moved into `send_prompt_inner`. Only on the genuine UI path
        // (`delegation.is_none()`): delegation / sub-agent prompts are not user
        // messages. Emitted after the send succeeds (below) so a prompt that
        // never reached the agent produces no "user message" notification.
        let user_prompt_preview = if delegation.is_none() {
            user_prompt_text_preview(&blocks)
        } else {
            None
        };

        // Project the user's prompt blocks for the cross-client viewer
        // broadcast BEFORE `send_prompt_inner` consumes `blocks`, and hand the
        // payload to the connection loop (via `ConnectionCommand::Prompt`) so it
        // emits the `UserMessage` event in-order, right before the agent
        // request — guaranteeing its seq precedes the turn's agent events and
        // that it only fires for a prompt actually processed (a failed enqueue
        // delivers no command, so nothing strands a `pending_user_message`).
        // Gated on `delegation.is_none()` (children surface kickoff text
        // separately) and a bound conversation row (a sidebar-visible turn). The
        // `message_id` prefers the sender's client-supplied id (exact echo
        // dedup), falling back to a connection-scoped id for non-UI senders.
        let user_message: Option<(String, Vec<crate::acp::UserMessageBlock>)> =
            if delegation.is_none() && conversation_id_for_status.is_some() {
                let user_blocks = crate::acp::user_blocks_from_prompt(&blocks);
                if user_blocks.is_empty() {
                    None
                } else {
                    // A client-supplied id in the parsers' turn-id namespace
                    // (`turn-<digits>`, which every parser assigns) would collide
                    // with a persisted transcript turn id and break id-keyed dedup
                    // — a colliding id can suppress or hide a prompt. The id is
                    // untrusted (the web/Tauri prompt API accepts it verbatim), so
                    // reject that shape and fall back to a connection-scoped id;
                    // legitimate UI senders use `optimistic-<uuid>`.
                    let message_id = match client_message_id {
                        Some(id) if !is_reserved_turn_id(&id) => id,
                        _ => format!("user-{}-{}", conn_id, state_arc.read().await.event_seq),
                    };
                    Some((message_id, user_blocks))
                }
            } else {
                None
            };
        let bypass_autonomous_hold = delegation.is_some();

        // We hold `_prompt_guard` here, so call the lock-free inner helper —
        // re-entering `send_prompt` would try to acquire the same mutex and
        // deadlock. The helper reserves channel capacity FIRST; only after a
        // successful reserve (and successful capture when applicable) does it
        // set active_turn / turn_in_flight, with no await before the infallible
        // `permit.send`. Failures at reserve or at capture therefore happen
        // BEFORE the gate is set — there is nothing turn-related to roll back.
        // On those failures we still flip the row to `Cancelled` so the UI
        // doesn't strand on `in_progress`: no `TurnComplete` will ever arrive
        // for a prompt that never reached the agent, so without this the
        // lifecycle subscriber's PendingReview write also never fires and the
        // row would be stuck until a follow-up `send_prompt_linked` re-flipped it.
        // Only root (non-delegation) prompts install mandatory profile routes.
        // Child tasks go through the same helper but must not scan task text.
        // Awaiting-reply eligibility is a separate policy bit.
        match self
            .send_prompt_inner(
                Some(db),
                conn_id,
                blocks,
                user_message,
                bypass_autonomous_hold,
                delegation.is_none(),
                mark_awaiting_reply,
                capture,
            )
            .await
        {
            Ok(()) => {
                // The prompt reached the agent: surface it to the chat-channel
                // "user message" event feed. Notification-only — never gates the
                // send result.
                if let Some(text_preview) = user_prompt_preview {
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::UserPromptSent { text_preview },
                    )
                    .await;
                }
                Ok(conversation_id_for_status)
            }
            Err(send_err) => {
                if let Some(cid) = conversation_id_for_status {
                    match conversation_service::update_status_with_patch(
                        &db.conn,
                        cid,
                        ConversationStatus::Cancelled,
                    )
                    .await
                    {
                        Ok(patch) => {
                            emit_with_state(
                                &state_arc,
                                &emitter,
                                AcpEvent::ConversationStatusChanged {
                                    conversation_id: cid,
                                    status: ConversationStatus::Cancelled,
                                },
                            )
                            .await;
                            crate::commands::conversations::emit_conversation_state(
                                &emitter, patch,
                            );
                        }
                        Err(rollback_err) => {
                            // Best-effort: original send error is the load-bearing
                            // signal; rollback failure is logged but not surfaced.
                            tracing::error!(
                                "[ACP][ERROR] failed to mark conversation {cid} cancelled \
                                 after send failure (original={send_err}): {rollback_err}"
                            );
                        }
                    }
                }
                Err(send_err)
            }
        }
    }

    pub async fn set_mode(&self, conn_id: &str, mode_id: String) -> Result<(), AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::SetMode { mode_id })
            .await
            .map_err(|_| AcpError::ProcessExited)
    }

    pub async fn set_config_option(
        &self,
        conn_id: &str,
        config_id: String,
        value_id: String,
    ) -> Result<(), AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::SetConfigOption {
                config_id,
                value_id,
            })
            .await
            .map_err(|_| AcpError::ProcessExited)
    }

    /// Pause or clear the session's active goal via the connection loop
    /// (codex-acp #293; the provider-neutral `_session/goal` since codex 1.2 /
    /// claude 0.66). Looked up by connectionId; the loop sources the sessionId
    /// from the live session, so callers only supply the action.
    ///
    /// STOPS THE RUNNING TURN TOO, for adapters whose control request travels
    /// out of band (see `registry::goal_control_is_out_of_band`). Without that,
    /// the button is a lie: codex's pause/clear are pure app-server metadata
    /// that only take hold at the next idle point, so the user clicks 暂停 and
    /// watches the agent keep working — the whole reason the goal card's
    /// controls were once ripped out.
    ///
    /// Two conditions guard it, and neither is "is a turn running?":
    /// * the goal must have been ACTIVE when the user clicked
    ///   (`SessionState.goal_active`, read before the request goes out). An
    ///   active goal is the thing driving the work, so stopping the work is
    ///   what the click means. A PAUSED goal drives nothing — clearing one is
    ///   housekeeping, and must never abort a turn the user started themselves
    ///   in the meantime.
    /// * the control must have LANDED. The loop reports that back over a
    ///   oneshot; a rejected request (or a closed channel) leaves the turn
    ///   alone, so a goal that is still `active` never gets its work killed
    ///   only to have codex resume it at the next idle point. The round-trip
    ///   also makes the interrupt strictly second: the goal is already
    ///   non-active when the abort lands, so nothing auto-continues on the way
    ///   out.
    ///
    /// Liveness deliberately does NOT gate it. `ConnectionStatus::Prompting`
    /// (and `turn_in_flight` with it) tracks turns CODEG started, and a goal
    /// loop's continuations are started by codex itself — detached turns no host
    /// request owns — so gating on them would skip the interrupt in exactly the
    /// case that motivated this. Any post-hoc read is unsound anyway: by the
    /// time the manager acts on it the turn it described may have ended. With
    /// the goal known active, the interrupt means precisely "press Stop on the
    /// user's behalf": `cancel()`'s row write is CAS'd from `InProgress`, a
    /// `session/cancel` with nothing running is a no-op, and its permission
    /// drain / delegation cascade are the semantics the Stop button already has
    /// (including its own turn-boundary race, which is not made worse here).
    ///
    /// Awaiting the round-trip is deliberate and cheap — an out-of-band control
    /// is a plain RPC that returns without waiting for the turn. It is NOT
    /// cancellation-shielded (unlike `submit_feedback_native`): a caller that
    /// disappears mid-await simply doesn't get the follow-up interrupt, which is
    /// the pre-existing behavior, never worse. The control itself is owned by
    /// the loop from the moment it is enqueued.
    pub async fn goal_control(
        &self,
        db: &DatabaseConnection,
        conn_id: &str,
        action: GoalControlAction,
    ) -> Result<(), AcpError> {
        let (cmd_tx, agent_type, state_arc) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (conn.cmd_tx.clone(), conn.agent_type, conn.state.clone())
        };
        // Read the goal state as it was when the user clicked — reading it after
        // the round-trip would see the transition we just asked for.
        let interrupts = crate::acp::registry::goal_control_is_out_of_band(agent_type)
            && state_arc.read().await.goal_active;
        // Only ask for an answer when it would change what we do next.
        let (reply_tx, reply_rx) = if interrupts {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        cmd_tx
            .send(ConnectionCommand::GoalControl {
                action,
                reply: reply_tx,
            })
            .await
            .map_err(|_| AcpError::ProcessExited)?;

        let Some(reply_rx) = reply_rx else {
            return Ok(());
        };
        if !matches!(reply_rx.await, Ok(true)) {
            return Ok(());
        }
        tracing::info!(
            "[ACP] goal {:?} landed; interrupting so the work stops now connection={}",
            action,
            conn_id
        );
        self.cancel(db, conn_id).await
    }

    pub async fn cancel(&self, db: &DatabaseConnection, conn_id: &str) -> Result<(), AcpError> {
        self.cancel_with_admission(db, conn_id, None)
            .await
            .map_err(SharedControlAdmissionError::into_error)
    }

    async fn cancel_with_admission(
        &self,
        db: &DatabaseConnection,
        conn_id: &str,
        shared_claim: Option<&crate::acp::shared_session::SharedStopClaim>,
    ) -> Result<(), SharedControlAdmissionError> {
        let (prompt_lock, control_tx, state_arc, emitter) = {
            let connections = self.connections.lock().await;
            let conn = connections.get(conn_id).ok_or_else(|| {
                SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::ConnectionNotFound(
                    conn_id.into(),
                ))
            })?;
            (
                conn.prompt_lock.clone(),
                conn.control_tx.clone(),
                conn.state.clone(),
                conn.emitter.clone(),
            )
        };
        let _prompt_guard = prompt_lock.lock_owned().await;
        let conversation_id = state_arc.read().await.conversation_id;
        let cleanup_error = match (
            conversation_id,
            self.delegation_snapshot()
                .and_then(|injection| injection.continuation_coordinator.upgrade()),
        ) {
            (Some(conversation_id), Some(coordinator)) => coordinator
                .handle_parent_stop(conn_id, conversation_id)
                .await
                .err(),
            _ => None,
        };
        let cancel_permit = control_tx.reserve().await.map_err(|_| {
            SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::ProcessExited)
        })?;
        if let Some(claim) = shared_claim {
            self.shared_session_broker
                .validate_stop_claim(claim)
                .await
                .map_err(|error| {
                    SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::Shared(error))
                })?;
        }
        // Reserve only after cleanup so receiver closure during that await is a
        // definite failure. Final exact-turn validation then immediately
        // precedes the synchronous admission, with no await in between.
        cancel_permit.send(ConnectionControl::Cancel);

        // Eagerly flip the row to `Cancelled` so the sidebar/tabs leave the
        // "running" state immediately. The agent typically replies with
        // `TurnComplete{cancelled}` which the lifecycle subscriber ignores,
        // and stays connected (so `handle_terminal_event` doesn't fire either)
        // — without this write the row would strand on `InProgress`.
        // CAS-guarded so we don't overwrite a `PendingReview`/`Completed`
        // status if the turn happened to end just before the user clicked.
        if let Some(cid) = conversation_id {
            match conversation_service::update_status_if_with_patch(
                db,
                cid,
                ConversationStatus::InProgress,
                ConversationStatus::Cancelled,
            )
            .await
            {
                Ok(Some(patch)) => {
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::ConversationStatusChanged {
                            conversation_id: cid,
                            status: ConversationStatus::Cancelled,
                        },
                    )
                    .await;
                    crate::commands::conversations::emit_conversation_state(&emitter, patch);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(
                        "[ACP][ERROR] failed to mark conversation {cid} cancelled \
                         on user cancel (conn={conn_id}): {e}"
                    );
                }
            }
        }

        if let Some(error) = cleanup_error {
            return Err(SharedControlAdmissionError::MayHaveBeenAdmitted(
                AcpError::protocol(format!("continuation stop persistence failed: {error}")),
            ));
        }
        Ok(())
    }

    pub async fn respond_permission(
        &self,
        conn_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<(), AcpError> {
        self.respond_permission_with_admission(conn_id, request_id, option_id)
            .await
            .map_err(SharedControlAdmissionError::into_error)
    }

    async fn respond_permission_with_admission(
        &self,
        conn_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<(), SharedControlAdmissionError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections.get(conn_id).ok_or_else(|| {
                SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::ConnectionNotFound(
                    conn_id.into(),
                ))
            })?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::RespondPermission {
                request_id: request_id.into(),
                option_id: option_id.into(),
            })
            .await
            .map_err(|_| {
                SharedControlAdmissionError::DefinitelyNotAdmitted(AcpError::ProcessExited)
            })
    }

    /// Fork the agent's session and persist the resulting two-row layout in
    /// one backend call: the current row gets re-pointed at S2 (the forked
    /// session) with a `[Fork]` title prefix, and a freshly-created sibling
    /// row preserves the pre-fork (S1) history at `PendingReview`. Frontend
    /// no longer touches `external_id` or fork-related row creation —
    /// the wire `ForkResultInfo` carries `sibling_conversation_id` for tab/UI
    /// reconciliation.
    pub async fn fork_session(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        // Caller-supplied linkage for a connection that resumed a historical
        // conversation but hasn't sent a prompt through it yet. Such a
        // connection is bound to its session via `session_id` (resume) but its
        // conversation ROW isn't linked until the first prompt fires
        // `ConversationLinked` (see `send_prompt_linked`). A fork-send forks
        // BEFORE that first prompt, so without adopting the row here the fork
        // would reject as unlinked. Ignored when the connection is already
        // linked (the common new-conversation-then-fork path), and both must be
        // `Some` to link (a `conversation_id` needs its `folder_id`, mirroring
        // `send_prompt_linked`'s Branch A contract).
        link_conversation_id: Option<i32>,
        link_folder_id: Option<i32>,
    ) -> Result<ForkResultInfo, AcpError> {
        let _permit = self.admission.admit()?;
        let (state_arc, cmd_tx, emitter) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (
                conn.state.clone(),
                conn.cmd_tx.clone(),
                conn.emitter.clone(),
            )
        };

        // Serialize the fork against concurrent prompts on this connection via
        // the same per-connection `prompt_lock` that `send_prompt`/
        // `send_prompt_linked` hold. A fork re-points the live session, so a
        // prompt must never start a turn underneath it. The lock is held for the
        // WHOLE operation (gate check → enqueue → protocol round-trip →
        // persistence); because the LOCK (not a flag) provides the exclusion,
        // the fork never SETS `turn_in_flight`, so there is no flag a dropped
        // future could strand and no window where a prompt's side effects (row
        // create / InProgress) commit only to lose the gate to a fork and roll
        // back to `Cancelled`.
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let prompt_guard = prompt_lock.lock_owned().await;

        // Link the conversation row on demand, under the prompt lock so it
        // can't race a concurrent first prompt. A conversation opened from
        // history resumes via `session_id`, but its row is bound to the
        // connection only when the first prompt fires `ConversationLinked`;
        // fork-send forks first, so adopt the caller-supplied row here (the
        // same existing-row path as `send_prompt_linked` Branch A). No-op when
        // already linked, or when the caller didn't supply both ids (the check
        // below then rejects, unchanged).
        if state_arc.read().await.conversation_id.is_none() {
            if let (Some(cid), Some(fid)) = (link_conversation_id, link_folder_id) {
                emit_with_state(
                    &state_arc,
                    &emitter,
                    AcpEvent::ConversationLinked {
                        conversation_id: cid,
                        folder_id: fid,
                        parent_conversation_id: None,
                        parent_tool_use_id: None,
                    },
                )
                .await;
            }
        }

        // Fork requires a linked conversation row — the sibling we're about
        // to create exists to preserve THIS row's pre-fork history. Without
        // a current row, fork would either orphan S1 or violate the
        // no-pre-prompt-row invariant.
        let conversation_id = state_arc.read().await.conversation_id.ok_or_else(|| {
            AcpError::protocol("fork_session requires a linked conversation row".to_string())
        })?;

        // Reject if a turn is already in flight. `prompt_lock` is FREE between a
        // prompt's enqueue and its `TurnComplete` (it is released the moment the
        // command is queued), so the lock alone can't catch a turn the loop is
        // mid-processing — only the gate can. We CHECK the gate (bouncing with
        // `TurnInProgress` so the caller re-queues) under the prompt lock, where
        // the loop is the only writer and the value can't flip to true
        // underneath us, but we never SET it: not setting the gate is precisely
        // why a dropped fork can't wedge the connection.
        if state_arc.read().await.turn_in_flight {
            return Err(AcpError::TurnInProgress);
        }

        // CANCELLATION SHIELD. Up to here the fork is side-effect-free: if THIS
        // future is dropped now (e.g. an HTTP client disconnecting mid-fork), the
        // `prompt_guard` drops and nothing happened. But the instant we enqueue
        // `ConnectionCommand::Fork`, the connection loop executes the agent
        // `session/fork` and re-points the live session to S2 REGARDLESS of
        // whether this caller survives — `handle_fork_or_exit` ignores a dead
        // reply channel and still attaches + emits `SessionStarted{S2}`. So the
        // DB persistence (sibling row preserving S1 + `[Fork]` title) must NOT be
        // tied to this future; otherwise a dropped caller would strand the live
        // session on S2 with the pre-fork S1 history orphaned and no sibling row.
        // We run enqueue → reply → persist → emit in a DETACHED task that OWNS
        // the `prompt_guard`: dropping this future no longer aborts the
        // persistence — it runs to completion and only then releases the lock.
        // We await the task's handle purely to hand the result back to a live
        // caller; the result is harmlessly discarded if the caller is gone.
        let db_conn = db.conn.clone();
        let conn_id_for_task = conn_id.to_string();
        let handle = tokio::spawn(async move {
            // Holding the owned guard for the whole task is what shields the
            // persistence from caller cancellation.
            let _prompt_guard = prompt_guard;
            let outcome: Result<ForkResultInfo, AcpError> = async {
                // Protocol-only round trip — no DB writes inside the loop.
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                cmd_tx
                    .send(ConnectionCommand::Fork { reply: reply_tx })
                    .await
                    .map_err(|_| AcpError::ProcessExited)?;
                let protocol_result = reply_rx
                    .await
                    .map_err(|_| AcpError::protocol("Fork reply channel closed".to_string()))??;

                let forked_session_id = protocol_result.forked_session_id;
                let original_session_id = protocol_result.original_session_id;

                let sibling_id = Self::persist_fork_outcome(
                    &db_conn,
                    conversation_id,
                    forked_session_id.clone(),
                    original_session_id.clone(),
                )
                .await?;

                // Fork mutates the sidebar in two ways the rest of the system
                // never sees otherwise: the current row's title (`[Fork] …`) and
                // external_id (→ S2) changed, and a brand-new sibling row now
                // exists (external_id S1, PendingReview). Broadcast both on
                // `conversation://changed` so every other client converges in
                // real time instead of waiting for a manual refresh. Both rows
                // are roots; the helper still guards `parent_id` internally.
                crate::commands::conversations::emit_conversation_upsert(
                    &emitter,
                    &db_conn,
                    conversation_id,
                )
                .await;
                crate::commands::conversations::emit_conversation_upsert(
                    &emitter, &db_conn, sibling_id,
                )
                .await;

                Ok(ForkResultInfo {
                    forked_session_id,
                    original_session_id,
                    sibling_conversation_id: sibling_id,
                })
            }
            .await;
            // Surface failures even when the caller is gone (the detached task's
            // Result would otherwise be dropped silently).
            if let Err(ref e) = outcome {
                tracing::error!(
                    "[ACP][ERROR] fork persistence failed (conn={conn_id_for_task}): {e}"
                );
            }
            outcome
        });

        match handle.await {
            Ok(result) => result,
            Err(join_err) => {
                tracing::error!(
                    "[ACP][ERROR] fork persistence task did not complete (conn={conn_id}): \
                     {join_err}"
                );
                Err(AcpError::protocol(format!(
                    "fork persistence task did not complete: {join_err}"
                )))
            }
        }
    }

    /// Persist the two-row fork layout: re-point the current row at S2 with a
    /// `[Fork]` title prefix, and INSERT a sibling row preserving the pre-fork
    /// (S1) history at `PendingReview`. Returns the sibling row id.
    ///
    /// Factored out of [`fork_session`] so the cancellation-shielded task body
    /// stays readable. Everything runs in one transaction so a mid-sequence
    /// failure can't leak: if INSERT fails we don't re-point the current row at
    /// S2 (it stays bound to S1; the lifecycle subscriber's eventual
    /// `SessionStarted{S2}` write would still occur, but the user-visible row
    /// layout stays consistent until then). If the current-row UPDATE fails we
    /// never insert a sibling — no orphan.
    ///
    /// The transaction is deliberately WRITE-FIRST. SeaORM's SQLite backend
    /// always opens a transaction with a plain (deferred) `BEGIN` — access mode
    /// isn't configurable per-transaction for SQLite — so a transaction that
    /// LED with the `SELECT` of the current row would take a read snapshot
    /// first; if any other pooled connection commits a write before this
    /// transaction's later UPDATE (routine under this app's concurrent
    /// multi-conversation load), SQLite can't promote that now-stale snapshot
    /// to a writer and fails the whole transaction with `SQLITE_BUSY_SNAPSHOT`
    /// (code 517) — surfaced to the user as "database is locked" even though
    /// nothing was actually deadlocked, and NOT retried by `busy_timeout` (that
    /// only covers ordinary lock contention). So the FIRST statement is a write
    /// (bump `updated_at`, which we want anyway and which claims the writer
    /// lock), and only THEN do we read the row. Reading under the held write
    /// lock has a second payoff: because no other writer can interpose between
    /// the read and the UPDATE/INSERT, the title/metadata we derive can't be a
    /// stale snapshot that a concurrent rename/soft-delete already superseded —
    /// the fork observes the latest committed row and never clobbers a newer
    /// title or forks from stale routing.
    ///
    /// The claim write is filtered on `deleted_at IS NULL` (the codebase-wide
    /// "live row" predicate). Forking a soft-deleted conversation would
    /// otherwise resurrect it as a fresh, visible sibling (`deleted_at = None`),
    /// so a claim that matches no LIVE row is treated as not-found and the whole
    /// fork aborts without writing anything.
    async fn persist_fork_outcome(
        db_conn: &DatabaseConnection,
        conversation_id: i32,
        forked_session_id: String,
        original_session_id: String,
    ) -> Result<i32, AcpError> {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, QueryFilter};

        db_conn
            .transaction::<_, i32, sea_orm::DbErr>(|txn| {
                Box::pin(async move {
                    let now = chrono::Utc::now();

                    // WRITE FIRST — see the fn doc. Bumping `updated_at` is the
                    // transaction's opening statement so SQLite acquires the
                    // writer lock immediately instead of taking a deferred read
                    // snapshot it would later have to (and might fail to)
                    // promote. Filtered on `deleted_at IS NULL` so a soft-deleted
                    // conversation can't be forked back into a live sibling;
                    // `rows_affected == 0` means the row is gone OR deleted.
                    let claimed = conversation::Entity::update_many()
                        .col_expr(conversation::Column::UpdatedAt, Expr::value(now))
                        .filter(conversation::Column::Id.eq(conversation_id))
                        .filter(conversation::Column::DeletedAt.is_null())
                        .exec(txn)
                        .await?;
                    if claimed.rows_affected == 0 {
                        return Err(sea_orm::DbErr::Custom(format!(
                            "conversation {conversation_id} not found or already deleted"
                        )));
                    }

                    // Read UNDER the write lock: this SELECT sees the latest
                    // committed state and no other writer can interpose before
                    // this transaction finishes, so the derived title/metadata
                    // below can't be superseded by a concurrent rename/delete.
                    // The successful live-row claim above guarantees this returns
                    // Some; the `ok_or_else` is defensive.
                    let current = conversation::Entity::find_by_id(conversation_id)
                        .one(txn)
                        .await?
                        .ok_or_else(|| {
                            sea_orm::DbErr::Custom(format!(
                                "conversation {conversation_id} not found"
                            ))
                        })?;

                    // Strip any `[Fork]` prefix tolerantly (matches the prior
                    // frontend regex `/^\[Fork]\s*/g` behaviour for both spaced
                    // and no-space variants). None title stays None.
                    let clean_title: Option<String> = current.title.as_ref().map(|t| {
                        t.strip_prefix("[Fork]")
                            .map(str::trim_start)
                            .unwrap_or(t.as_str())
                            .to_string()
                    });

                    let folder_id = current.folder_id;
                    let agent_type_str = current.agent_type.clone();
                    let git_branch = current.git_branch.clone();
                    // Capture before `into()` so the live row retains its guard
                    // and the historical sibling copies the same finalized flag.
                    // Fork never inserts a sibling auto-title job.
                    let auto_title_finalized = current.auto_title_finalized;
                    // The sibling keeps the original's sidebar routing (a forked
                    // chat conversation must stay in the Chat group). `Delegate`
                    // is unreachable here — children are never forked from the
                    // UI — but the invariant `delegate ⟺ parent_id set` wins
                    // over inheritance, so it degrades to `Regular`.
                    let sibling_kind = match current.kind {
                        ConversationKind::Delegate => ConversationKind::Regular,
                        ref kind => kind.clone(),
                    };

                    // UPDATE current row → S2. Writing external_id explicitly
                    // here closes the race against `refreshConversations()`
                    // after this fn returns; the lifecycle subscriber's later
                    // SessionStarted{S2} write is an idempotent no-op.
                    let mut active: conversation::ActiveModel = current.into();
                    if let Some(ref clean) = clean_title {
                        active.title = Set(Some(format!("[Fork] {clean}")));
                    }
                    active.external_id = Set(Some(forked_session_id));
                    active.updated_at = Set(now);
                    // Model→ActiveModel conversion keeps auto_title_finalized.
                    active.update(txn).await?;

                    // INSERT sibling row preserving pre-fork (S1) history.
                    // PendingReview because no live agent is attached to S1.
                    let sibling = conversation::ActiveModel {
                        id: NotSet,
                        folder_id: Set(folder_id),
                        title: Set(clean_title),
                        title_locked: Set(false),
                        auto_title_finalized: Set(auto_title_finalized),
                        agent_type: Set(agent_type_str),
                        status: Set(ConversationStatus::PendingReview),
                        kind: Set(sibling_kind),
                        model: Set(None),
                        git_branch: Set(git_branch),
                        external_id: Set(Some(original_session_id)),
                        parent_id: Set(None),
                        parent_tool_use_id: Set(None),
                        delegation_call_id: Set(None),
                        delegation_route_override: Set(None),
                        delegation_task_status: Set(None),
                        delegation_error_code: Set(None),
                        delegation_started_at: Set(None),
                        delegation_finished_at: Set(None),
                        delegation_tool_call_count: Set(None),
                        delegation_edit_tool_call_count: Set(None),
                        delegation_touched_files_json: Set(None),
                        delegation_touched_files_truncated: Set(None),
                        delegation_additions: Set(None),
                        delegation_deletions: Set(None),
                        delegation_line_counts_complete: Set(None),
                        message_count: Set(0),
                        created_at: Set(now),
                        updated_at: Set(now),
                        deleted_at: Set(None),
                        pinned_at: Set(None),
                        awaiting_reply_token: Set(None),
                        delegation_run_generation: Set(None),
                        last_termination_audit_json: Set(None),

                        origin_cwd: Set(None),
                    };
                    let inserted = sibling.insert(txn).await?;
                    Ok(inserted.id)
                })
            })
            .await
            .map_err(|e| AcpError::protocol(e.to_string()))
    }

    fn record_disconnect_intent(&self, connection_id: &str, origin: AcpDisconnectOrigin) {
        if let Some(injection) = self.delegation_snapshot() {
            injection.parent_connection_exit_causes.record_intent(
                connection_id,
                origin,
                chrono::Utc::now(),
            );
        }
    }

    pub async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError> {
        self.disconnect_with_origin(conn_id, AcpDisconnectOrigin::LegacyUnspecified)
            .await
    }

    pub async fn disconnect_with_origin(
        &self,
        conn_id: &str,
        origin: AcpDisconnectOrigin,
    ) -> Result<(), AcpError> {
        // Final incarnation CAS + intent + map remove → admission fence + clear
        // leases → Disconnect control. A lost CAS must leave the survivor routable.
        let incarnation = {
            let connections = self.connections.lock().await;
            match connections.get(conn_id) {
                Some(conn) => conn.connection_incarnation.clone(),
                None => return Err(AcpError::ConnectionNotFound(conn_id.into())),
            }
        };
        #[cfg(test)]
        {
            let hook = self
                .disconnect_final_cas_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(hook) = hook {
                hook.reached.notify_one();
                hook.resume.notified().await;
            }
        }
        let removed = {
            let mut connections = self.connections.lock().await;
            match connections.get(conn_id) {
                Some(conn) if conn.connection_incarnation == incarnation => {
                    self.record_disconnect_intent(conn_id, origin);
                    connections.remove(conn_id)
                }
                _ => None,
            }
        };
        if let Some(conn) = removed {
            self.clear_tool_leases(conn_id, &conn.connection_incarnation)
                .await;
            tracing::info!("[ACP] disconnect connection={}", conn_id);
            // Initialize does not drain `control_rx` until `run_conversation_loop`.
            // Abort only while still Connecting so a Connected session can unwind
            // through Disconnect (delegation cleanup + Disconnected emit).
            let pre_loop = {
                let st = conn.state.read().await;
                matches!(st.status, ConnectionStatus::Connecting)
            };
            if pre_loop {
                if let Some(abort) = conn.task_abort {
                    abort.abort();
                }
                if let Some(inj) = self.delegation_snapshot() {
                    crate::acp::connection::cleanup_delegation_parent(&inj, conn_id, &conn.state)
                        .await;
                }
                emit_with_state(
                    &conn.state,
                    &conn.emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Disconnected,
                    },
                )
                .await;
            }
            // Bound control-lane admit so a saturated/stalled receiver cannot
            // hang escalation after leases are already cleared and the map
            // entry removed. Send errors/timeouts are best-effort.
            let _ = tokio::time::timeout(
                crate::acp::tool_watchdog::CONTROL_LANE_ADMIT_TIMEOUT,
                conn.control_tx.send(ConnectionControl::Disconnect),
            )
            .await;
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Atomically fence admission and drop all tool-watchdog leases for an exact
    /// connection incarnation after its routing-map entry has been removed.
    async fn clear_tool_leases(&self, connection_id: &str, incarnation: &str) {
        let _ = self
            .tool_lease_registry
            .remove_connection(connection_id, incarnation)
            .await;
    }

    /// Host-only wait cancel registry (shared with listener/coordinator).
    pub fn wait_cancel_registry(
        &self,
    ) -> Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry> {
        self.wait_cancel_registry.clone()
    }

    /// Host-only MCP cancel registry.
    pub fn mcp_cancel_registry(&self) -> Arc<crate::acp::tool_watchdog::McpCancelRegistry> {
        self.mcp_cancel_registry.clone()
    }

    /// Generation-guarded terminal cancel admission on the control lane.
    ///
    /// Uses bounded try_send + oneshot ack; never awaits process-tree kill.
    /// On admit/ack timeout returns `Failed` so the supervisor continues
    /// the escalation budget.
    pub async fn admit_cancel_terminal_if_current(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome> {
        use crate::acp::connection::ConnectionControl;
        use crate::acp::tool_watchdog::{
            SpecificCancelOutcome, TERMINAL_ACK_TIMEOUT, TERMINAL_ADMIT_TIMEOUT,
        };

        let (state, control_tx) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(&stamp.connection_id)
                .ok_or(SpecificCancelOutcome::Failed)?;
            if conn.connection_incarnation != stamp.connection_incarnation {
                return Err(SpecificCancelOutcome::Failed);
            }
            (Arc::clone(&conn.state), conn.control_tx.clone())
        };
        {
            let state = state.read().await;
            match state.active_turn_generation {
                Some(active) if active == stamp.turn_generation => {}
                _ => return Err(SpecificCancelOutcome::Failed),
            }
        }

        let admit_deadline = tokio::time::Instant::now() + TERMINAL_ADMIT_TIMEOUT;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let mut pending = Some(ConnectionControl::CancelTerminal {
            session_id: session_id.to_string(),
            terminal_id: terminal_id.to_string(),
            reply: reply_tx,
        });

        while let Some(msg) = pending.take() {
            match control_tx.try_send(msg) {
                Ok(()) => break,
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    if tokio::time::Instant::now() >= admit_deadline {
                        return Err(SpecificCancelOutcome::Failed);
                    }
                    pending = Some(returned);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(SpecificCancelOutcome::Failed);
                }
            }
        }

        match tokio::time::timeout(TERMINAL_ACK_TIMEOUT, reply_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => Err(SpecificCancelOutcome::Failed),
        }
    }

    /// Host-only Broker cancel for a verified singleton task.
    ///
    /// `cause` selects the Broker reason code: UserStop → `user_cancelled`,
    /// AutoTimeout → `tool_stalled_timeout` (never public Timeout no-op path).
    pub async fn cancel_delegation_task_if_verified(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        task_id: &str,
        cause: crate::acp::tool_watchdog::CancelCause,
    ) -> Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome> {
        use crate::acp::termination::{
            AcpDisconnectOrigin, AcpTerminationClassification, AcpTerminationReason,
            AcpTerminationSource, AcpTerminationSummaryV1,
        };
        use crate::acp::tool_watchdog::{error_code_for_cause, CancelCause, SpecificCancelOutcome};

        // Generation guard: incarnation + active turn generation must match.
        {
            let connections = self.connections.lock().await;
            let Some(conn) = connections.get(&stamp.connection_id) else {
                return Err(SpecificCancelOutcome::Failed);
            };
            if conn.connection_incarnation != stamp.connection_incarnation {
                return Err(SpecificCancelOutcome::Failed);
            }
            let state = conn.state.read().await;
            match state.active_turn_generation {
                Some(active) if active == stamp.turn_generation => {}
                _ => return Err(SpecificCancelOutcome::Failed),
            }
        }

        let Some(injection) = self.delegation_snapshot() else {
            return Err(SpecificCancelOutcome::Failed);
        };
        let conversation_id = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(&stamp.connection_id)
                .ok_or(SpecificCancelOutcome::Failed)?;
            let state = conn.state.clone();
            drop(connections);
            let id = state.read().await.conversation_id;
            id
        };
        let reason = error_code_for_cause(cause);
        let observed_at = chrono::Utc::now();
        let mut termination = match cause {
            CancelCause::AutoTimeout => AcpTerminationSummaryV1::new(
                AcpTerminationSource::Watchdog,
                AcpTerminationReason::ToolStalledTimeout,
                AcpTerminationClassification::AutomatedAmbiguous,
                true,
                observed_at,
            ),
            CancelCause::UserStop => AcpTerminationSummaryV1::new(
                AcpTerminationSource::Frontend,
                AcpTerminationReason::UserCancelled,
                AcpTerminationClassification::Explicit,
                true,
                observed_at,
            ),
        };
        if cause == CancelCause::UserStop {
            termination.frontend_origin = Some(AcpDisconnectOrigin::LegacyUnspecified);
            termination.requested_at = Some(observed_at);
        }
        let _report = injection
            .broker
            .cancel_task_by_id_with_termination(
                &stamp.connection_id,
                conversation_id,
                task_id,
                reason,
                termination,
            )
            .await;
        Ok(())
    }

    /// Cancel only the request-scoped wait handle; never child tasks.
    ///
    /// Validates the **full** [`WaitStamp`] (incarnation, turn generation,
    /// parent tool identity) built from the lease — never a reduced parent
    /// match. `cause` is forwarded so UserStop emits `user_cancelled`.
    pub async fn cancel_delegation_wait_if_verified(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        wait_id: &str,
        cause: crate::acp::tool_watchdog::CancelCause,
    ) -> Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome> {
        use crate::acp::tool_watchdog::{
            wait_stamp_from_lease, SpecificCancelOutcome, WaitCancelResult,
        };

        let conversation_id = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(&stamp.connection_id)
                .ok_or(SpecificCancelOutcome::Failed)?;
            if conn.connection_incarnation != stamp.connection_incarnation {
                return Err(SpecificCancelOutcome::Failed);
            }
            let state = conn.state.clone();
            drop(connections);
            let snap = state.read().await;
            match snap.active_turn_generation {
                Some(active) if active == stamp.turn_generation => {}
                _ => return Err(SpecificCancelOutcome::Failed),
            }
            snap.conversation_id.ok_or(SpecificCancelOutcome::Failed)?
        };

        let expected = wait_stamp_from_lease(stamp, wait_id, conversation_id);
        match self.wait_cancel_registry.cancel(&expected, cause).await {
            WaitCancelResult::Cancelled | WaitCancelResult::AlreadySettled => Ok(()),
            WaitCancelResult::NotFound | WaitCancelResult::Stale => {
                Err(SpecificCancelOutcome::Failed)
            }
        }
    }

    /// Invoke an opaque MCP cancel token under generation guard.
    pub async fn cancel_mcp_if_verified(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        token: crate::acp::tool_watchdog::McpCancelToken,
    ) -> Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome> {
        use crate::acp::tool_watchdog::{McpCancelResult, SpecificCancelOutcome};

        {
            let connections = self.connections.lock().await;
            let Some(conn) = connections.get(&stamp.connection_id) else {
                return Err(SpecificCancelOutcome::Failed);
            };
            if conn.connection_incarnation != stamp.connection_incarnation {
                return Err(SpecificCancelOutcome::Failed);
            }
            let state = conn.state.read().await;
            match state.active_turn_generation {
                Some(active) if active == stamp.turn_generation => {}
                _ => return Err(SpecificCancelOutcome::Failed),
            }
        }

        match self.mcp_cancel_registry.cancel(stamp, token).await {
            McpCancelResult::Cancelled | McpCancelResult::AlreadySettled => Ok(()),
            McpCancelResult::Unsupported => Ok(()), // invoke ok; escalate if lease stays live
            McpCancelResult::Stale | McpCancelResult::NotFound | McpCancelResult::TimedOut => {
                Err(SpecificCancelOutcome::Failed)
            }
        }
    }

    /// Generation-guarded ACP turn cancel (session/cancel control).
    ///
    /// Sends [`ConnectionControl::CancelTurn`] — **not** unqualified
    /// [`ConnectionControl::Cancel`] — so automatic timeout never routes
    /// through user-cancel parent-tree cascade semantics. Requires an active
    /// turn generation match (stale/`None` is rejected).
    ///
    /// Control-lane admission is bounded by
    /// [`CONTROL_LANE_ADMIT_TIMEOUT`](crate::acp::tool_watchdog::CONTROL_LANE_ADMIT_TIMEOUT).
    /// On timeout/closed lane returns `Err` so the supervisor marks turn-stage
    /// failed and continues disconnect/settlement.
    pub async fn cancel_turn_if_current(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        cause: crate::acp::tool_watchdog::CancelCause,
    ) -> Result<(), ()> {
        use crate::acp::tool_watchdog::CONTROL_LANE_ADMIT_TIMEOUT;

        let control_tx = {
            let connections = self.connections.lock().await;
            let conn = connections.get(&stamp.connection_id).ok_or(())?;
            if conn.connection_incarnation != stamp.connection_incarnation {
                return Err(());
            }
            let state = conn.state.read().await;
            match state.active_turn_generation {
                Some(active_gen) if active_gen == stamp.turn_generation => {}
                _ => return Err(()),
            }
            conn.control_tx.clone()
        };
        match tokio::time::timeout(
            CONTROL_LANE_ADMIT_TIMEOUT,
            control_tx.send(crate::acp::connection::ConnectionControl::CancelTurn {
                turn_generation: stamp.turn_generation,
                cause,
            }),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(()),
        }
    }

    /// Production cancel host for the escalation supervisor.
    pub fn production_cancel_host(&self) -> ProductionCancelHost {
        ProductionCancelHost {
            manager: self.clone_ref(),
        }
    }

    /// Public supervisor entry: escalate one already-claimed lease through
    /// specific-cancel → turn → disconnect. Task 7 will load settings and
    /// schedule scans; this makes the executor reachable for real connections.
    pub async fn escalate_claimed_lease(
        &self,
        claim: &crate::acp::tool_watchdog::CancellationClaim,
        convergence: Duration,
    ) -> crate::acp::tool_watchdog::EscalationReport {
        let host = self.production_cancel_host();
        let probe = ProductionConvergenceProbe {
            manager: self.clone_ref(),
        };
        crate::acp::tool_watchdog::escalate_claimed_lease(
            &host,
            &probe,
            self.tool_lease_registry.as_ref(),
            claim,
            convergence,
        )
        .await
    }

    /// Scan the shared lease registry, advance overdue warnings into Grace, and
    /// execute every `ClaimCancel` action.
    ///
    /// On [`RegistryAction::PublishWarning`]:
    /// 1. Call [`ToolExecutionLeaseRegistry::warning_published`] so the lease
    ///    leaves `Warning` and enters `Grace` (required for a later scan to
    ///    emit `ClaimCancel`).
    /// 2. Emit `AcpEvent::ToolWatchdogChanged` with the Grace projection when
    ///    the connection is still live.
    ///
    /// Escalations are **spawned independently** so the scan returns after claim
    /// and warn-ack work only. One stuck cancellation cannot block the 1s
    /// periodic scan or coalescing wakes; each escalation still runs under its
    /// own convergence budget.
    ///
    /// The registry never emits warn + cancel for the same lease in one pass;
    /// this method preserves that separation by acknowledging warnings without
    /// claiming cancel on the same scan.
    pub async fn scan_and_execute_cancellations(
        &self,
        at: crate::acp::tool_watchdog::WatchdogInstant,
        convergence: Duration,
    ) -> ScanCancelReport {
        use crate::acp::tool_watchdog::{RegistryAction, WatchdogMetricLabel};

        let actions = self.tool_lease_registry.scan(at).await;
        let mut warnings = Vec::new();
        let mut escalations_spawned = 0usize;
        for action in actions {
            match action {
                RegistryAction::ClaimCancel { claim, projection } => {
                    // Emit Cancelling immediately so clients clear Grace controls
                    // and show the claim transition without waiting for settle.
                    self.emit_tool_watchdog_changed(&claim.stamp.connection_id, projection.clone())
                        .await;
                    let category = self
                        .tool_lease_registry
                        .lease_category(&claim.stamp.lease_id)
                        .await
                        .unwrap_or(crate::acp::tool_watchdog::ToolCategory::Other);
                    let mgr = self.clone_ref();
                    let run = async move {
                        let agent = mgr
                            .agent_type_for_connection(&claim.stamp.connection_id)
                            .await;
                        let label = WatchdogMetricLabel::new(agent, category);
                        if matches!(
                            claim.cause,
                            crate::acp::tool_watchdog::CancelCause::AutoTimeout
                        ) {
                            mgr.tool_watchdog_metrics
                                .record_automatic_timeout(label.clone());
                        }
                        let report = mgr.escalate_claimed_lease(&claim, convergence).await;
                        // Supervisor-owned settlement must publish TimedOut so
                        // attach maps and banners drop Grace/Cancelling state.
                        if let Some(settled) = report.settled_projection.clone() {
                            mgr.emit_tool_watchdog_changed(&claim.stamp.connection_id, settled)
                                .await;
                        }
                        mgr.tool_watchdog_metrics.record_escalation(label, &report);
                    };
                    #[cfg(feature = "tauri-runtime")]
                    tauri::async_runtime::spawn(run);
                    #[cfg(not(feature = "tauri-runtime"))]
                    tokio::spawn(run);
                    escalations_spawned += 1;
                }
                RegistryAction::PublishWarning {
                    stamp,
                    projection: _,
                } => {
                    // Advance Warning → Grace so a subsequent scan can ClaimCancel.
                    // Registry scan never pairs warn+cancel for the same lease.
                    match self
                        .tool_lease_registry
                        .warning_published(&stamp.lease_id, stamp.version, at)
                        .await
                    {
                        Ok(grace_projection) => {
                            let agent = self.agent_type_for_connection(&stamp.connection_id).await;
                            let category = grace_projection.tool_title;
                            self.tool_watchdog_metrics
                                .record_warning_episode(WatchdogMetricLabel::new(agent, category));
                            self.emit_tool_watchdog_changed(
                                &stamp.connection_id,
                                grace_projection.clone(),
                            )
                            .await;
                            warnings.push((stamp, grace_projection));
                        }
                        Err(_) => {
                            // Stale/missing between scan and ack — skip.
                        }
                    }
                }
                other => {
                    // EmitCleared etc. are not produced by scan today.
                    let _ = other;
                }
            }
        }
        ScanCancelReport {
            escalations_spawned,
            warnings,
        }
    }

    pub(crate) async fn agent_type_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<crate::models::AgentType> {
        let connections = self.connections.lock().await;
        connections.get(connection_id).map(|c| c.agent_type)
    }

    /// CAS extend a Grace lease; emits projection on success.
    pub async fn tool_watchdog_extend(
        &self,
        lease_id: &str,
        version: u64,
    ) -> Result<
        crate::acp::tool_watchdog::ToolWatchdogProjection,
        crate::acp::tool_watchdog::StaleLease,
    > {
        use crate::acp::tool_watchdog::{WatchdogInstant, WatchdogMetricLabel};

        let at = WatchdogInstant::now();
        let projection = self
            .tool_lease_registry
            .extend(lease_id, version, at)
            .await?;
        if let Some(stamp) = self.tool_lease_registry.lease_stamp(lease_id).await {
            let agent = self.agent_type_for_connection(&stamp.connection_id).await;
            self.tool_watchdog_metrics
                .record_extension(WatchdogMetricLabel::new(agent, projection.tool_title));
            self.emit_tool_watchdog_changed(&stamp.connection_id, projection.clone())
                .await;
        }
        self.wake_tool_watchdog();
        Ok(projection)
    }

    /// CAS user-stop claim; emits Cancelling and schedules escalation.
    ///
    /// Uses the Cancelling projection returned atomically by
    /// `ToolExecutionLeaseRegistry::claim_cancel` so a concurrent
    /// complete/settle cannot flip a successful claim into a stale error via a
    /// second live lookup.
    pub async fn tool_watchdog_user_cancel(
        &self,
        lease_id: &str,
        version: u64,
    ) -> Result<
        crate::acp::tool_watchdog::ToolWatchdogProjection,
        crate::acp::tool_watchdog::StaleLease,
    > {
        use crate::acp::tool_watchdog::{
            CancelCause, WatchdogMetricLabel, CANCEL_CONVERGENCE_SECS,
        };
        use std::time::Duration;

        let (claim, projection) = self
            .tool_lease_registry
            .claim_cancel(lease_id, version, CancelCause::UserStop)
            .await?;
        let agent = self
            .agent_type_for_connection(&claim.stamp.connection_id)
            .await;
        self.tool_watchdog_metrics
            .record_user_stop(WatchdogMetricLabel::new(agent, projection.tool_title));
        self.emit_tool_watchdog_changed(&claim.stamp.connection_id, projection.clone())
            .await;

        // Escalate in the background so the control API stays responsive.
        let mgr = self.clone_ref();
        let claim_bg = claim;
        let tool_category = projection.tool_title;
        let run = async move {
            let agent = mgr
                .agent_type_for_connection(&claim_bg.stamp.connection_id)
                .await;
            let label = WatchdogMetricLabel::new(agent, tool_category);
            let report = mgr
                .escalate_claimed_lease(&claim_bg, Duration::from_secs(CANCEL_CONVERGENCE_SECS))
                .await;
            if let Some(settled) = report.settled_projection.clone() {
                mgr.emit_tool_watchdog_changed(&claim_bg.stamp.connection_id, settled)
                    .await;
            }
            mgr.tool_watchdog_metrics.record_escalation(label, &report);
        };
        #[cfg(feature = "tauri-runtime")]
        tauri::async_runtime::spawn(run);
        #[cfg(not(feature = "tauri-runtime"))]
        tokio::spawn(run);

        Ok(projection)
    }

    /// Best-effort `ToolWatchdogChanged` emit for a live connection.
    ///
    /// No-ops when the connection has already been removed (map-missing);
    /// lease state is still advanced by the registry caller.
    pub(crate) async fn emit_tool_watchdog_changed(
        &self,
        connection_id: &str,
        projection: crate::acp::tool_watchdog::ToolWatchdogProjection,
    ) {
        let (state, emitter) = {
            let connections = self.connections.lock().await;
            let Some(conn) = connections.get(connection_id) else {
                return;
            };
            (conn.state.clone(), conn.emitter.clone())
        };
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::ToolWatchdogChanged { projection },
        )
        .await;
    }

    /// Emit Cleared projections after registry demotions that span connections
    /// (settings disable). Resolves each lease's connection via the registry.
    pub(crate) async fn emit_tool_watchdog_clears(
        &self,
        projections: impl IntoIterator<Item = crate::acp::tool_watchdog::ToolWatchdogProjection>,
    ) {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;
        for projection in projections {
            if !matches!(
                projection.phase,
                ToolWatchdogPhase::Cleared | ToolWatchdogPhase::TimedOut
            ) {
                continue;
            }
            let Some(stamp) = self
                .tool_lease_registry
                .lease_stamp(&projection.lease_id)
                .await
            else {
                continue;
            };
            self.emit_tool_watchdog_changed(&stamp.connection_id, projection)
                .await;
        }
    }

    /// Incarnation-guarded disconnect (final convergence fallback).
    pub async fn disconnect_if_incarnation(
        &self,
        connection_id: &str,
        incarnation: &str,
    ) -> Result<(), ()> {
        {
            let connections = self.connections.lock().await;
            let conn = connections.get(connection_id).ok_or(())?;
            if conn.connection_incarnation != incarnation {
                return Err(());
            }
        }
        self.disconnect_with_origin(connection_id, AcpDisconnectOrigin::IdleTimeout)
            .await
            .map_err(|_| ())
    }
}

/// Result of [`ConnectionManager::scan_and_execute_cancellations`].
///
/// Escalations are spawned independently; this report only counts how many
/// were scheduled so the scan loop stays 1s-responsive.
#[derive(Debug)]
pub struct ScanCancelReport {
    pub escalations_spawned: usize,
    pub warnings: Vec<(
        crate::acp::tool_watchdog::LeaseStamp,
        crate::acp::tool_watchdog::ToolWatchdogProjection,
    )>,
}

/// Production [`CancelHost`] that drives real connection control lanes,
/// Broker cancel, wait cancel, MCP cancel, and incarnation disconnect.
pub struct ProductionCancelHost {
    manager: ConnectionManager,
}

impl crate::acp::tool_watchdog::CancelHost for ProductionCancelHost {
    fn admit_cancel_terminal(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        session_id: &str,
        terminal_id: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome>,
                > + Send
                + '_,
        >,
    > {
        let session_id = session_id.to_string();
        let terminal_id = terminal_id.to_string();
        let stamp = stamp.clone();
        Box::pin(async move {
            self.manager
                .admit_cancel_terminal_if_current(&stamp, &session_id, &terminal_id)
                .await
        })
    }

    fn cancel_delegation_task(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        task_id: &str,
        cause: crate::acp::tool_watchdog::CancelCause,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome>,
                > + Send
                + '_,
        >,
    > {
        let task_id = task_id.to_string();
        let stamp = stamp.clone();
        Box::pin(async move {
            self.manager
                .cancel_delegation_task_if_verified(&stamp, &task_id, cause)
                .await
        })
    }

    fn cancel_delegation_wait(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        wait_id: &str,
        cause: crate::acp::tool_watchdog::CancelCause,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome>,
                > + Send
                + '_,
        >,
    > {
        let wait_id = wait_id.to_string();
        let stamp = stamp.clone();
        Box::pin(async move {
            self.manager
                .cancel_delegation_wait_if_verified(&stamp, &wait_id, cause)
                .await
        })
    }

    fn cancel_mcp(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        token: crate::acp::tool_watchdog::McpCancelToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), crate::acp::tool_watchdog::SpecificCancelOutcome>,
                > + Send
                + '_,
        >,
    > {
        let stamp = stamp.clone();
        Box::pin(async move { self.manager.cancel_mcp_if_verified(&stamp, token).await })
    }

    fn cancel_turn(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
        cause: crate::acp::tool_watchdog::CancelCause,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + '_>> {
        let stamp = stamp.clone();
        Box::pin(async move { self.manager.cancel_turn_if_current(&stamp, cause).await })
    }

    fn disconnect_incarnation(
        &self,
        connection_id: &str,
        incarnation: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + '_>> {
        let connection_id = connection_id.to_string();
        let incarnation = incarnation.to_string();
        Box::pin(async move {
            self.manager
                .disconnect_if_incarnation(&connection_id, &incarnation)
                .await
        })
    }
}

/// Production convergence probe: lease liveness + turn still Prompting.
struct ProductionConvergenceProbe {
    manager: ConnectionManager,
}

impl crate::acp::tool_watchdog::ConvergenceProbe for ProductionConvergenceProbe {
    fn lease_is_live(
        &self,
        lease_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        let lease_id = lease_id.to_string();
        Box::pin(async move { self.manager.tool_lease_registry.is_live(&lease_id).await })
    }

    fn turn_still_prompting(
        &self,
        stamp: &crate::acp::tool_watchdog::LeaseStamp,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        let stamp = stamp.clone();
        Box::pin(async move {
            let connections = self.manager.connections.lock().await;
            let Some(conn) = connections.get(&stamp.connection_id) else {
                return false;
            };
            if conn.connection_incarnation != stamp.connection_incarnation {
                return false;
            }
            let state = conn.state.read().await;
            state.active_turn_generation == Some(stamp.turn_generation) && state.turn_in_flight
        })
    }
}

struct DisconnectSelection {
    connection_id: String,
    connection_incarnation: String,
    ownership: Option<DisconnectOwnershipStamp>,
}

struct DisconnectOwnershipStamp {
    owner_window_label: String,
    owner_operation_id: Option<String>,
    ownership_generation: u64,
}

impl DisconnectSelection {
    fn incarnation_only(connection_id: String, connection: &AgentConnection) -> Self {
        Self {
            connection_id,
            connection_incarnation: connection.connection_incarnation.clone(),
            ownership: None,
        }
    }

    fn with_ownership(connection_id: String, connection: &AgentConnection) -> Self {
        Self {
            connection_id,
            connection_incarnation: connection.connection_incarnation.clone(),
            ownership: Some(DisconnectOwnershipStamp {
                owner_window_label: connection.owner_window_label.clone(),
                owner_operation_id: connection.owner_operation_id.clone(),
                ownership_generation: connection.ownership_generation,
            }),
        }
    }

    fn matches(&self, connection: &AgentConnection) -> bool {
        if connection.connection_incarnation != self.connection_incarnation {
            return false;
        }
        self.ownership.as_ref().is_none_or(|ownership| {
            connection.owner_window_label == ownership.owner_window_label
                && connection.owner_operation_id == ownership.owner_operation_id
                && connection.ownership_generation == ownership.ownership_generation
        })
    }
}

impl ConnectionManager {
    /// Revalidate each captured selection fence, remove winners, then fence and
    /// clear only those removed incarnations before returning them for control.
    async fn take_connections_for_disconnect(
        &self,
        planned: Vec<DisconnectSelection>,
        origin: AcpDisconnectOrigin,
    ) -> Vec<(String, crate::acp::connection::AgentConnection)> {
        #[cfg(test)]
        {
            let hook = self
                .disconnect_final_cas_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(hook) = hook {
                hook.reached.notify_one();
                hook.resume.notified().await;
            }
        }
        // Author intent and remove only while the captured selection fence
        // still matches under the final connection-map lock.
        let mut removed = Vec::with_capacity(planned.len());
        {
            let mut connections = self.connections.lock().await;
            for selected in planned {
                let same = connections
                    .get(&selected.connection_id)
                    .is_some_and(|connection| selected.matches(connection));
                if same {
                    self.record_disconnect_intent(&selected.connection_id, origin);
                    if let Some(connection) = connections.remove(&selected.connection_id) {
                        removed.push((selected.connection_id, connection));
                    }
                }
            }
        }
        for (connection_id, connection) in &removed {
            self.clear_tool_leases(connection_id, &connection.connection_incarnation)
                .await;
        }
        removed
    }

    /// Compare-and-disconnect under the connections lock.
    ///
    /// When all lease expectations are `None`/empty, behaves like
    /// [`Self::disconnect`] (legacy main / non-leased paths).
    ///
    /// When any lease field is set: only remove+disconnect if the live
    /// connection still matches. Stale ownership is a successful no-op so
    /// delayed cleanup cannot kill a newer owner after rebind.
    pub async fn disconnect_if_owner(
        &self,
        conn_id: &str,
        expected_owner_window: Option<&str>,
        expected_operation_id: Option<&str>,
        expected_generation: Option<u64>,
        origin: AcpDisconnectOrigin,
    ) -> Result<(), AcpError> {
        if origin != AcpDisconnectOrigin::ApplicationShutdown
            && self
                .shared_session_broker
                .is_managed_connection(conn_id)
                .await
        {
            return Err(SharedSessionError::ProtocolRequired.into());
        }
        let expect_window = expected_owner_window
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let expect_op = expected_operation_id
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let has_lease =
            expect_window.is_some() || expect_op.is_some() || expected_generation.is_some();
        if !has_lease {
            return self.disconnect_with_origin(conn_id, origin).await;
        }

        // Snapshot the matching ownership incarnation, then revalidate and
        // remove under the final map lock. Drop cleanup remains an idempotent backstop.
        let incarnation = {
            let connections = self.connections.lock().await;
            let Some(conn) = connections.get(conn_id) else {
                // Already gone — idempotent success for leased cleanup.
                return Ok(());
            };
            if let Some(win) = expect_window {
                if conn.owner_window_label != win {
                    tracing::info!(
                        "[ACP] disconnect_if_owner stale window conn={} expected={} actual={}",
                        conn_id,
                        win,
                        conn.owner_window_label
                    );
                    return Ok(());
                }
            }
            if let Some(op) = expect_op {
                if conn.owner_operation_id.as_deref() != Some(op) {
                    tracing::info!(
                        "[ACP] disconnect_if_owner stale operation conn={} expected={} actual={:?}",
                        conn_id,
                        op,
                        conn.owner_operation_id
                    );
                    return Ok(());
                }
            }
            if let Some(gen) = expected_generation {
                if conn.ownership_generation != gen {
                    tracing::info!(
                        "[ACP] disconnect_if_owner stale generation conn={} expected={} actual={}",
                        conn_id,
                        gen,
                        conn.ownership_generation
                    );
                    return Ok(());
                }
            }
            conn.connection_incarnation.clone()
        };
        #[cfg(test)]
        {
            let hook = self
                .disconnect_final_cas_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(hook) = hook {
                hook.reached.notify_one();
                hook.resume.notified().await;
            }
        }
        let removed = {
            let mut connections = self.connections.lock().await;
            let Some(conn) = connections.get(conn_id) else {
                return Ok(());
            };
            if let Some(win) = expect_window {
                if conn.owner_window_label != win {
                    return Ok(());
                }
            }
            if let Some(op) = expect_op {
                if conn.owner_operation_id.as_deref() != Some(op) {
                    return Ok(());
                }
            }
            if let Some(gen) = expected_generation {
                if conn.ownership_generation != gen {
                    return Ok(());
                }
            }
            if conn.connection_incarnation != incarnation {
                return Ok(());
            }
            self.record_disconnect_intent(conn_id, origin);
            connections.remove(conn_id)
        };
        if let Some(conn) = removed {
            self.clear_tool_leases(conn_id, &conn.connection_incarnation)
                .await;
            tracing::info!(
                "[ACP] disconnect_if_owner connection={} window={:?} op={:?} gen={:?}",
                conn_id,
                expect_window,
                expect_op,
                expected_generation
            );
            let _ = conn.control_tx.send(ConnectionControl::Disconnect).await;
        }
        Ok(())
    }

    /// Probe an agent for the modes / config_options it advertises on a fresh
    /// session, then immediately disconnect. The probe runs with
    /// `EventEmitter::Noop` so no event reaches the desktop webview, the
    /// global `WebEventBroadcaster`, or the `InternalEventBus` — the events
    /// land only in this probe connection's own (unsubscribed) per-connection
    /// stream and in its `SessionState` (which is the read source here).
    ///
    /// Used by the delegation-settings UI to enumerate the options the user
    /// can override, with the guarantee that what the UI shows is exactly
    /// what `codeg-mcp` will pass through to `session/set_config_option`
    /// when a delegation actually fires.
    ///
    /// Returns `Ok(snapshot)` even when the agent advertises no options
    /// (empty `config_options`, `None` modes) — that's a valid outcome the
    /// UI can render as "this agent has nothing to configure."
    pub async fn probe_agent_options(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        launch_inputs: AcpLaunchInputs,
    ) -> Result<AgentOptionsSnapshot, AcpError> {
        // Owner window label is informational only (used for
        // disconnect_by_owner_window), but worth being explicit so a probe
        // connection that somehow leaks past the disconnect below is easy to
        // identify in logs / debug snapshots.
        let owner_window = "delegation-probe".to_string();
        // Serialize concurrent probes for the same agent_type. Rapid tab
        // switching in the settings UI would otherwise fan out one real
        // CLI process per click — each one running up to 60s. The mutex
        // bounds this to one in-flight probe per agent type; different
        // agent_types still probe in parallel.
        //
        // The outer `probe_locks` guard MUST be dropped BEFORE the
        // `.lock_owned().await` on the per-agent mutex. If we held it
        // across the await, a probe queued behind another for the SAME
        // agent_type would keep the outer map locked, blocking probes
        // for every OTHER agent_type too — silently turning the
        // per-agent serialization into a global one.
        let per_agent_lock: Arc<tokio::sync::Mutex<()>> = {
            let mut locks = self.probe_locks.lock().await;
            locks
                .entry(agent_type)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _probe_guard = per_agent_lock.lock_owned().await;
        let conn_id = self
            .spawn_agent(
                agent_type,
                working_dir,
                None, // brand-new session — no resume
                launch_inputs,
                owner_window,
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                internal_probe_launch_context(),
                None,
                None,
            )
            .await?;

        // Hold an `Arc<RwLock<SessionState>>` alongside the manager's own
        // entry so the state survives even if the connection task cleans
        // up its map slot mid-handshake. Without this, an agent that
        // errors during Initialize would trigger cleanup before the
        // probe's poll loop sees the `AcpEvent::Error` payload, and
        // `wait_for_session_options` would surface the unhelpful
        // `ConnectionNotFound` instead of the agent's own error text.
        let state_arc = self.get_state(&conn_id).await;

        // Generous timeout because some agents (Gemini in particular) take
        // 8-10s just to answer Initialize before session/new can even start;
        // a tight cap here would consistently return an empty snapshot and
        // make the settings UI claim those agents have nothing to configure.
        // Matches the per-step Initialize timeout in `connection.rs`.
        let probe_timeout = Duration::from_secs(60);
        let raw_snapshot = self.wait_for_session_options(&conn_id, probe_timeout).await;

        // If the wait errored, prefer the agent's own captured error
        // message over the generic ProbeTimedOut / ConnectionNotFound —
        // an agent that died on Initialize already explained why.
        let snapshot = match raw_snapshot {
            Ok(s) => Ok(s),
            Err(wait_err) => {
                let captured = if let Some(state) = state_arc.as_ref() {
                    state.read().await.last_error.clone()
                } else {
                    None
                };
                Err(match captured {
                    Some(err) => AcpError::protocol(err.message),
                    None => wait_err,
                })
            }
        };

        // Always disconnect — including on Err — so a failed probe doesn't
        // leak an agent process. Ignore disconnect errors (best-effort
        // cleanup; the agent will exit when its stdio is dropped anyway).
        let _ = self
            .disconnect_with_origin(&conn_id, AcpDisconnectOrigin::InternalJobComplete)
            .await;
        snapshot
    }

    /// Poll a connection's `SessionState` until the agent signals it has
    /// finished publishing its initial selectors (`SelectorsReady`), then
    /// give a small grace window for any tightly-following follow-up updates
    /// before snapshotting. Waiting on `selectors_ready` — not just
    /// `config_options.is_some()` — matters because some agents emit an
    /// empty `SessionConfigOptions` first and then push the real options
    /// in a subsequent update; returning on the first `Some(vec![])` would
    /// race ahead of those updates and report the agent as having nothing
    /// to configure.
    ///
    /// The `SessionConfigOptions` / `SelectorsReady` ACP events populate
    /// `SessionState` via `apply_event` regardless of which `EventEmitter`
    /// variant the connection uses — that's why the probe can rely on
    /// `Noop` and still observe the values here.
    ///
    /// Returns `AcpError::ProbeTimedOut` when the timeout elapses without
    /// `selectors_ready` ever flipping to `true`. Distinguishing that case
    /// from a clean "ready with no options" snapshot lets the UI tell the
    /// user "the agent never published its options — retry" instead of
    /// silently claiming the agent has nothing to configure.
    async fn wait_for_session_options(
        &self,
        conn_id: &str,
        timeout: Duration,
    ) -> Result<AgentOptionsSnapshot, AcpError> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(50);
        // Grace window between `selectors_ready` flipping true and the
        // snapshot we return. Lets a stragging `ConfigOptionUpdate` that
        // an agent emits in the same tick land before we read.
        let grace_period = Duration::from_millis(500);
        let mut selectors_ready_at: Option<std::time::Instant> = None;
        loop {
            let (config_options, modes, available_commands, selectors_ready) = {
                let conns = self.connections.lock().await;
                let conn = conns
                    .get(conn_id)
                    .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
                let s = conn.state.read().await;
                (
                    s.config_options.clone(),
                    s.modes.clone(),
                    s.available_commands.clone(),
                    s.selectors_ready,
                )
            };
            if selectors_ready {
                let ready_at = *selectors_ready_at.get_or_insert_with(std::time::Instant::now);
                if ready_at.elapsed() >= grace_period {
                    // Commands ride along from the same probe session (the grace
                    // window lets a late `available_commands` land before we read).
                    return Ok(AgentOptionsSnapshot {
                        modes,
                        config_options: config_options.unwrap_or_default(),
                        available_commands,
                    });
                }
            }
            if start.elapsed() >= timeout {
                return Err(AcpError::ProbeTimedOut);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    pub async fn disconnect_by_owner_window(&self, owner_window_label: &str) -> usize {
        let planned: Vec<DisconnectSelection> = {
            let connections = self.connections.lock().await;
            connections
                .iter()
                .filter_map(|(id, conn)| {
                    if conn.owner_window_label == owner_window_label {
                        Some(DisconnectSelection::with_ownership(id.clone(), conn))
                    } else {
                        None
                    }
                })
                .collect()
        };
        let removed = self
            .take_connections_for_disconnect(planned, AcpDisconnectOrigin::ProviderUnmount)
            .await;
        let disconnected = removed.len();
        for (_id, conn) in removed {
            let _ = conn.control_tx.send(ConnectionControl::Disconnect).await;
        }
        tracing::info!(
            "[ACP] disconnect by owner window owner_window={} count={}",
            owner_window_label,
            disconnected
        );
        disconnected
    }

    /// Disconnect connections owned by `owner_window_label` **and** matching
    /// `owner_operation_id` incarnation. Connections without an operation stamp
    /// are only matched when `operation_id` is empty (legacy main path).
    pub async fn disconnect_by_owner_window_and_operation(
        &self,
        owner_window_label: &str,
        operation_id: &str,
    ) -> usize {
        let planned: Vec<DisconnectSelection> = {
            let connections = self.connections.lock().await;
            connections
                .iter()
                .filter_map(|(id, conn)| {
                    if conn.owner_window_label != owner_window_label {
                        return None;
                    }
                    let conn_op = conn.owner_operation_id.as_deref().unwrap_or("");
                    if conn_op == operation_id {
                        Some(DisconnectSelection::with_ownership(id.clone(), conn))
                    } else {
                        None
                    }
                })
                .collect()
        };
        let removed = self
            .take_connections_for_disconnect(planned, AcpDisconnectOrigin::ProviderUnmount)
            .await;
        let disconnected = removed.len();
        for (_id, conn) in removed {
            let _ = conn.control_tx.send(ConnectionControl::Disconnect).await;
        }
        tracing::info!(
            "[ACP] disconnect by owner window+op owner_window={} op={} count={}",
            owner_window_label,
            operation_id,
            disconnected
        );
        disconnected
    }

    /// Idle-only residual disconnect for pop-out close.
    ///
    /// Two-phase (sweep-style): snapshot candidates matching `(label, op)`, then
    /// re-validate under the connections lock that the connection is still on
    /// that stamp, same incarnation, and idle (`Connected`, no pending
    /// permission, no active background work) before remove + Disconnect.
    ///
    /// Final busy re-check is exclusive: phase 3 holds a session-state **write**
    /// lock across idle revalidation and map removal so concurrent Prompting
    /// cannot slip in after an idle read and still get disconnected (TOCTOU).
    /// Busy leftovers are never force-killed.
    pub async fn disconnect_idle_by_owner_window_and_operation(
        &self,
        owner_window_label: &str,
        operation_id: &str,
    ) -> usize {
        let now = chrono::Utc::now();
        // Snapshot lease (id, owner, op, gen, incarnation) for stamp matches.
        // Idle is re-checked under exclusive state write + map lock at removal.
        let candidates: Vec<(String, String, Option<String>, u64, String)> = {
            let connections = self.connections.lock().await;
            connections
                .iter()
                .filter_map(|(id, conn)| {
                    if conn.owner_window_label != owner_window_label {
                        return None;
                    }
                    let conn_op = conn.owner_operation_id.as_deref().unwrap_or("");
                    if conn_op != operation_id {
                        return None;
                    }
                    Some((
                        id.clone(),
                        conn.owner_window_label.clone(),
                        conn.owner_operation_id.clone(),
                        conn.ownership_generation,
                        conn.connection_incarnation.clone(),
                    ))
                })
                .collect()
        };

        let mut disconnected = 0usize;
        for (id, expected_owner, expected_op, expected_gen, expected_incarnation) in candidates {
            // Phase 1: ownership + idle pre-check; capture incarnation while
            // the connection is still in the routing map. Final TOCTOU-safe
            // decision is phase 3 (exclusive write + remove).
            //
            // Do **not** clear/fence tool leases here: if phase 3 later skips
            // (busy / no write lock), a permanent fence would break the
            // surviving connection's watchdog.
            let incarnation = {
                let connections = self.connections.lock().await;
                let Some(conn) = connections.get(&id) else {
                    continue;
                };
                if conn.owner_window_label != expected_owner
                    || conn.owner_operation_id != expected_op
                    || conn.ownership_generation != expected_gen
                    || conn.connection_incarnation != expected_incarnation
                {
                    tracing::info!(
                        "[ACP] idle residual skipped rebinding/changed connection={}",
                        id
                    );
                    continue;
                }
                let Ok(state) = conn.state.try_read() else {
                    continue;
                };
                if !is_idle_for_residual(&state, now) {
                    continue;
                }
                drop(state);
                conn.connection_incarnation.clone()
            };
            // Phase 3: exclusive idle re-check + remove with no gap.
            // Clone the state Arc so the write guard does not borrow the map
            // entry; hold that write lock across remove so Prompting (which
            // needs state.write) cannot race between idle check and map drop.
            // Clear/fence leases only after the exclusive pass decides to remove.
            let removed = {
                let mut connections = self.connections.lock().await;
                let Some(conn) = connections.get(&id) else {
                    continue;
                };
                if conn.owner_window_label != expected_owner
                    || conn.owner_operation_id != expected_op
                    || conn.ownership_generation != expected_gen
                    || conn.connection_incarnation != incarnation
                {
                    continue;
                }
                let state_arc = Arc::clone(&conn.state);
                let Ok(state) = state_arc.try_write() else {
                    continue;
                };
                if !is_idle_for_residual(&state, now) {
                    continue;
                }
                self.record_disconnect_intent(&id, AcpDisconnectOrigin::IdleTimeout);
                // Write lock still held — status cannot become Prompting here.
                let removed = connections.remove(&id);
                drop(state);
                removed
            };
            if let Some(conn) = removed {
                // Map entry is gone — fence admission + clear leases so a
                // still-running loop cannot recreate watchdog state.
                self.clear_tool_leases(&id, &incarnation).await;
                tracing::info!(
                    "[ACP] idle residual disconnecting connection={} owner_window={} op={}",
                    id,
                    owner_window_label,
                    operation_id
                );
                let _ = conn.control_tx.send(ConnectionControl::Disconnect).await;
                disconnected += 1;
            }
        }
        tracing::info!(
            "[ACP] disconnect idle by owner window+op owner_window={} op={} count={}",
            owner_window_label,
            operation_id,
            disconnected
        );
        disconnected
    }

    /// Max `ownership_generation` among connections currently owned by
    /// `(owner_window_label, operation_id)`. Used by close emit harden path:
    /// if residual already moved ownership to `main` while a premature
    /// `ConnectionGone` was committed, upgrade before publishing closed.
    pub async fn max_ownership_generation_for_owner_operation(
        &self,
        owner_window_label: &str,
        operation_id: &str,
    ) -> Option<u64> {
        let connections = self.connections.lock().await;
        let mut max_gen: Option<u64> = None;
        for conn in connections.values() {
            if conn.owner_window_label != owner_window_label {
                continue;
            }
            if conn.owner_operation_id.as_deref() != Some(operation_id) {
                continue;
            }
            max_gen = Some(max_gen.map_or(conn.ownership_generation, |m| {
                m.max(conn.ownership_generation)
            }));
        }
        max_gen
    }

    /// Best-effort residual reverse: rebind **every** connection still matching
    /// `(from_label, operation_id)` to `to_label`, advancing generation and
    /// keeping the operation stamp (v1). No conversation graph / root lookup —
    /// covers late children missed by primary reverse once the root is already
    /// on `main`.
    ///
    /// Returns `(rebound_count, max_ownership_generation)` so close paths can
    /// upgrade a premature `Superseded`/`ReverseUncertain` outcome to
    /// `Reversed { gen }` when the late reverse actually lands.
    pub async fn rebind_stamped_connections_owner_window(
        &self,
        from_label: &str,
        operation_id: &str,
        to_label: &str,
    ) -> (usize, Option<u64>) {
        let to_write: Vec<(u64, Arc<RwLock<SessionState>>)> = {
            let mut connections = self.connections.lock().await;
            let mut to_write = Vec::new();
            for conn in connections.values_mut() {
                if conn.owner_window_label != from_label {
                    continue;
                }
                if conn.owner_operation_id.as_deref() != Some(operation_id) {
                    continue;
                }
                conn.owner_window_label = to_label.to_string();
                conn.ownership_generation = conn.ownership_generation.saturating_add(1).max(1);
                to_write.push((conn.ownership_generation, Arc::clone(&conn.state)));
            }
            to_write
        };
        let mut rebound = 0usize;
        let mut max_gen: Option<u64> = None;
        for (gen, state) in to_write {
            let mut st = state.write().await;
            st.owner_window_label = to_label.to_string();
            rebound += 1;
            max_gen = Some(max_gen.map_or(gen, |m| m.max(gen)));
        }
        if rebound > 0 {
            tracing::info!(
                "[ACP] rebind stamped connections from={} op={} to={} count={} max_gen={:?}",
                from_label,
                operation_id,
                to_label,
                rebound,
                max_gen
            );
        }
        (rebound, max_gen)
    }

    /// Rebind root (by conversation_id / connection_id) and descendants that
    /// share the same prior owner label. Returns rebound count + generation.
    pub async fn rebind_connection_owner_window(
        &self,
        conversation_id: i32,
        connection_id: Option<&str>,
        from_owner_window: &str,
        to_owner_window: &str,
        operation_id: &str,
        expected_generation: Option<u64>,
    ) -> Result<crate::acp::owner_rebind::RebindResult, crate::app_error::AppCommandError> {
        use crate::acp::owner_rebind::RebindResult;
        use crate::app_error::AppCommandError;

        struct RebindRow {
            id: String,
            parent_connection_id: Option<String>,
            owner_window_label: String,
            owner_operation_id: Option<String>,
            ownership_generation: u64,
            connection_incarnation: String,
            origin: crate::acp::delegation::route::DelegationConnectionOrigin,
            state: Arc<tokio::sync::RwLock<SessionState>>,
        }

        struct ExpectedRebindTarget {
            id: String,
            connection_incarnation: String,
            owner_window_label: String,
            owner_operation_id: Option<String>,
            ownership_generation: u64,
        }

        fn rebind_cas_failed(
            kind: &str,
            expected: impl std::fmt::Display,
            have: impl std::fmt::Display,
        ) -> AppCommandError {
            AppCommandError::task_execution_failed(format!(
                "{kind} CAS failed: expected {expected}, have {have}"
            ))
        }

        fn mismatch_expected_rebind_target(
            live: &AgentConnection,
            expected: &ExpectedRebindTarget,
        ) -> Option<AppCommandError> {
            if live.connection_incarnation != expected.connection_incarnation {
                return Some(rebind_cas_failed(
                    "incarnation",
                    &expected.connection_incarnation,
                    &live.connection_incarnation,
                ));
            }
            if live.owner_window_label != expected.owner_window_label {
                return Some(rebind_cas_failed(
                    "owner label",
                    &expected.owner_window_label,
                    &live.owner_window_label,
                ));
            }
            if live.owner_operation_id != expected.owner_operation_id {
                return Some(rebind_cas_failed(
                    "owner operation",
                    expected.owner_operation_id.as_deref().unwrap_or(""),
                    live.owner_operation_id.as_deref().unwrap_or(""),
                ));
            }
            if live.ownership_generation != expected.ownership_generation {
                return Some(rebind_cas_failed(
                    "generation",
                    expected.ownership_generation,
                    live.ownership_generation,
                ));
            }
            None
        }

        let snapshot: Vec<RebindRow> = {
            let connections = self.connections.lock().await;
            connections
                .iter()
                .map(|(id, conn)| RebindRow {
                    id: id.clone(),
                    parent_connection_id: conn.parent_connection_id.clone(),
                    owner_window_label: conn.owner_window_label.clone(),
                    owner_operation_id: conn.owner_operation_id.clone(),
                    ownership_generation: conn.ownership_generation,
                    connection_incarnation: conn.connection_incarnation.clone(),
                    origin: conn.origin,
                    state: Arc::clone(&conn.state),
                })
                .collect()
        };

        #[cfg(test)]
        {
            let hook = self
                .rebind_after_snapshot_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(hook) = hook {
                hook.reached.notify_one();
                hook.resume.notified().await;
            }
        }

        let root_id = if let Some(cid) = connection_id {
            if snapshot.iter().any(|row| row.id == cid) {
                cid.to_string()
            } else {
                return Err(AppCommandError::not_found(format!(
                    "connection {cid} not found"
                )));
            }
        } else {
            let mut found: Option<String> = None;
            for row in &snapshot {
                let state = row.state.read().await;
                if state.conversation_id == Some(conversation_id) {
                    found = Some(row.id.clone());
                    if !matches!(
                        row.origin,
                        crate::acp::delegation::route::DelegationConnectionOrigin::CodegChild
                    ) {
                        break;
                    }
                }
            }
            found.ok_or_else(|| {
                AppCommandError::not_found(format!(
                    "no connection for conversation {conversation_id}"
                ))
            })?
        };

        let root = snapshot
            .iter()
            .find(|row| row.id == root_id)
            .ok_or_else(|| AppCommandError::not_found(format!("connection {root_id} not found")))?;

        let current_label = root.owner_window_label.clone();
        let current_gen = root.ownership_generation;
        let current_op = root.owner_operation_id.clone();
        let root_expected = ExpectedRebindTarget {
            id: root_id.clone(),
            connection_incarnation: root.connection_incarnation.clone(),
            owner_window_label: current_label.clone(),
            owner_operation_id: current_op.clone(),
            ownership_generation: current_gen,
        };

        // Idempotent success is only allowed after re-checking the live root.
        if current_label == to_owner_window && current_op.as_deref() == Some(operation_id) {
            let connections = self.connections.lock().await;
            return match connections.get(&root_id) {
                Some(live) => {
                    if let Some(err) = mismatch_expected_rebind_target(live, &root_expected) {
                        Err(err)
                    } else {
                        Ok(RebindResult {
                            rebound_count: 0,
                            ownership_generation: current_gen,
                            operation_id: operation_id.to_string(),
                        })
                    }
                }
                None => Err(AppCommandError::not_found(format!(
                    "connection {root_id} not found"
                ))),
            };
        }

        if let Some(exp) = expected_generation {
            if current_gen != exp {
                return Err(AppCommandError::task_execution_failed(format!(
                    "generation CAS failed: expected {exp}, have {current_gen}"
                )));
            }
        }

        if current_label != from_owner_window {
            return Err(AppCommandError::task_execution_failed(format!(
                "owner label CAS failed: expected {from_owner_window}, have {current_label}"
            )));
        }

        if !to_owner_window.starts_with("conversation-") {
            let root_op = current_op.as_deref().unwrap_or("");
            if root_op != operation_id {
                return Err(AppCommandError::task_execution_failed(format!(
                    "owner operation CAS failed: expected {operation_id}, have {root_op}"
                )));
            }
        }

        let new_gen = current_gen.saturating_add(1).max(1);
        let prior_label = current_label;

        let mut related_connection_ids = std::collections::HashSet::new();
        related_connection_ids.insert(root_id.clone());
        let mut related_conversation_ids = std::collections::HashSet::new();
        related_conversation_ids.insert(conversation_id);
        if let Some(root_row) = snapshot.iter().find(|row| row.id == root_id) {
            let st = root_row.state.read().await;
            for d in st.active_delegations.values() {
                related_conversation_ids.insert(d.child_conversation_id);
            }
        }

        let mut expanded = true;
        while expanded {
            expanded = false;
            let mut rows = Vec::new();
            for row in &snapshot {
                if related_connection_ids.contains(&row.id) {
                    continue;
                }
                if row.owner_window_label != prior_label {
                    continue;
                }
                let st = row.state.read().await;
                let child_convs: Vec<i32> = st
                    .active_delegations
                    .values()
                    .map(|d| d.child_conversation_id)
                    .collect();
                rows.push((
                    row.id.clone(),
                    row.parent_connection_id.clone(),
                    st.conversation_id,
                    child_convs,
                ));
            }
            for (id, parent_id, conv_id, child_convs) in rows {
                let parent_linked = parent_id
                    .as_ref()
                    .is_some_and(|pid| related_connection_ids.contains(pid));
                let conv_linked =
                    conv_id.is_some_and(|cid| related_conversation_ids.contains(&cid));
                if !(parent_linked || conv_linked) {
                    continue;
                }
                if related_connection_ids.insert(id) {
                    expanded = true;
                }
                for cid in child_convs {
                    if related_conversation_ids.insert(cid) {
                        expanded = true;
                    }
                }
            }
        }

        let expected_targets: Vec<ExpectedRebindTarget> = snapshot
            .iter()
            .filter(|row| related_connection_ids.contains(&row.id))
            .map(|row| ExpectedRebindTarget {
                id: row.id.clone(),
                connection_incarnation: row.connection_incarnation.clone(),
                owner_window_label: row.owner_window_label.clone(),
                owner_operation_id: row.owner_operation_id.clone(),
                ownership_generation: row.ownership_generation,
            })
            .collect();
        let to_write: Vec<Arc<tokio::sync::RwLock<SessionState>>> = {
            let mut connections = self.connections.lock().await;
            for expected in &expected_targets {
                match connections.get(&expected.id) {
                    Some(live) => {
                        if let Some(err) = mismatch_expected_rebind_target(live, expected) {
                            return Err(err);
                        }
                    }
                    None => {
                        return Err(rebind_cas_failed(
                            "incarnation",
                            &expected.connection_incarnation,
                            "<absent>",
                        ));
                    }
                }
            }
            let mut to_write = Vec::new();
            for expected in &expected_targets {
                let Some(conn) = connections.get_mut(&expected.id) else {
                    return Err(rebind_cas_failed(
                        "incarnation",
                        &expected.connection_incarnation,
                        "<absent>",
                    ));
                };
                conn.owner_window_label = to_owner_window.to_string();
                conn.owner_operation_id = Some(operation_id.to_string());
                conn.ownership_generation = new_gen;
                to_write.push(Arc::clone(&conn.state));
            }
            to_write
        };

        let mut rebound = 0usize;
        for state in to_write {
            let mut st = state.write().await;
            st.owner_window_label = to_owner_window.to_string();
            rebound += 1;
        }

        Ok(RebindResult {
            rebound_count: rebound,
            ownership_generation: new_gen,
            operation_id: operation_id.to_string(),
        })
    }

    /// Close manager-wide connection admission. New spawn/connect requests
    /// fail with [`AcpError::ServerShuttingDown`]. Already-issued permits
    /// remain valid until they insert or fail.
    pub fn begin_shutdown(&self) {
        self.admission.close();
    }

    /// Wait until every issued admission permit has been released.
    pub async fn wait_for_admissions(&self) {
        self.admission.close_and_wait().await;
    }

    /// Drain live connections after admission is closed. Loops until admission
    /// is closed, in-flight permits are zero, and the connection map is empty.
    pub async fn drain_for_shutdown(&self, origin: AcpDisconnectOrigin) -> usize {
        self.admission.close();
        let shutdown_diagnostics = if origin == AcpDisconnectOrigin::ApplicationShutdown {
            self.shared_session_broker.begin_shutdown().await;
            self.shared_session_diagnostics().await
        } else {
            Vec::new()
        };
        let mut shutdown_shared_pid_cells = Vec::new();
        for diagnostic in &shutdown_diagnostics {
            if let Some(pid) = self
                .shared_session_broker
                .driver_child_pid_for_connection(&diagnostic.connection_id)
                .await
            {
                shutdown_shared_pid_cells.push(pid);
            }
        }
        let mut disconnected = 0usize;
        let mut pid_cells: Vec<Arc<std::sync::atomic::AtomicU32>> = Vec::new();
        loop {
            let planned: Vec<DisconnectSelection> = {
                let connections = self.connections.lock().await;
                connections
                    .iter()
                    .map(|(id, connection)| {
                        DisconnectSelection::incarnation_only(id.clone(), connection)
                    })
                    .collect()
            };
            let (accepting, in_flight) = self.admission.snapshot();
            if !accepting && in_flight == 0 && planned.is_empty() {
                break;
            }
            if planned.is_empty() {
                self.admission.wait_until_idle().await;
                continue;
            }
            let removed = self.take_connections_for_disconnect(planned, origin).await;
            disconnected += removed.len();
            self.dispatch_taken_disconnects(removed, &mut pid_cells)
                .await;
        }
        tracing::info!("[ACP] drain_for_shutdown count={}", disconnected);
        self.finish_disconnect_backstop(
            origin,
            disconnected,
            pid_cells,
            shutdown_shared_pid_cells,
            shutdown_diagnostics,
        )
        .await;
        disconnected
    }

    #[cfg(test)]
    pub fn admission_in_flight_for_test(&self) -> usize {
        self.admission.snapshot().1
    }

    #[cfg(test)]
    pub fn enable_stub_direct_spawn_for_test(&self) {
        self.stub_direct_spawn
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    async fn hold_after_admission_for_test(&self) {
        let hook = self
            .admission_insert_hold
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(hook) = hook {
            hook.reached.notify_one();
            hook.resume.notified().await;
        }
    }

    async fn dispatch_taken_disconnects(
        &self,
        removed: Vec<(String, crate::acp::connection::AgentConnection)>,
        pid_cells: &mut Vec<Arc<std::sync::atomic::AtomicU32>>,
    ) {
        crate::acp::terminal_runtime::kill_all_registered_acp_terminals().await;
        for (conn_id, conn) in removed {
            let pre_loop = {
                let st = conn.state.read().await;
                matches!(st.status, ConnectionStatus::Connecting)
            };
            if pre_loop {
                if let Some(abort) = conn.task_abort {
                    abort.abort();
                }
                if let Some(inj) = self.delegation_snapshot() {
                    crate::acp::connection::cleanup_delegation_parent(&inj, &conn_id, &conn.state)
                        .await;
                }
                emit_with_state(
                    &conn.state,
                    &conn.emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Disconnected,
                    },
                )
                .await;
            }
            // Shutdown cannot wait for a saturated command lane. A missed
            // graceful signal is covered by the process-tree backstop below.
            let _ = conn.control_tx.try_send(ConnectionControl::Disconnect);
            pid_cells.push(conn.child_pid);
        }
    }

    async fn finish_disconnect_backstop(
        &self,
        origin: AcpDisconnectOrigin,
        disconnected: usize,
        mut pid_cells: Vec<Arc<std::sync::atomic::AtomicU32>>,
        shutdown_shared_pid_cells: Vec<Arc<std::sync::atomic::AtomicU32>>,
        shutdown_diagnostics: Vec<SharedSessionDiagnostic>,
    ) {
        for shared_pid in shutdown_shared_pid_cells {
            if !pid_cells.iter().any(|pid| Arc::ptr_eq(pid, &shared_pid)) {
                pid_cells.push(shared_pid);
            }
        }
        let retained_process = pid_cells
            .iter()
            .any(|pid| pid.load(std::sync::atomic::Ordering::SeqCst) != 0);
        if disconnected != 0 || retained_process {
            tokio::time::sleep(DISCONNECT_ALL_GRACE).await;

            let _ = tokio::task::spawn_blocking(move || {
                for cell in pid_cells {
                    // Load only after the grace window. The process may have
                    // spawned late or its reaper may have cleared this cell.
                    let pid = cell.load(std::sync::atomic::Ordering::SeqCst);
                    if pid == 0 {
                        continue;
                    }
                    match kill_tree::blocking::kill_tree(pid) {
                        Ok(_) => tracing::info!(
                            "[ACP] disconnect_all backstop killed process tree pid={pid}"
                        ),
                        Err(error) => tracing::debug!(
                            "[ACP] disconnect_all backstop kill_tree pid={pid}: {error}"
                        ),
                    }
                }
            })
            .await;
        }

        if origin != AcpDisconnectOrigin::ApplicationShutdown {
            return;
        }
        for diagnostic in shutdown_diagnostics {
            let absent = !self
                .connections
                .lock()
                .await
                .contains_key(&diagnostic.connection_id);
            if absent
                && self
                    .shared_session_broker
                    .remove_shutdown_session(&diagnostic.connection_id, diagnostic.generation)
                    .await
            {
                self.shared_launches
                    .lock()
                    .await
                    .remove(&(diagnostic.connection_id, diagnostic.generation));
            } else if !absent {
                self.shared_session_broker.record_cleanup_incomplete();
                tracing::warn!(
                    connection_id = diagnostic.connection_id,
                    generation = diagnostic.generation,
                    "[ACP] shared shutdown cleanup left a live manager entry"
                );
            }
        }
    }

    /// Disconnect every current connection, then hard-kill any agent process
    /// tree whose live PID remains published after the grace window.
    ///
    /// `take_connections_for_disconnect` preserves the fork's watchdog and
    /// incarnation fences before anything becomes unroutable. PID cells stay
    /// shared with the process callbacks so a late spawn is still reached and
    /// a process reaped during the grace window is skipped.
    pub async fn disconnect_all(&self, origin: AcpDisconnectOrigin) -> usize {
        if origin == AcpDisconnectOrigin::ApplicationShutdown {
            self.begin_shutdown();
            self.wait_for_admissions().await;
            return self.drain_for_shutdown(origin).await;
        }
        let mut disconnected = 0usize;
        let mut pid_cells: Vec<Arc<std::sync::atomic::AtomicU32>> = Vec::new();
        // Two snapshots: a connection inserted during the first take is
        // caught on the second pass.
        for _ in 0..2 {
            let planned: Vec<DisconnectSelection> = {
                let connections = self.connections.lock().await;
                connections
                    .iter()
                    .map(|(id, connection)| {
                        DisconnectSelection::incarnation_only(id.clone(), connection)
                    })
                    .collect()
            };
            if planned.is_empty() {
                break;
            }
            let removed = self.take_connections_for_disconnect(planned, origin).await;
            disconnected += removed.len();
            self.dispatch_taken_disconnects(removed, &mut pid_cells)
                .await;
        }
        tracing::info!("[ACP] disconnect_all count={}", disconnected);
        self.finish_disconnect_backstop(origin, disconnected, pid_cells, Vec::new(), Vec::new())
            .await;
        disconnected
    }

    pub async fn list_connections(&self) -> Vec<ConnectionInfo> {
        let connections = self.connections.lock().await;
        connections.values().map(|c| c.info()).collect()
    }

    /// Raw per-connection rows for the pet panel's active-session list.
    /// "Active" = the connection is currently `Prompting`, awaiting a
    /// permission, or in an `Error` state — the sessions a user would want to
    /// see or act on from the floating pet. Idle `Connected` sessions are
    /// excluded to keep the list focused (mirrors the Codex pet "signal"
    /// model).
    ///
    /// `title` is left empty here: this layer has no DB handle. The command
    /// layer (`pet_list_active_sessions_core`) fills it from the conversation
    /// row. Connections without both a bound `conversation_id` and `folder_id`
    /// are skipped — the panel needs both to render a row and to navigate to
    /// it. Lock discipline mirrors `find_connection_by_conversation_id`: hold
    /// the connections mutex while taking each per-session read lock (the
    /// reads are microseconds and released each iteration).
    pub async fn list_active_sessions(&self) -> Vec<crate::models::pet::PetSessionEntry> {
        let connections = self.connections.lock().await;
        let mut out = Vec::new();
        for (id, conn) in connections.iter() {
            let state = conn.state.read().await;
            let (Some(conversation_id), Some(folder_id)) = (state.conversation_id, state.folder_id)
            else {
                continue;
            };
            let pending = state
                .pending_permission
                .as_ref()
                .map(crate::models::pet::PetPermissionSummary::from);
            let is_active = pending.is_some()
                || matches!(
                    state.status,
                    ConnectionStatus::Prompting | ConnectionStatus::Error
                );
            if !is_active {
                continue;
            }
            out.push(crate::models::pet::PetSessionEntry {
                connection_id: id.clone(),
                conversation_id,
                folder_id,
                agent_type: state.agent_type,
                title: String::new(),
                status: state.status.clone(),
                pending,
                parent: None,
            });
        }
        out
    }

    /// Snapshot `external_id` and subscribe to the private event stream under
    /// one `SessionState` read lock so a concurrent `SessionStarted` cannot
    /// land between the two observations.
    pub async fn identity_and_subscribe(
        &self,
        conn_id: &str,
    ) -> Result<
        (
            Option<String>,
            tokio::sync::broadcast::Receiver<std::sync::Arc<crate::acp::types::EventEnvelope>>,
        ),
        AcpError,
    > {
        let state_arc = self
            .get_state(conn_id)
            .await
            .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
        let state = state_arc.read().await;
        let external_id = state.external_id.clone();
        let rx = state.event_stream().subscribe();
        Ok((external_id, rx))
    }

    /// Clone the `Arc<RwLock<SessionState>>` for a given connection id so the
    /// caller can read/write state without holding the connections mutex.
    /// Returns `None` if no such connection is registered.
    pub async fn get_state(
        &self,
        conn_id: &str,
    ) -> Option<std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>> {
        let state = {
            let connections = self.connections.lock().await;
            connections.get(conn_id).map(|conn| conn.state.clone())
        };
        match state {
            Some(state) => Some(state),
            None => self
                .shared_session_broker
                .public_state_and_emitter(conn_id)
                .await
                .map(|(state, _)| state),
        }
    }

    /// Like `get_state`, but also clones the connection's `EventEmitter`.
    /// Used by the lifecycle subscriber when it needs to both update the
    /// per-session state and re-broadcast a derived event (e.g. emitting
    /// `ConversationStatusChanged` after writing the row's status).
    /// One short lock on the connections map; both pieces are cheap to clone.
    pub async fn get_state_and_emitter(
        &self,
        conn_id: &str,
    ) -> Option<(
        std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
        EventEmitter,
    )> {
        let state = {
            let connections = self.connections.lock().await;
            connections
                .get(conn_id)
                .map(|conn| (conn.state.clone(), conn.emitter.clone()))
        };
        match state {
            Some(state) => Some(state),
            None => {
                self.shared_session_broker
                    .public_state_and_emitter(conn_id)
                    .await
            }
        }
    }

    pub async fn publish_shared_event(
        &self,
        connection_id: &str,
        event: AcpEvent,
    ) -> Result<(), AcpError> {
        let handles = self
            .shared_session_broker()
            .public_state_and_emitter(connection_id)
            .await;
        let (state, emitter) = match handles {
            Some(handles) => handles,
            None => self
                .get_state_and_emitter(connection_id)
                .await
                .ok_or_else(|| AcpError::ConnectionNotFound(connection_id.into()))?,
        };
        emit_with_state(&state, &emitter, event).await;
        Ok(())
    }

    /// Wait (bounded) for the connected agent to advertise prompt capabilities.
    ///
    /// `None` means the wait ran out or the connection went away; callers treat
    /// that as "no information" rather than an error.
    pub async fn wait_for_prompt_capabilities(
        &self,
        conn_id: &str,
        timeout: Duration,
    ) -> Option<PromptCapabilitiesInfo> {
        let state = {
            let connections = self.connections.lock().await;
            connections.get(conn_id)?.state.clone()
        };
        let start = std::time::Instant::now();
        loop {
            {
                let s = state.read().await;
                if let Some(caps) = s.prompt_capabilities.clone() {
                    return Some(caps);
                }
                if matches!(
                    s.status,
                    ConnectionStatus::Disconnected | ConnectionStatus::Error
                ) {
                    return None;
                }
            }
            if start.elapsed() >= timeout {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Append a live-feedback note to a connection's session and broadcast it.
    ///
    /// Validation: the text is trimmed and rejected when empty
    /// ([`AcpError::InvalidFeedback`]) or longer than [`MAX_FEEDBACK_CHARS`] —
    /// the full text rides in the broadcast event, the snapshot, and the MCP
    /// response, so a sanity bound keeps one pathological note from bloating
    /// them. (There is deliberately no per-turn COUNT cap: the set is cleared
    /// every turn, so its size scales with human typing, not unboundedly.)
    ///
    /// Rejected with [`AcpError::NoActiveTurn`] unless a turn is in flight —
    /// feedback is mid-turn steering, pulled by the agent via the
    /// `check_user_feedback` MCP tool; with no active turn there is nothing to
    /// steer and the note would strand (the frontend falls back to an ordinary
    /// prompt). The append rides `emit_with_state` so `SessionState.feedback`,
    /// the ring buffer, and every attached client stay in lockstep.
    pub async fn submit_feedback(
        &self,
        conn_id: &str,
        text: String,
    ) -> Result<FeedbackItem, AcpError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(AcpError::InvalidFeedback("empty note".into()));
        }
        if trimmed.chars().count() > MAX_FEEDBACK_CHARS {
            return Err(AcpError::InvalidFeedback(format!(
                "note exceeds {MAX_FEEDBACK_CHARS} characters"
            )));
        }
        let text = trimmed.to_string();
        let (state, emitter) = self
            .get_state_and_emitter(conn_id)
            .await
            .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.to_string()))?;
        // Per-connection capability gate: reject if THIS agent never got the
        // `check_user_feedback` tool (e.g. its session started before the feature
        // was enabled) — the note could never be read. `feedback_tool_available`
        // is fixed at launch, so a plain read is race-free.
        if !state.read().await.feedback_tool_available {
            return Err(AcpError::FeedbackDisabled);
        }
        let item =
            FeedbackItem::new_pending(uuid::Uuid::new_v4().to_string(), text, chrono::Utc::now());
        // Gate on `turn_in_flight` and append in ONE critical section (via the
        // gated emit): a `TurnComplete` (flips the flag) or `UserMessage`
        // (clears `feedback`) can't slip between the gate and the append+seq, so
        // a note is never stranded on a finished turn nor re-added to a new one.
        let applied = emit_with_state_gated(
            &state,
            &emitter,
            AcpEvent::FeedbackSubmitted { item: item.clone() },
            |s| s.turn_in_flight,
        )
        .await;
        if !applied {
            return Err(AcpError::NoActiveTurn);
        }
        Ok(item)
    }

    /// Read the pending feedback for a connection WITHOUT marking it delivered.
    /// Returns an immediate snapshot. Read-only — backs the READ half of the
    /// `check_user_feedback` round-trip so the listener can commit delivery only
    /// after the response is actually written (a dropped / failed write leaves
    /// the notes pending for the agent's next check).
    pub async fn read_pending_feedback(&self, conn_id: &str) -> Vec<PendingFeedback> {
        let Some(state) = self.get_state(conn_id).await else {
            return Vec::new();
        };
        let pending: Vec<PendingFeedback> = {
            let s = state.read().await;
            s.feedback
                .iter()
                .filter(|f| f.status == FeedbackStatus::Pending)
                .map(|f| PendingFeedback {
                    id: f.id.clone(),
                    text: f.text.clone(),
                    created_at: f.created_at,
                })
                .collect()
        };
        bounded_feedback_batch(pending, MAX_FEEDBACK_RESPONSE_BYTES)
    }

    /// Mark the named notes `Delivered` and broadcast the consumption. Called by
    /// the listener ONLY after the `check_user_feedback` response was written to
    /// the companion, so a dropped / failed write leaves the notes pending and
    /// the agent's next check re-delivers them (at-least-once).
    ///
    /// Delivery boundary: "delivered" means the response reached the agent's MCP
    /// companion over the UDS. The one remaining hop (companion → agent stdout)
    /// can only fail when the agent process is gone/closing — i.e. the turn is
    /// being torn down, at which point the note is moot (the agent won't act on
    /// it). A mid-wait cancel is already handled upstream by the listener's
    /// peer-close race (no commit), and a cancel after the round-trip completes
    /// cannot suppress the response (the companion's inflight entry is already
    /// consumed). So this is the right boundary for a best-effort steering
    /// side-channel; an end-to-end ack would only cover the moot teardown tail.
    ///
    /// The mark happens under a single write lock; only notes still `Pending`
    /// flip (idempotent — a repeated commit, or a note already consumed by a
    /// racing call, is skipped) and only the ids actually flipped are emitted,
    /// so a double-commit can't double-broadcast.
    pub async fn commit_feedback_delivered(&self, conn_id: &str, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await else {
            return;
        };
        let id_set: std::collections::HashSet<&String> = ids.iter().collect();
        let delivered_at = chrono::Utc::now();
        let marked: Vec<String> = {
            let mut s = state.write().await;
            let mut marked = Vec::new();
            for f in s.feedback.iter_mut() {
                if f.status == FeedbackStatus::Pending && id_set.contains(&f.id) {
                    f.status = FeedbackStatus::Delivered;
                    f.delivered_at = Some(delivered_at);
                    marked.push(f.id.clone());
                }
            }
            marked
        };
        if !marked.is_empty() {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::FeedbackConsumed {
                    ids: marked,
                    delivered_at,
                },
            )
            .await;
        }
    }

    /// Register a blocking `ask_user_question` on a connection: park a one-shot
    /// in `pending_questions` keyed by a fresh `question_id`, broadcast the
    /// `QuestionRequest` (so every attached client renders the interactive card
    /// and a mid-turn attach recovers it from the snapshot), and hand the
    /// receiver back to the listener to await. `None` when the connection is
    /// gone (nothing to ask) OR when this connection already has a pending ask
    /// — see below.
    ///
    /// One pending ask per connection: `SessionState.pending_question` and the
    /// frontend card are single slots, so a second concurrent ask would
    /// overwrite the first's card/snapshot and orphan the first (still-parked)
    /// tool call with no way to answer it. A single agent is blocked in its
    /// `ask_user_question` call and cannot issue a second, so this only guards a
    /// parallel / misbehaving MCP client; the refused second call resolves as
    /// `declined` (the listener's None path) so its agent proceeds with its own
    /// judgment instead of hanging. The check + insert are atomic under the
    /// registry lock.
    pub async fn register_question(
        &self,
        conn_id: &str,
        questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion> {
        self.register_question_inner(conn_id, questions, None)
            .await
            .ok()
    }

    pub async fn register_recovery_question(
        &self,
        parent_conversation_id: i32,
        authorization_id: String,
        questions: Vec<QuestionSpec>,
    ) -> Result<RegisteredQuestion, RecoveryQuestionRegistrationError> {
        if self.recovery_authorization_service().is_none()
            || questions.len() != 1
            || questions[0].recovery().is_none()
        {
            return Err(RecoveryQuestionRegistrationError::Invalid);
        }
        let states: Vec<_> = self
            .connections
            .lock()
            .await
            .iter()
            .map(|(id, connection)| (id.clone(), connection.state.clone()))
            .collect();
        let mut parent_connection_id = None;
        for (connection_id, state) in states {
            if state.read().await.conversation_id == Some(parent_conversation_id) {
                parent_connection_id = Some(connection_id);
                break;
            }
        }
        let Some(parent_connection_id) = parent_connection_id else {
            return Err(RecoveryQuestionRegistrationError::ParentUnavailable);
        };
        self.register_question_inner(&parent_connection_id, questions, Some(authorization_id))
            .await
    }

    async fn register_question_inner(
        &self,
        conn_id: &str,
        questions: Vec<QuestionSpec>,
        recovery_authorization_id: Option<String>,
    ) -> Result<RegisteredQuestion, RecoveryQuestionRegistrationError> {
        // Defense-in-depth: the companion validates, but the broker socket is
        // only token-gated, so refuse to broadcast malformed/oversized specs
        // (None → the listener declines the ask, as for any other None path).
        if crate::acp::question::validate_specs(&questions).is_err() {
            return Err(RecoveryQuestionRegistrationError::Invalid);
        }
        let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await else {
            return Err(RecoveryQuestionRegistrationError::ParentUnavailable);
        };
        let question_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut reg = self.pending_questions.lock().await;
            if reg.values().any(|e| e.parent_connection_id == conn_id) {
                return Err(RecoveryQuestionRegistrationError::Occupied);
            }
            reg.insert(
                question_id.clone(),
                PendingQuestionEntry {
                    parent_connection_id: conn_id.to_string(),
                    questions: questions.clone(),
                    sender: tx,
                    recovery_authorization_id,
                    settling: false,
                },
            );
        }
        // Ungated emit: the agent is blocked in the tool call, so the card must
        // show regardless of any turn-flag timing.
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::QuestionRequest {
                question_id: question_id.clone(),
                questions: questions.clone(),
            },
        )
        .await;
        // Pause tool-watchdog leases while a structured agent question is open.
        // Demoting Warning/Grace must publish Cleared so attach maps drop them.
        {
            let s = state.read().await;
            if let Some(turn) = s.tool_watchdog_turn_stamp() {
                let attr = s.lease_attribution();
                drop(s);
                let cleared = attr.pause_question(&turn).await;
                for projection in cleared {
                    emit_with_state(
                        &state,
                        &emitter,
                        AcpEvent::ToolWatchdogChanged { projection },
                    )
                    .await;
                }
            }
        }
        // Teardown event-ordering race: `cancel_questions_by_parent` may have
        // drained this entry between the insert above and the emit just now. The
        // QuestionRequest we broadcast would then have no waiter, and the sweep's
        // QuestionResolved may have raced ahead of it — leaving a card up with no
        // live backend waiter. Emit a compensating QuestionResolved (ordered after
        // our QuestionRequest) and decline. (The listener's post-register token
        // re-check covers the complementary case: a register that lands entirely
        // after the sweep, which this presence check would not catch.)
        if self
            .compensate_if_question_drained(&question_id, &state, &emitter)
            .await
        {
            return Err(RecoveryQuestionRegistrationError::ParentUnavailable);
        }
        Ok(RegisteredQuestion {
            question_id,
            answer_rx: rx,
        })
    }

    /// Returns `true` — after emitting a clearing `QuestionResolved` — when
    /// `question_id` is no longer pending, i.e. a teardown sweep drained it in the
    /// window after its `QuestionRequest` was broadcast. The compensating event is
    /// ordered after the request so no client keeps a card with no live backend
    /// waiter. Returns `false` (no emit) while the entry is still parked.
    async fn compensate_if_question_drained(
        &self,
        question_id: &str,
        state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
        emitter: &EventEmitter,
    ) -> bool {
        if self
            .pending_questions
            .lock()
            .await
            .contains_key(question_id)
        {
            return false;
        }
        emit_with_state(
            state,
            emitter,
            AcpEvent::QuestionResolved {
                question_id: question_id.to_string(),
            },
        )
        .await;
        true
    }

    /// Look up the connection that owns a pending `question_id` without
    /// consuming the entry. Admission guards must use this authoritative owner
    /// (not the caller-supplied `connection_id`) because [`Self::answer_question`]
    /// routes by `question_id` and ignores the caller connection.
    pub async fn pending_question_parent_connection_id(&self, question_id: &str) -> Option<String> {
        self.pending_questions
            .lock()
            .await
            .get(question_id)
            .map(|entry| entry.parent_connection_id.clone())
    }

    #[cfg(test)]
    pub async fn pending_question_count_for_parent(&self, conn_id: &str) -> usize {
        self.pending_questions
            .lock()
            .await
            .values()
            .filter(|entry| entry.parent_connection_id == conn_id)
            .count()
    }

    /// Resolve a pending `ask_user_question` with the user's submission (from any
    /// client). Claims the entry atomically, persists recovery decisions before
    /// removal, sends the self-describing outcome to the blocked listener, and
    /// broadcasts `QuestionResolved` so the card clears on every client. Routing uses the entry's stored parent
    /// connection (the `question_id` is the authoritative key), so a stale
    /// `conn_id` from the caller can't misroute.
    ///
    /// Callers that enforce viewer-only admission must guard the owner returned
    /// by [`Self::pending_question_parent_connection_id`] **before** calling this
    /// (peek → guard → answer) so a rejected answer never consumes the pending
    /// entry.
    pub async fn answer_question(
        &self,
        conn_id: &str,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<(), AcpError> {
        match self
            .answer_question_with_admission(conn_id, question_id, answer)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => error.into_local_result(),
        }
    }

    async fn answer_question_with_admission(
        &self,
        conn_id: &str,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<(), SharedControlAdmissionError> {
        let _ = conn_id;
        let (questions, authorization_id) = match self.claim_question_settlement(question_id).await
        {
            QuestionSettlementClaim::Missing => {
                return Err(SharedControlAdmissionError::InteractionAlreadyResolved {
                    local_error: None,
                })
            }
            QuestionSettlementClaim::InFlight => {
                return Err(SharedControlAdmissionError::InteractionAlreadyResolved {
                    local_error: Some(AcpError::protocol(
                        "question settlement is already in progress",
                    )),
                })
            }
            QuestionSettlementClaim::Claimed {
                questions,
                recovery_authorization_id,
            } => (questions, recovery_authorization_id),
        };
        let outcome = build_outcome(&questions, &answer);
        let Some(authorization_id) = authorization_id else {
            self.finish_question_settlement(question_id, Some(outcome))
                .await;
            return Ok(());
        };
        let Some(service) = self.recovery_authorization_service() else {
            self.release_question_settlement(question_id).await;
            return Err(SharedControlAdmissionError::DefinitelyNotAdmitted(
                AcpError::protocol("recovery authorization service is unavailable"),
            ));
        };
        let manager = self.clone_ref();
        let question_id = question_id.to_string();
        let admission = tokio::spawn(async move {
            if let Err(error) = service
                .resolve_question(&authorization_id, outcome.clone())
                .await
            {
                manager.release_question_settlement(&question_id).await;
                return Err(AcpError::protocol(error.to_string()));
            }
            manager
                .finish_question_settlement(&question_id, Some(outcome))
                .await;
            Ok(())
        })
        .await;
        match admission {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(SharedControlAdmissionError::DefinitelyNotAdmitted(error)),
            Err(error) => Err(SharedControlAdmissionError::MayHaveBeenAdmitted(
                AcpError::protocol(error.to_string()),
            )),
        }
    }

    /// Cancel a pending `ask_user_question` — the companion's tool call was
    /// canceled (peer-close) or explicitly dismissed. Recovery decisions are
    /// persisted before the one-shot is removed and the card is cleared. No-op
    /// if the question was already answered / gone.
    pub async fn cancel_question(&self, conn_id: &str, question_id: &str) {
        let _ = conn_id;
        let authorization_id = match self.claim_question_settlement(question_id).await {
            QuestionSettlementClaim::Missing | QuestionSettlementClaim::InFlight => return,
            QuestionSettlementClaim::Claimed {
                recovery_authorization_id,
                ..
            } => recovery_authorization_id,
        };
        let Some(authorization_id) = authorization_id else {
            self.finish_question_settlement(question_id, None).await;
            return;
        };
        let Some(service) = self.recovery_authorization_service() else {
            self.release_question_settlement(question_id).await;
            return;
        };
        let manager = self.clone_ref();
        let question_id = question_id.to_string();
        let _ = tokio::spawn(async move {
            if service
                .resolve_question(
                    &authorization_id,
                    QuestionOutcome {
                        answers: Vec::new(),
                        declined: true,
                    },
                )
                .await
                .is_err()
            {
                manager.release_question_settlement(&question_id).await;
                return;
            }
            manager.finish_question_settlement(&question_id, None).await;
        })
        .await;
    }

    /// Cancel every pending `ask_user_question` parked on a connection that is
    /// tearing down. The `run_connection` cleanup guard calls this (alongside
    /// the delegation `DelegationBroker::cancel_by_parent` cascade) so question
    /// entries and listener tasks are reclaimed without depending on the
    /// companion's ask socket. Recovery abandonment runs in an owned retry task
    /// so connection teardown does not block on database recovery. No-op when
    /// nothing is pending for this parent.
    pub async fn cancel_questions_by_parent(&self, conn_id: &str) {
        let ids: Vec<String> = self
            .pending_questions
            .lock()
            .await
            .iter()
            .filter(|(_, e)| e.parent_connection_id == conn_id)
            .map(|(id, _)| id.clone())
            .collect();
        for question_id in ids {
            let manager = self.clone_ref();
            tokio::spawn(async move {
                manager
                    .abandon_question_for_parent_teardown(question_id)
                    .await;
            });
        }
    }

    async fn abandon_question_for_parent_teardown(&self, question_id: String) {
        if let Err(error) = self
            .settle_recovery_question_abandonment(&question_id, None)
            .await
        {
            tracing::error!(
                code = error.code(),
                "[recovery_authorization] parent teardown abandonment stopped"
            );
        }
    }

    pub(crate) async fn abandon_recovery_question_until_terminal(
        &self,
        authorization_id: &str,
        question_id: &str,
    ) -> Result<(), crate::acp::recovery_authorization::RecoveryAuthorizationError> {
        self.settle_recovery_question_abandonment(question_id, Some(authorization_id))
            .await
    }

    async fn settle_recovery_question_abandonment(
        &self,
        question_id: &str,
        expected_authorization_id: Option<&str>,
    ) -> Result<(), crate::acp::recovery_authorization::RecoveryAuthorizationError> {
        use crate::acp::recovery_authorization::{
            recovery_cleanup_retry_delay, RecoveryAuthorizationError,
        };

        let mut claim_waits = 0_u32;
        let authorization_id = loop {
            match self.claim_question_settlement(question_id).await {
                QuestionSettlementClaim::Missing => {
                    let Some(expected_authorization_id) = expected_authorization_id else {
                        return Ok(());
                    };
                    let Some(service) = self.recovery_authorization_service() else {
                        return Err(RecoveryAuthorizationError::ChallengeConflict);
                    };
                    return service
                        .abandon_until_terminal(expected_authorization_id, Some(question_id))
                        .await;
                }
                QuestionSettlementClaim::InFlight => {
                    let delay = recovery_cleanup_retry_delay(claim_waits);
                    claim_waits = claim_waits.saturating_add(1);
                    tokio::time::sleep(delay).await;
                }
                QuestionSettlementClaim::Claimed {
                    recovery_authorization_id,
                    ..
                } => {
                    let Some(authorization_id) = recovery_authorization_id else {
                        self.finish_question_settlement(question_id, None).await;
                        return Ok(());
                    };
                    if expected_authorization_id
                        .is_some_and(|expected| expected != authorization_id)
                    {
                        self.release_question_settlement(question_id).await;
                        return Err(RecoveryAuthorizationError::QuestionBindingConflict);
                    }
                    break authorization_id;
                }
            }
        };
        let Some(service) = self.recovery_authorization_service() else {
            self.release_question_settlement(question_id).await;
            return Err(RecoveryAuthorizationError::ChallengeConflict);
        };
        if let Err(error) = service
            .abandon_until_terminal(&authorization_id, Some(question_id))
            .await
        {
            self.release_question_settlement(question_id).await;
            return Err(error);
        }
        self.finish_question_settlement(question_id, None).await;
        Ok(())
    }

    async fn claim_question_settlement(&self, question_id: &str) -> QuestionSettlementClaim {
        let mut pending = self.pending_questions.lock().await;
        let Some(entry) = pending.get_mut(question_id) else {
            return QuestionSettlementClaim::Missing;
        };
        if entry.settling {
            return QuestionSettlementClaim::InFlight;
        }
        entry.settling = true;
        QuestionSettlementClaim::Claimed {
            questions: entry.questions.clone(),
            recovery_authorization_id: entry.recovery_authorization_id.clone(),
        }
    }

    async fn release_question_settlement(&self, question_id: &str) {
        if let Some(entry) = self.pending_questions.lock().await.get_mut(question_id) {
            entry.settling = false;
        }
    }

    pub(crate) async fn finish_question_settlement(
        &self,
        question_id: &str,
        outcome: Option<QuestionOutcome>,
    ) {
        let entry = self.pending_questions.lock().await.remove(question_id);
        let Some(entry) = entry else {
            return;
        };
        if let Some(outcome) = outcome {
            let _ = entry.sender.send(outcome);
        }
        if let Some((state, emitter)) = self
            .get_state_and_emitter(&entry.parent_connection_id)
            .await
        {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::QuestionResolved {
                    question_id: question_id.to_string(),
                },
            )
            .await;
            tool_watchdog_resume_from_state(&state).await;
        }
    }

    /// Register a pending Grok `exit_plan_mode` approval on `conn_id`, broadcast
    /// `PlanApprovalRequest` (so every attached client renders the card and a
    /// mid-turn attach recovers it from the snapshot), and return the receiver
    /// the connection's ext handler awaits. `None` when the connection is gone
    /// (nothing to approve) OR when one is already pending on it (the agent is
    /// blocked in a single `exit_plan_mode` call; a second would orphan the
    /// first). Mirrors [`Self::register_question`].
    pub async fn register_plan_approval(
        &self,
        conn_id: &str,
        tool_call_id: String,
        plan_markdown: String,
    ) -> Option<RegisteredPlanApproval> {
        let (state, emitter) = self.get_state_and_emitter(conn_id).await?;
        let approval_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut reg = self.pending_plan_approvals.lock().await;
            if reg.values().any(|e| e.parent_connection_id == conn_id) {
                return None;
            }
            reg.insert(
                approval_id.clone(),
                PendingPlanApprovalEntry {
                    parent_connection_id: conn_id.to_string(),
                    sender: tx,
                },
            );
        }
        // Ungated emit: the agent is blocked in the tool call, so the card must
        // show regardless of any turn-flag timing.
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::PlanApprovalRequest {
                approval_id: approval_id.clone(),
                tool_call_id,
                plan_markdown,
            },
        )
        .await;
        // Teardown event-ordering race (mirrors `register_question`): the
        // cleanup guard's `cancel_plan_approvals_by_parent` may have drained this
        // entry between the insert above and the emit just now — its
        // `PlanApprovalResolved` could then have raced ahead of our
        // `PlanApprovalRequest`, leaving a card up with no live backend waiter.
        // Emit a compensating `PlanApprovalResolved` (ordered after our request)
        // and decline.
        if self
            .compensate_if_plan_approval_drained(&approval_id, &state, &emitter)
            .await
        {
            return None;
        }
        Some(RegisteredPlanApproval {
            approval_id,
            answer_rx: rx,
        })
    }

    pub async fn pending_plan_approval_parent_connection_id(
        &self,
        approval_id: &str,
    ) -> Option<String> {
        self.pending_plan_approvals
            .lock()
            .await
            .get(approval_id)
            .map(|entry| entry.parent_connection_id.clone())
    }

    /// Returns `true` — after emitting a clearing `PlanApprovalResolved` — when
    /// `approval_id` is no longer pending, i.e. a teardown sweep drained it in the
    /// window after its `PlanApprovalRequest` was broadcast. Mirrors
    /// [`Self::compensate_if_question_drained`].
    async fn compensate_if_plan_approval_drained(
        &self,
        approval_id: &str,
        state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
        emitter: &EventEmitter,
    ) -> bool {
        if self
            .pending_plan_approvals
            .lock()
            .await
            .contains_key(approval_id)
        {
            return false;
        }
        emit_with_state(
            state,
            emitter,
            AcpEvent::PlanApprovalResolved {
                approval_id: approval_id.to_string(),
            },
        )
        .await;
        true
    }

    /// Resolve a pending plan approval with the user's decision (from any
    /// client). Removes the one-shot atomically (first answer wins; a duplicate /
    /// already-resolved id is an idempotent no-op), sends the decision to the
    /// blocked ext handler, and broadcasts `PlanApprovalResolved` so the card
    /// clears on every client. Routing uses the entry's stored parent connection
    /// (the `approval_id` is the authoritative key), so a stale `conn_id` from the
    /// caller can't misroute. Mirrors [`Self::answer_question`].
    pub async fn answer_plan_approval(
        &self,
        conn_id: &str,
        approval_id: &str,
        answer: PlanApprovalAnswer,
    ) -> Result<(), AcpError> {
        match self
            .answer_plan_approval_with_admission(conn_id, approval_id, answer)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => error.into_local_result(),
        }
    }

    async fn answer_plan_approval_with_admission(
        &self,
        conn_id: &str,
        approval_id: &str,
        answer: PlanApprovalAnswer,
    ) -> Result<(), SharedControlAdmissionError> {
        let _ = conn_id;
        let entry = self.pending_plan_approvals.lock().await.remove(approval_id);
        let Some(entry) = entry else {
            // Already answered / canceled / gone elsewhere — idempotent success.
            return Err(SharedControlAdmissionError::InteractionAlreadyResolved {
                local_error: None,
            });
        };
        // Ignore a dropped receiver: the handler may have abandoned the wait
        // (teardown) at the same instant; the resolved event below still clears
        // the card.
        let _ = entry.sender.send(answer);
        if let Some((state, emitter)) = self
            .get_state_and_emitter(&entry.parent_connection_id)
            .await
        {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::PlanApprovalResolved {
                    approval_id: approval_id.to_string(),
                },
            )
            .await;
        }
        Ok(())
    }

    /// Cancel every pending plan approval parked on a connection that is tearing
    /// down (the `run_connection` cleanup guard calls this). Dropping each
    /// entry's sender resolves the handler's await as a disconnect (Grok keeps
    /// plan mode active, re-surfaces on reconnect); the `PlanApprovalResolved`
    /// broadcast clears the card. Mirrors [`Self::cancel_questions_by_parent`].
    pub async fn cancel_plan_approvals_by_parent(&self, conn_id: &str) {
        let drained: Vec<String> = {
            let mut reg = self.pending_plan_approvals.lock().await;
            let ids: Vec<String> = reg
                .iter()
                .filter(|(_, e)| e.parent_connection_id == conn_id)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                reg.remove(id);
            }
            ids
        };
        if drained.is_empty() {
            return;
        }
        // Best-effort card clear: the connection may already be out of the map
        // (disconnect removes it before this sweep), so tolerate `None`.
        if let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await {
            for approval_id in drained {
                emit_with_state(
                    &state,
                    &emitter,
                    AcpEvent::PlanApprovalResolved { approval_id },
                )
                .await;
            }
        }
    }

    /// Resolve a conversation_id to its currently-active connection id, if any.
    /// Used by the by-conversation snapshot endpoint and the LifecycleSubscriber.
    /// Per-session state is acquired via `read().await` to avoid the
    /// `try_read`-skip false negative that would intermittently return None
    /// while `emit_with_state` is mid-update — the wait is microseconds.
    pub async fn find_connection_by_conversation_id(&self, conversation_id: i32) -> Option<String> {
        let connections = self.connections.lock().await;
        for (id, conn) in connections.iter() {
            let state = conn.state.read().await;
            if state.conversation_id == Some(conversation_id) {
                return Some(id.clone());
            }
        }
        None
    }

    /// The in-flight user prompt for `conversation_id` and the instant its turn
    /// started, if a turn is currently running on its live connection. `Some`
    /// exactly between `UserMessage` and `TurnComplete` (see
    /// `SessionState.pending_user_message` / `pending_user_message_started_at`);
    /// `None` when no connection is bound to the conversation or no turn is in
    /// flight.
    ///
    /// Used by the detail endpoint to stamp the persisted in-flight user turn
    /// with the broadcast `message_id`, so a cross-client viewer's synthesized
    /// turn (keyed by that same id) dedups against it instead of rendering a
    /// second copy. The start instant lets the matcher tell the in-flight prompt
    /// apart from a prior identical one. One lock pass over the connections map,
    /// mirroring `find_connection_by_conversation_id`.
    pub async fn pending_user_message_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Option<(
        crate::acp::session_state::PendingUserMessage,
        Option<chrono::DateTime<chrono::Utc>>,
    )> {
        let connections = self.connections.lock().await;
        for conn in connections.values() {
            let state = conn.state.read().await;
            if state.conversation_id == Some(conversation_id) {
                return state
                    .pending_user_message
                    .clone()
                    .map(|pending| (pending, state.pending_user_message_started_at));
            }
        }
        None
    }

    /// Resolve an `(external_id, agent_type)` (agent session) to its
    /// currently-active connection id, if any. Sibling to
    /// `find_connection_by_conversation_id`, used as the discovery fallback for
    /// the cross-client viewer attach: a connection binds its `conversation_id`
    /// only on the first prompt, but its `external_id` is set as soon as the
    /// session starts — so for a historical conversation opened by a second
    /// client *before* anyone has sent a prompt, the by-conversation lookup
    /// misses while this one still finds the live owner, letting the second
    /// client attach as a viewer instead of reusing the connection as a
    /// (mis-tagged) owner and later tearing it down.
    ///
    /// `agent_type` is part of the match because `external_id` is unique only
    /// per agent (`UNIQUE(external_id, agent_type)`), not globally — without it,
    /// a session id shared across two agents could attach a viewer to the wrong
    /// agent's connection.
    pub async fn find_connection_by_external_id(
        &self,
        external_id: &str,
        agent_type: AgentType,
    ) -> Option<String> {
        let connections = self.connections.lock().await;
        for (id, conn) in connections.iter() {
            if conn.agent_type != agent_type {
                continue;
            }
            let state = conn.state.read().await;
            if state.external_id.as_deref() == Some(external_id) {
                return Some(id.clone());
            }
        }
        None
    }

    /// Collect **all** live connection ids bound to a conversation identity.
    ///
    /// Single map lock; never first-hit only. Scan rules:
    /// 1. Include every connection whose `state.conversation_id == Some(conversation_id)`
    ///    **without** filtering on `agent_type` (resolver validates identity).
    /// 2. Also include connections whose `(external_id, agent_type)` match when
    ///    `external_id` is `Some` (compatible external binding).
    ///
    /// Used by delegated-child access projection for multi-candidate parent
    /// live-turn resolution (any in-flight candidate locks; identity conflicts
    /// fail closed).
    pub async fn find_all_connections_for_conversation_identity(
        &self,
        conversation_id: i32,
        external_id: Option<&str>,
        agent_type: AgentType,
    ) -> Vec<String> {
        let connections = self.connections.lock().await;
        let mut ids = Vec::new();
        for (id, conn) in connections.iter() {
            let state = conn.state.read().await;
            let by_conversation = state.conversation_id == Some(conversation_id);
            let by_external = match external_id {
                Some(ext) if !ext.is_empty() => {
                    conn.agent_type == agent_type && state.external_id.as_deref() == Some(ext)
                }
                _ => false,
            };
            if by_conversation || by_external {
                ids.push(id.clone());
            }
        }
        ids
    }

    /// Batch-snapshot raw visible partial assistant text for conversation ids.
    ///
    /// Single pass over the connection map: clone `(connection_id, state Arc)`
    /// under the map lock, **drop the map lock**, then `state.read()`. Never
    /// uses [`Self::find_connection_by_conversation_id`] (which holds the map
    /// lock across `state.read`).
    ///
    /// When multiple connections share a conversation id:
    /// 1. Prefer `live_message.is_some()`
    /// 2. Max `live_message.started_at`
    /// 3. Tie-break connection id ascending
    ///
    /// Values are `visible_assistant_text` (raw; caller applies `bound_context`).
    /// Conversations with no matching connection are omitted (promote treats
    /// missing keys as `""`).
    pub async fn snapshot_partial_assistant_text_for_conversations(
        &self,
        conversation_ids: &[i32],
    ) -> HashMap<i32, String> {
        use crate::acp::session_state::visible_assistant_text;
        use crate::auto_title::partial_source::{fold_partial_candidates, PartialCandidate};
        use std::collections::HashSet;

        if conversation_ids.is_empty() {
            return HashMap::new();
        }
        let wanted: HashSet<i32> = conversation_ids.iter().copied().collect();

        // REQUIRED lock pattern: clone Arcs under the map lock; drop before
        // any state.read().await. AgentConnection is not Clone.
        let handles: Vec<(String, Arc<tokio::sync::RwLock<crate::acp::SessionState>>)> = {
            let guard = self.connections.lock().await;
            guard
                .iter()
                .map(|(id, conn)| (id.clone(), conn.state.clone()))
                .collect()
        }; // map MutexGuard dropped here

        let mut by_conversation: HashMap<i32, Vec<PartialCandidate>> = HashMap::new();
        for (conn_id, state) in handles {
            let s = state.read().await;
            let Some(cid) = s.conversation_id else {
                continue;
            };
            if !wanted.contains(&cid) {
                continue;
            }
            by_conversation
                .entry(cid)
                .or_default()
                .push(PartialCandidate {
                    connection_id: conn_id,
                    has_live: s.live_message.is_some(),
                    started_at: s.live_message.as_ref().map(|m| m.started_at),
                    text: visible_assistant_text(s.live_message.as_ref()),
                });
        }

        fold_partial_candidates(by_conversation)
    }
}

/// Production impl of `ConnectionSpawner` used by `DelegationBroker`.
///
/// Bundles `Arc<ConnectionManager>` with `Arc<AppDatabase>` because
/// `cancel` writes the cancelled status onto the conversation row, which
/// happens inside `ConnectionManager::cancel`. The wrapper exists so the
/// broker can depend on a small `dyn`-able interface instead of pulling
/// in the full `AppState` graph.
///
/// `data_dir` is required so `spawn` can build a runtime env that
/// includes the git credential helper — without it, delegated subagents
/// fail any git command that depends on the codeg-injected helper.
#[derive(Clone)]
pub struct ConnectionManagerSpawner {
    pub manager: Arc<ConnectionManager>,
    pub db: Arc<AppDatabase>,
    pub data_dir: Arc<PathBuf>,
    pub runtime: crate::commands::delegation::DelegationRuntimeSettings,
}

/// Coherent parent snapshot for delegated child launch. Owned by
/// `ConnectionManagerSpawner` and consumed only by production `spawn` (and the
/// named inheritance test that pins that path without spawning an agent).
///
/// Ownership fields are a best-effort snapshot for logging / provisional
/// launch args; the authoritative stamp is re-read under the connections lock
/// at child registration (`resolve_spawn_ownership_under_lock`).
struct ParentSpawnLaunchSnapshot {
    emitter: EventEmitter,
    owner_window_label: String,
    owner_operation_id: Option<String>,
    /// Captured for tests / diagnostics; insert re-reads live parent gen.
    #[allow(dead_code)]
    ownership_generation: u64,
    parent_working_dir: Option<String>,
    launch_context: ConnectionLaunchContext,
}

impl ConnectionManagerSpawner {
    /// Read live parent emitter/owner/workdir and build Delegation launch
    /// context from the parent's latest `effective_locale` in one snapshot.
    /// Production `spawn` is the only call site besides the focused test.
    async fn resolve_parent_spawn_launch_snapshot(
        &self,
        parent_connection_id: &str,
    ) -> Result<ParentSpawnLaunchSnapshot, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        // Falling back is not safe: a child whose emitter is wired to a
        // different broadcaster would emit events the frontend never sees.
        let conns = self.manager.connections.lock().await;
        let parent = conns.get(parent_connection_id).ok_or_else(|| {
            SpawnerError::Spawn(format!(
                "parent connection {parent_connection_id} not found"
            ))
        })?;
        let (parent_working_dir, parent_locale) = {
            let s = parent.state.read().await;
            let pwd = s
                .working_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string());
            (pwd, s.effective_locale)
        };
        Ok(ParentSpawnLaunchSnapshot {
            emitter: parent.emitter.clone(),
            owner_window_label: parent.owner_window_label.clone(),
            owner_operation_id: parent.owner_operation_id.clone(),
            ownership_generation: parent.ownership_generation,
            parent_working_dir,
            launch_context: delegation_launch_context(parent_locale),
        })
    }
}

#[async_trait::async_trait]
impl crate::acp::delegation::spawner::ConnectionSpawner for ConnectionManagerSpawner {
    async fn spawn(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
    ) -> Result<String, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        let parent = self
            .resolve_parent_spawn_launch_snapshot(parent_connection_id)
            .await?;
        let effective_working_dir = working_dir.or(parent.parent_working_dir);

        // Build the same launch inputs `acp_connect` would build for a
        // user-initiated session — disabled check, settings overrides,
        // model provider creds, git helper, and terminal settings. Without
        // this, delegated subagents would skip the user's configuration.
        // Children always force Codeg origin regardless of global settings.
        let runtime = self.runtime.snapshot();
        let launch_inputs = crate::acp::terminal_context::build_acp_launch_inputs(
            &self.db,
            agent_type,
            None,
            self.data_dir.as_path(),
            crate::acp::terminal_context::AcpRouteRequest::codeg_child(),
            &runtime,
        )
        .await
        .map_err(|e| SpawnerError::Spawn(e.to_string()))?;

        // Snapshot carries Delegation purpose + parent's latest effective locale.
        // Provisional ownership from the snapshot; insert re-reads parent under
        // lock (parent-generation CAS fence for concurrent rebind).
        self.manager
            .spawn_agent(
                agent_type,
                effective_working_dir,
                None,
                launch_inputs,
                parent.owner_window_label,
                parent.emitter,
                preferred_mode_id,
                preferred_config_values,
                parent.launch_context,
                parent.owner_operation_id,
                Some(parent_connection_id.to_string()),
            )
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))
    }

    async fn spawn_with_workflow_binding(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        workflow_binding: Option<crate::acp::delegation::workflow::WorkflowChildMcpBinding>,
    ) -> Result<String, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        let parent = self
            .resolve_parent_spawn_launch_snapshot(parent_connection_id)
            .await?;
        let effective_working_dir = working_dir.or(parent.parent_working_dir);
        let runtime = self.runtime.snapshot();
        let launch_inputs = crate::acp::terminal_context::build_acp_launch_inputs(
            &self.db,
            agent_type,
            None,
            self.data_dir.as_path(),
            crate::acp::terminal_context::AcpRouteRequest::codeg_child(),
            &runtime,
        )
        .await
        .map_err(|e| SpawnerError::Spawn(e.to_string()))?;

        self.manager
            .spawn_agent_with_attach_mode_and_workflow_binding(
                agent_type,
                effective_working_dir,
                None,
                launch_inputs,
                parent.owner_window_label,
                parent.emitter,
                preferred_mode_id,
                preferred_config_values,
                parent.launch_context,
                parent.owner_operation_id,
                Some(parent_connection_id.to_string()),
                crate::acp::session_attach::SessionAttachMode::Default,
                None,
                workflow_binding,
            )
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))
    }

    async fn spawn_resume_existing(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        external_session_id: String,
        preallocated_connection_id: Option<String>,
    ) -> Result<String, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        use crate::acp::session_attach::SessionAttachMode;
        let parent = self
            .resolve_parent_spawn_launch_snapshot(parent_connection_id)
            .await?;
        let effective_working_dir = working_dir.or(parent.parent_working_dir);
        let runtime = self.runtime.snapshot();
        let launch_inputs = crate::acp::terminal_context::build_acp_launch_inputs(
            &self.db,
            agent_type,
            None,
            self.data_dir.as_path(),
            crate::acp::terminal_context::AcpRouteRequest::codeg_child(),
            &runtime,
        )
        .await
        .map_err(|e| SpawnerError::Spawn(e.to_string()))?;

        self.manager
            .spawn_agent_with_attach_mode(
                agent_type,
                effective_working_dir,
                Some(external_session_id),
                launch_inputs,
                parent.owner_window_label,
                parent.emitter,
                preferred_mode_id,
                preferred_config_values,
                parent.launch_context,
                parent.owner_operation_id,
                Some(parent_connection_id.to_string()),
                SessionAttachMode::ResumeExistingOnly,
                preallocated_connection_id,
            )
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))
    }

    async fn spawn_resume_existing_with_workflow_binding(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        external_session_id: String,
        preallocated_connection_id: Option<String>,
        workflow_binding: Option<crate::acp::delegation::workflow::WorkflowChildMcpBinding>,
    ) -> Result<String, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        let parent = self
            .resolve_parent_spawn_launch_snapshot(parent_connection_id)
            .await?;
        let effective_working_dir = working_dir.or(parent.parent_working_dir);
        let runtime = self.runtime.snapshot();
        let launch_inputs = crate::acp::terminal_context::build_acp_launch_inputs(
            &self.db,
            agent_type,
            None,
            self.data_dir.as_path(),
            crate::acp::terminal_context::AcpRouteRequest::codeg_child(),
            &runtime,
        )
        .await
        .map_err(|e| SpawnerError::Spawn(e.to_string()))?;

        self.manager
            .spawn_agent_with_attach_mode_and_workflow_binding(
                agent_type,
                effective_working_dir,
                Some(external_session_id),
                launch_inputs,
                parent.owner_window_label,
                parent.emitter,
                preferred_mode_id,
                preferred_config_values,
                parent.launch_context,
                parent.owner_operation_id,
                Some(parent_connection_id.to_string()),
                crate::acp::session_attach::SessionAttachMode::ResumeExistingOnly,
                preallocated_connection_id,
                workflow_binding,
            )
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))
    }

    async fn send_prompt_linked_for_delegation(
        &self,
        conn_id: &str,
        task: String,
        link: crate::acp::delegation::spawner::DelegationLink,
        prebound_child: Option<(i32, i32)>,
    ) -> Result<
        crate::acp::delegation::spawner::AcceptedDelegationPrompt,
        crate::acp::delegation::spawner::SpawnerError,
    > {
        use crate::acp::delegation::spawner::{AcceptedDelegationPrompt, SpawnerError};
        // Prebound path: child row already exists (durable gen-1 reserving
        // fence). Adopt it — do not re-create. Keep `link` so send_prompt_linked
        // preserves delegation child semantics (no user-message, no awaiting-
        // reply, no mandatory-route scan). Link FKs were written at create.
        // Legacy path: resolve folder from the child's working_dir and create
        // a linked row during send.
        let (folder_id, conversation_id, link_for_send) = match prebound_child {
            Some((cid, fid)) => (Some(fid), Some(cid), Some(link)),
            None => {
                let working_dir_pathbuf = {
                    let conns = self.manager.connections.lock().await;
                    let conn = conns
                        .get(conn_id)
                        .ok_or_else(|| SpawnerError::send(format!("child {conn_id} not found")))?;
                    let s = conn.state.read().await;
                    s.working_dir.clone()
                };
                let folder_path = working_dir_pathbuf
                    .ok_or_else(|| {
                        SpawnerError::send(
                            "child connection has no working_dir; cannot derive folder_id",
                        )
                    })?
                    .to_string_lossy()
                    .to_string();
                // RegistrationOnly: working_dir FK for a hidden delegation
                // child — never ForceOpen solely because the child row exists.
                // User-visible folder open is handled elsewhere (explicit open).
                let folder = crate::db::service::folder_service::ensure_folder(
                    &self.db.conn,
                    &folder_path,
                    crate::db::service::folder_service::EnsureFolderMode::RegistrationOnly,
                )
                .await
                .map_err(|e| SpawnerError::send(format!("ensure_folder: {e}")))?;
                (Some(folder.id), None, Some(link))
            }
        };

        // Broker task is the authoritative visible text; locale resolves via
        // the child's inherited effective_locale (capture locale = None).
        let capture = Some(PromptCaptureContext::new(Some(task.clone()), None));
        match self
            .manager
            .send_prompt_linked(
                &self.db,
                conn_id,
                vec![PromptInputBlock::Text { text: task }],
                folder_id,
                conversation_id,
                link_for_send,
                capture,
            )
            .await
        {
            Ok(Some(cid)) => {
                // Sample accept time immediately at the command-path success
                // boundary — before any unrelated awaits (watchdog state lock).
                // Do not re-read conversation.delegation_started_at (may be
                // stale gen-1 or missing before promote projection).
                let prompt_accepted_at = chrono::Utc::now();
                // Soft-watchdog: first successful child prompt enqueue resets
                // agent activity so a newly accepted silent child gets a full
                // threshold window. Does not touch idle-sweep last_activity_at
                // beyond whatever send_prompt already did for general liveness.
                if let Some(state) = self.manager.get_state(conn_id).await {
                    state.write().await.mark_agent_activity(chrono::Utc::now());
                }
                Ok(AcceptedDelegationPrompt {
                    child_conversation_id: cid,
                    prompt_accepted_at,
                })
            }
            Ok(None) => Err(SpawnerError::send(
                "send_prompt_linked succeeded but no conversation_id was bound",
            )),
            Err(e) => {
                // Row may already exist (created before prompt enqueue). Preserve
                // its id so the broker can settle failed/spawn_failed.
                let child_conversation_id = {
                    let conns = self.manager.connections.lock().await;
                    match conns.get(conn_id) {
                        Some(conn) => conn.state.read().await.conversation_id,
                        None => None,
                    }
                };
                Err(SpawnerError::Send {
                    message: e.to_string(),
                    child_conversation_id,
                })
            }
        }
    }

    async fn cancel(
        &self,
        conn_id: &str,
    ) -> Result<(), crate::acp::delegation::spawner::SpawnerError> {
        self.manager
            .cancel(&self.db.conn, conn_id)
            .await
            .map_err(|e| crate::acp::delegation::spawner::SpawnerError::Cancel(e.to_string()))
    }

    async fn disconnect(
        &self,
        conn_id: &str,
    ) -> Result<(), crate::acp::delegation::spawner::SpawnerError> {
        self.manager
            .disconnect_with_origin(conn_id, AcpDisconnectOrigin::InternalJobComplete)
            .await
            .map_err(|e| crate::acp::delegation::spawner::SpawnerError::Disconnect(e.to_string()))
    }
}

/// Production impl of `ParentSessionLookup` for the delegation listener.
/// Resolves the parent's current `conversation_id` by reading its
/// `SessionState`. Bundled with `ConnectionManagerSpawner` here so the
/// concrete wiring lives next to the manager it depends on.
#[derive(Clone)]
pub struct ConnectionManagerParentLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl crate::acp::delegation::listener::ParentSessionLookup for ConnectionManagerParentLookup {
    async fn current_conversation_id(&self, parent_connection_id: &str) -> Option<i32> {
        let state = self.manager.get_state(parent_connection_id).await?;
        let snapshot = state.read().await;
        snapshot.conversation_id
    }

    async fn parent_wait_context(
        &self,
        parent_connection_id: &str,
    ) -> Option<crate::acp::delegation::listener::ParentWaitContext> {
        let state = self.manager.get_state(parent_connection_id).await?;
        let snapshot = state.read().await;
        let conversation_id = snapshot.conversation_id?;
        let turn_generation = snapshot.active_turn_generation.unwrap_or(0);
        // Wait tool id is request-associated only (companion `_meta` / rewrite).
        // Never scan `active_tool_calls` for a status-looking label.
        Some(crate::acp::delegation::listener::ParentWaitContext {
            conversation_id,
            connection_incarnation: snapshot.connection_incarnation.clone(),
            turn_generation,
            parent_tool_use_id: None,
        })
    }

    async fn bind_delegation_wait(
        &self,
        parent_connection_id: &str,
        expected: &crate::acp::tool_watchdog::WaitStamp,
    ) -> crate::acp::tool_watchdog::BindDelegationWaitResult {
        use crate::acp::tool_watchdog::{tool_lease_key, BindDelegationWaitResult};

        // Keep original opaque host bytes when non-blank (trim only for emptiness).
        let tool_id = match expected.parent_tool_use_id.as_ref() {
            Some(s) if !s.trim().is_empty() => s.clone(),
            _ => return BindDelegationWaitResult::WaitToolIdMissing,
        };

        if expected.connection_id != parent_connection_id {
            return BindDelegationWaitResult::WaitStampStale;
        }

        let state = match self.manager.get_state(parent_connection_id).await {
            Some(s) => s,
            None => return BindDelegationWaitResult::BindFailed,
        };
        let (attr, turn) = {
            let snapshot = state.read().await;
            let Some(turn) = snapshot.tool_watchdog_turn_stamp() else {
                return BindDelegationWaitResult::WaitStampStale;
            };
            if turn.connection_incarnation != expected.connection_incarnation
                || turn.turn_generation != expected.turn_generation
            {
                return BindDelegationWaitResult::WaitStampStale;
            }
            if snapshot.conversation_id != Some(expected.parent_conversation_id) {
                return BindDelegationWaitResult::WaitStampStale;
            }
            (snapshot.lease_attribution(), turn)
        };

        // Exact lease only — never register_or_touch_tool / invent a lease.
        let Some(lease_stamp) = attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, &tool_id))
            .await
        else {
            return BindDelegationWaitResult::WaitToolLeaseMismatch;
        };

        if lease_stamp.connection_id != expected.connection_id
            || lease_stamp.connection_incarnation != expected.connection_incarnation
            || lease_stamp.turn_generation != expected.turn_generation
        {
            return BindDelegationWaitResult::WaitStampStale;
        }
        // Bound lease tool id must equal registered wait tool id.
        let lease_tool = lease_stamp
            .tool_call_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let expected_tool = tool_id.trim();
        if lease_tool != Some(expected_tool) {
            return BindDelegationWaitResult::WaitToolLeaseMismatch;
        }

        match attr
            .bind_delegation_wait(&lease_stamp, &expected.wait_id)
            .await
        {
            Some(_) => BindDelegationWaitResult::Bound,
            None => BindDelegationWaitResult::BindFailed,
        }
    }
}

/// Production impl of `SessionFeedbackAccess` for the delegation listener's
/// `check_user_feedback` arm. Resolves the parent connection's pending feedback
/// by delegating to `ConnectionManager::read_pending_feedback` /
/// `commit_feedback_delivered`. Mirrors
/// `ConnectionManagerParentLookup` so the listener stays unit-testable with an
/// in-memory stub.
#[derive(Clone)]
pub struct ConnectionManagerFeedbackLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl SessionFeedbackAccess for ConnectionManagerFeedbackLookup {
    async fn read_pending_feedback(&self, parent_connection_id: &str) -> Vec<PendingFeedback> {
        self.manager
            .read_pending_feedback(parent_connection_id)
            .await
    }

    async fn commit_feedback_delivered(&self, parent_connection_id: &str, ids: Vec<String>) {
        self.manager
            .commit_feedback_delivered(parent_connection_id, ids)
            .await
    }
}

/// Production impl of `SessionQuestionAccess` for the delegation listener's
/// `ask_user_question` arm. Registers / cancels the parent connection's pending
/// question by delegating to `ConnectionManager`. Mirrors
/// `ConnectionManagerFeedbackLookup` so the listener stays unit-testable with an
/// in-memory stub.
#[derive(Clone)]
pub struct ConnectionManagerQuestionLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl SessionQuestionAccess for ConnectionManagerQuestionLookup {
    async fn register_question(
        &self,
        parent_connection_id: &str,
        questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion> {
        self.manager
            .register_question(parent_connection_id, questions)
            .await
    }

    async fn cancel_question(&self, parent_connection_id: &str, question_id: &str) {
        self.manager
            .cancel_question(parent_connection_id, question_id)
            .await
    }

    async fn cancel_questions_by_parent(&self, parent_connection_id: &str) {
        self.manager
            .cancel_questions_by_parent(parent_connection_id)
            .await
    }
}

/// Production impl of [`SessionPlanApprovalAccess`] for the Grok `exit_plan_mode`
/// ext bridge. Registers / cancels the parent connection's pending plan approval
/// by delegating to `ConnectionManager`. Mirrors `ConnectionManagerQuestionLookup`
/// so the connection handler stays unit-testable with an in-memory stub.
#[derive(Clone)]
pub struct ConnectionManagerPlanApprovalLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl SessionPlanApprovalAccess for ConnectionManagerPlanApprovalLookup {
    async fn register_plan_approval(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
        plan_markdown: String,
    ) -> Option<RegisteredPlanApproval> {
        self.manager
            .register_plan_approval(parent_connection_id, tool_call_id, plan_markdown)
            .await
    }

    async fn cancel_plan_approvals_by_parent(&self, parent_connection_id: &str) {
        self.manager
            .cancel_plan_approvals_by_parent(parent_connection_id)
            .await
    }
}

#[cfg(test)]
mod disconnect_origin {
    use super::*;
    use crate::acp::connection::{DelegationInjection, ParentConnectionExitEvidence};
    use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker};
    use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
    use crate::acp::delegation::types::DelegationError;
    use crate::acp::plan_approval::{RegisteredPlanApproval, SessionPlanApprovalAccess};
    use crate::acp::question::{QuestionSpec, RegisteredQuestion, SessionQuestionAccess};
    use crate::acp::termination::AcpDisconnectOrigin;
    use crate::acp::tool_watchdog::{RegisterTool, ToolCategory, TurnStamp, WatchdogInstant};

    struct EmptyDepth;

    #[async_trait::async_trait]
    impl ConversationDepthLookup for EmptyDepth {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    struct AllAgentsAvailable;

    #[async_trait::async_trait]
    impl crate::acp::connection::AgentAvailabilityLookup for AllAgentsAvailable {
        async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
            Vec::new()
        }
    }

    struct NoQuestions;

    #[async_trait::async_trait]
    impl SessionQuestionAccess for NoQuestions {
        async fn register_question(
            &self,
            _parent_connection_id: &str,
            _questions: Vec<QuestionSpec>,
        ) -> Option<RegisteredQuestion> {
            None
        }

        async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}

        async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}
    }

    struct NoPlanApprovals;

    #[async_trait::async_trait]
    impl SessionPlanApprovalAccess for NoPlanApprovals {
        async fn register_plan_approval(
            &self,
            _parent_connection_id: &str,
            _tool_call_id: String,
            _plan_markdown: String,
        ) -> Option<RegisteredPlanApproval> {
            None
        }

        async fn cancel_plan_approvals_by_parent(&self, _parent_connection_id: &str) {}
    }

    fn install_exit_evidence(manager: &ConnectionManager) -> Arc<ParentConnectionExitEvidence> {
        let evidence = Arc::new(ParentConnectionExitEvidence::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        manager.install_delegation(DelegationInjection {
            broker,
            continuation_coordinator: std::sync::Weak::new(),
            parent_connection_exit_causes: evidence.clone(),
            tokens: Arc::new(crate::acp::delegation::listener::TokenRegistry::default()),
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: PathBuf::from("disconnect-origin.sock"),
            agent_availability: Arc::new(AllAgentsAvailable),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
            questions: Arc::new(NoQuestions),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
            plan_approvals: Arc::new(NoPlanApprovals),
        });
        evidence
    }

    fn install_disconnect_final_cas_hook(
        manager: &ConnectionManager,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *manager
            .disconnect_final_cas_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(DisconnectFinalCasHook {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        (reached, resume)
    }

    async fn install_live_disconnect_control(
        manager: &ConnectionManager,
        connection_id: &str,
    ) -> tokio::sync::mpsc::Receiver<ConnectionControl> {
        let (control_tx, control_rx, _liveness) = crate::acp::connection::connection_channel(1);
        manager
            .connections
            .lock()
            .await
            .get_mut(connection_id)
            .expect("connection for live control")
            .control_tx = control_tx;
        control_rx
    }

    async fn connection_incarnation(manager: &ConnectionManager, connection_id: &str) -> String {
        manager
            .connections
            .lock()
            .await
            .get(connection_id)
            .expect("test connection")
            .connection_incarnation
            .clone()
    }

    async fn admit_registry_tool(
        manager: &ConnectionManager,
        connection_id: &str,
        incarnation: &str,
        turn_generation: u64,
    ) -> String {
        let turn = TurnStamp {
            connection_id: connection_id.into(),
            connection_incarnation: incarnation.into(),
            session_id: format!("session-{connection_id}-{turn_generation}"),
            turn_generation,
        };
        let at = WatchdogInstant::now();
        manager
            .tool_lease_registry
            .start_turn(turn.clone(), at)
            .await;
        assert!(
            manager.tool_lease_registry.has_fallback(&turn).await,
            "start_turn must admit a fallback lease"
        );
        manager
            .tool_lease_registry
            .register_tool(RegisterTool {
                turn,
                tool_call_id: format!("tool-{connection_id}-{turn_generation}"),
                category: ToolCategory::Other,
                at,
            })
            .await
            .expect("register_tool must admit a routable incarnation")
            .stamp
            .lease_id
    }

    async fn assert_registry_remains_routable(
        manager: &ConnectionManager,
        connection_id: &str,
        incarnation: &str,
    ) {
        assert!(
            !manager
                .tool_lease_registry
                .is_fenced(connection_id, incarnation)
                .await,
            "a surviving disconnect loser must not be fenced"
        );
        let _ = admit_registry_tool(manager, connection_id, incarnation, 99).await;
    }

    #[tokio::test]
    async fn disconnect_aborts_a_connecting_driver_that_ignores_control() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("c-connecting", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let join = tokio::spawn(async move {
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        let abort = join.abort_handle();
        let state = {
            let mut map = manager.connections.lock().await;
            let conn = map.get_mut("c-connecting").expect("test connection");
            conn.task_abort = Some(abort);
            conn.status = ConnectionStatus::Connecting;
            Arc::clone(&conn.state)
        };
        state.write().await.status = ConnectionStatus::Connecting;
        manager
            .disconnect_with_origin("c-connecting", AcpDisconnectOrigin::LegacyUnspecified)
            .await
            .expect("disconnect");
        let joined = tokio::time::timeout(std::time::Duration::from_secs(1), join)
            .await
            .expect("aborted driver must finish");
        assert!(joined.is_err(), "driver task must be aborted");
    }

    #[tokio::test]
    async fn disconnect_does_not_abort_a_connected_driver() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("c-connected", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let join = tokio::spawn(async move {
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        let abort = join.abort_handle();
        {
            let mut map = manager.connections.lock().await;
            let conn = map.get_mut("c-connected").expect("test connection");
            conn.task_abort = Some(abort);
            conn.status = ConnectionStatus::Connected;
        }
        manager
            .disconnect_with_origin("c-connected", AcpDisconnectOrigin::LegacyUnspecified)
            .await
            .expect("disconnect");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !join.is_finished(),
            "Connected disconnect must send Disconnect, not abort the driver"
        );
        join.abort();
    }

    #[tokio::test]
    async fn manager_records_origin_before_map_removal_and_disconnect_control() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("c1", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let evidence = install_exit_evidence(&manager);
        let incarnation = connection_incarnation(&manager, "c1").await;
        let lease_id = admit_registry_tool(&manager, "c1", &incarnation, 1).await;
        let mut control_rx = install_live_disconnect_control(&manager, "c1").await;

        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *manager
            .disconnect_final_cas_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(DisconnectFinalCasHook {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .disconnect_with_origin("c1", AcpDisconnectOrigin::DisconnectAll)
                    .await
            })
        };

        reached.notified().await;
        assert_eq!(
            evidence.peek("c1"),
            None,
            "intent must wait for the final incarnation CAS"
        );
        assert!(manager.connections.lock().await.contains_key("c1"));
        assert!(
            !manager
                .tool_lease_registry
                .is_fenced("c1", &incarnation)
                .await,
            "winner must remain unfenced until its final map CAS succeeds"
        );
        resume.notify_one();
        assert!(matches!(
            control_rx.recv().await,
            Some(ConnectionControl::Disconnect)
        ));
        assert!(
            manager
                .tool_lease_registry
                .is_fenced("c1", &incarnation)
                .await,
            "removed winner must be fenced before Disconnect delivery"
        );
        assert!(
            !manager.tool_lease_registry.is_live(&lease_id).await,
            "removed winner leases must be cleared before Disconnect delivery"
        );
        disconnect.await.expect("disconnect join").unwrap();

        assert_eq!(
            evidence
                .peek("c1")
                .expect("successful final CAS must record intent")
                .frontend_origin,
            Some(AcpDisconnectOrigin::DisconnectAll)
        );
        assert!(manager.get_state("c1").await.is_none());
    }

    #[tokio::test]
    async fn disconnect_origin_absent_unleased_id_records_no_intent() {
        let manager = ConnectionManager::new();
        let evidence = install_exit_evidence(&manager);

        let result = manager
            .disconnect_with_origin("missing", AcpDisconnectOrigin::DisconnectAll)
            .await;

        assert!(matches!(
            result,
            Err(AcpError::ConnectionNotFound(connection_id)) if connection_id == "missing"
        ));
        assert_eq!(
            evidence.peek("missing"),
            None,
            "an absent connection must not leave stale disconnect intent"
        );
    }

    #[tokio::test]
    async fn disconnect_origin_unleased_replacement_race_records_no_intent_for_survivor() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("c1", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let original_incarnation = manager
            .connections
            .lock()
            .await
            .get("c1")
            .expect("original connection")
            .connection_incarnation
            .clone();
        let evidence = install_exit_evidence(&manager);
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *manager
            .disconnect_final_cas_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(DisconnectFinalCasHook {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .disconnect_with_origin("c1", AcpDisconnectOrigin::DisconnectAll)
                    .await
            })
        };

        reached.notified().await;
        manager
            .insert_test_connection("c1", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let replacement_incarnation = manager
            .connections
            .lock()
            .await
            .get("c1")
            .expect("replacement connection")
            .connection_incarnation
            .clone();
        assert_ne!(replacement_incarnation, original_incarnation);
        resume.notify_one();
        disconnect.await.expect("disconnect join").unwrap();

        let surviving_incarnation = manager
            .connections
            .lock()
            .await
            .get("c1")
            .expect("replacement must survive")
            .connection_incarnation
            .clone();
        assert_eq!(surviving_incarnation, replacement_incarnation);
        assert_eq!(
            evidence.peek("c1"),
            None,
            "a disconnect that loses the incarnation CAS must not classify the replacement"
        );
    }

    #[tokio::test]
    async fn disconnect_origin_disconnect_all_skips_disappeared_and_replaced_candidates() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("gone", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        manager
            .insert_test_connection("replaced", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let gone_incarnation = connection_incarnation(&manager, "gone").await;
        let original_incarnation = manager
            .connections
            .lock()
            .await
            .get("replaced")
            .expect("original connection")
            .connection_incarnation
            .clone();
        let evidence = install_exit_evidence(&manager);
        let (reached, resume) = install_disconnect_final_cas_hook(&manager);
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .disconnect_all(AcpDisconnectOrigin::DisconnectAll)
                    .await
            })
        };

        reached.notified().await;
        manager.connections.lock().await.remove("gone");
        manager
            .insert_test_connection("replaced", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let replacement_incarnation = manager
            .connections
            .lock()
            .await
            .get("replaced")
            .expect("replacement connection")
            .connection_incarnation
            .clone();
        assert_ne!(replacement_incarnation, original_incarnation);
        resume.notify_one();

        // `disconnect_all` snapshots twice so a connection inserted during the
        // first take is still collected. Handshake that second take; otherwise
        // the test waits on join while the second pass waits on `resume`.
        reached.notified().await;
        manager
            .insert_test_connection("replaced", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let second_replacement_incarnation = manager
            .connections
            .lock()
            .await
            .get("replaced")
            .expect("second replacement connection")
            .connection_incarnation
            .clone();
        assert_ne!(second_replacement_incarnation, replacement_incarnation);
        resume.notify_one();

        assert_eq!(disconnect.await.expect("disconnect join"), 0);
        assert_eq!(evidence.peek("gone"), None);
        assert_eq!(evidence.peek("replaced"), None);
        assert_eq!(
            manager
                .connections
                .lock()
                .await
                .get("replaced")
                .expect("replacement must remain routable")
                .connection_incarnation,
            second_replacement_incarnation
        );
        assert_registry_remains_routable(&manager, "gone", &gone_incarnation).await;
        assert_registry_remains_routable(&manager, "replaced", &original_incarnation).await;
        assert_registry_remains_routable(&manager, "replaced", &replacement_incarnation).await;
    }

    #[tokio::test]
    async fn disconnect_origin_owner_window_rebind_with_same_incarnation_survives() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("window", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let original_incarnation = {
            let mut connections = manager.connections.lock().await;
            let connection = connections.get_mut("window").expect("window connection");
            connection.owner_window_label = "conversation-1".into();
            connection.owner_operation_id = Some("op-a".into());
            connection.ownership_generation = 1;
            connection.connection_incarnation.clone()
        };
        let evidence = install_exit_evidence(&manager);
        let (reached, resume) = install_disconnect_final_cas_hook(&manager);
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.disconnect_by_owner_window("conversation-1").await })
        };

        reached.notified().await;
        {
            let mut connections = manager.connections.lock().await;
            let connection = connections.get_mut("window").expect("rebound connection");
            connection.owner_window_label = "main".into();
            connection.owner_operation_id = Some("op-b".into());
            connection.ownership_generation = 2;
            connection.state.write().await.owner_window_label = "main".into();
        }
        resume.notify_one();

        assert_eq!(disconnect.await.expect("disconnect join"), 0);
        let connections = manager.connections.lock().await;
        let survivor = connections
            .get("window")
            .expect("rebound connection survives");
        assert_eq!(survivor.connection_incarnation, original_incarnation);
        assert_eq!(survivor.owner_window_label, "main");
        assert_eq!(evidence.peek("window"), None);
        drop(connections);
        assert_registry_remains_routable(&manager, "window", &original_incarnation).await;
    }

    #[tokio::test]
    async fn disconnect_origin_owner_operation_generation_rebind_survives() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("operation", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let original_incarnation = {
            let mut connections = manager.connections.lock().await;
            let connection = connections
                .get_mut("operation")
                .expect("operation connection");
            connection.owner_window_label = "conversation-1".into();
            connection.owner_operation_id = Some("op-a".into());
            connection.ownership_generation = 1;
            connection.connection_incarnation.clone()
        };
        let evidence = install_exit_evidence(&manager);
        let (reached, resume) = install_disconnect_final_cas_hook(&manager);
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .disconnect_by_owner_window_and_operation("conversation-1", "op-a")
                    .await
            })
        };

        reached.notified().await;
        manager
            .connections
            .lock()
            .await
            .get_mut("operation")
            .expect("rebound operation")
            .ownership_generation = 2;
        resume.notify_one();

        assert_eq!(disconnect.await.expect("disconnect join"), 0);
        let connections = manager.connections.lock().await;
        let survivor = connections
            .get("operation")
            .expect("generation rebound survives");
        assert_eq!(survivor.ownership_generation, 2);
        assert_eq!(evidence.peek("operation"), None);
        drop(connections);
        assert_registry_remains_routable(&manager, "operation", &original_incarnation).await;
    }

    #[tokio::test]
    async fn disconnect_origin_successful_bulk_removal_records_origin_before_control() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("bulk", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let evidence = install_exit_evidence(&manager);
        let incarnation = connection_incarnation(&manager, "bulk").await;
        let lease_id = admit_registry_tool(&manager, "bulk", &incarnation, 1).await;
        let mut control_rx = install_live_disconnect_control(&manager, "bulk").await;
        let (reached, resume) = install_disconnect_final_cas_hook(&manager);
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .disconnect_all(AcpDisconnectOrigin::ApplicationShutdown)
                    .await
            })
        };

        reached.notified().await;
        assert_eq!(
            evidence.peek("bulk"),
            None,
            "bulk intent must wait for the final selection fence"
        );
        assert!(manager.connections.lock().await.contains_key("bulk"));
        assert!(
            !manager
                .tool_lease_registry
                .is_fenced("bulk", &incarnation)
                .await,
            "bulk winner must not be fenced before final selection validation"
        );
        {
            let mut connections = manager.connections.lock().await;
            let connection = connections
                .get_mut("bulk")
                .expect("selected bulk connection");
            connection.owner_window_label = "rebound-window".into();
            connection.owner_operation_id = Some("rebound-operation".into());
            connection.ownership_generation = 7;
        }
        resume.notify_one();
        assert!(matches!(
            control_rx.recv().await,
            Some(ConnectionControl::Disconnect)
        ));
        assert!(
            manager
                .tool_lease_registry
                .is_fenced("bulk", &incarnation)
                .await
        );
        assert!(!manager.tool_lease_registry.is_live(&lease_id).await);
        assert_eq!(
            evidence
                .peek("bulk")
                .expect("intent must precede disconnect control")
                .frontend_origin,
            Some(AcpDisconnectOrigin::ApplicationShutdown)
        );
        assert_eq!(disconnect.await.expect("disconnect join"), 1);
        assert!(!manager.connections.lock().await.contains_key("bulk"));
    }

    #[tokio::test]
    async fn disconnect_origin_successful_owner_window_removal_records_origin_before_control() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("window", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        {
            let mut connections = manager.connections.lock().await;
            let connection = connections.get_mut("window").expect("window connection");
            connection.owner_window_label = "conversation-1".into();
            connection.owner_operation_id = Some("op-a".into());
            connection.ownership_generation = 1;
        }
        let evidence = install_exit_evidence(&manager);
        let incarnation = connection_incarnation(&manager, "window").await;
        let lease_id = admit_registry_tool(&manager, "window", &incarnation, 1).await;
        let mut control_rx = install_live_disconnect_control(&manager, "window").await;
        let (reached, resume) = install_disconnect_final_cas_hook(&manager);
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.disconnect_by_owner_window("conversation-1").await })
        };

        reached.notified().await;
        assert_eq!(
            evidence.peek("window"),
            None,
            "window intent must wait for the final ownership fence"
        );
        assert!(manager.connections.lock().await.contains_key("window"));
        assert!(
            !manager
                .tool_lease_registry
                .is_fenced("window", &incarnation)
                .await,
            "window winner must not be fenced before final ownership validation"
        );
        resume.notify_one();
        assert!(matches!(
            control_rx.recv().await,
            Some(ConnectionControl::Disconnect)
        ));
        assert!(
            manager
                .tool_lease_registry
                .is_fenced("window", &incarnation)
                .await
        );
        assert!(!manager.tool_lease_registry.is_live(&lease_id).await);
        assert_eq!(
            evidence
                .peek("window")
                .expect("intent must precede disconnect control")
                .frontend_origin,
            Some(AcpDisconnectOrigin::ProviderUnmount)
        );
        assert_eq!(disconnect.await.expect("disconnect join"), 1);
        assert!(!manager.connections.lock().await.contains_key("window"));
    }

    #[tokio::test]
    async fn disconnect_origin_idle_sweep_records_idle_timeout_before_teardown() {
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection("idle", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let evidence = install_exit_evidence(&manager);
        let incarnation = connection_incarnation(&manager, "idle").await;
        let lease_id = admit_registry_tool(&manager, "idle", &incarnation, 1).await;
        let mut control_rx = install_live_disconnect_control(&manager, "idle").await;
        let state = manager.get_state("idle").await.expect("idle connection");
        state.write().await.last_activity_at = chrono::Utc::now() - chrono::Duration::minutes(10);

        assert_eq!(manager.sweep_idle(Duration::from_secs(300)).await, 1);
        assert!(matches!(
            control_rx.recv().await,
            Some(ConnectionControl::Disconnect)
        ));
        assert!(
            manager
                .tool_lease_registry
                .is_fenced("idle", &incarnation)
                .await
        );
        assert!(!manager.tool_lease_registry.is_live(&lease_id).await);
        assert_eq!(
            evidence
                .peek("idle")
                .expect("idle teardown must record intent")
                .frontend_origin,
            Some(AcpDisconnectOrigin::IdleTimeout)
        );
    }

    #[tokio::test]
    async fn disconnect_origin_idle_sweep_busy_transition_remains_routable() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("idle-busy", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let evidence = install_exit_evidence(&manager);
        let incarnation = connection_incarnation(&manager, "idle-busy").await;
        let state = manager
            .get_state("idle-busy")
            .await
            .expect("idle-busy connection");
        state.write().await.last_activity_at = chrono::Utc::now() - chrono::Duration::minutes(10);
        let (reached, resume) = install_disconnect_final_cas_hook(&manager);
        let sweep = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sweep_idle(Duration::from_secs(300)).await })
        };

        reached.notified().await;
        state.write().await.status = ConnectionStatus::Prompting;
        resume.notify_one();

        assert_eq!(sweep.await.expect("idle sweep join"), 0);
        assert!(manager.connections.lock().await.contains_key("idle-busy"));
        assert_eq!(evidence.peek("idle-busy"), None);
        assert_registry_remains_routable(&manager, "idle-busy", &incarnation).await;
    }

    #[tokio::test]
    async fn disconnect_origin_unexposed_teardown_records_abandoned_connect_before_abort() {
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection("abandoned", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let evidence = install_exit_evidence(&manager);
        let connections = manager.connections.clone();
        let evidence_watch = evidence.clone();
        let cleanup = tokio::spawn(async move {
            loop {
                if evidence_watch.peek("abandoned").is_some() {
                    connections.lock().await.remove("abandoned");
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        manager
            .teardown_unexposed_for_test_with_waits(
                "abandoned",
                Duration::from_millis(50),
                Duration::from_millis(50),
            )
            .await
            .expect("typed evidence should unblock simulated cleanup");
        cleanup.await.expect("cleanup join");
        assert_eq!(
            evidence
                .peek("abandoned")
                .expect("abandoned teardown must record intent")
                .frontend_origin,
            Some(AcpDisconnectOrigin::AbandonedConnect)
        );
    }

    #[tokio::test]
    async fn disconnect_origin_losing_final_owner_cas_does_not_poison_survivor_evidence() {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection("leased", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let evidence = install_exit_evidence(&manager);
        {
            let mut connections = manager.connections.lock().await;
            let connection = connections.get_mut("leased").expect("leased connection");
            connection.owner_window_label = "conversation-1".into();
            connection.owner_operation_id = Some("op-a".into());
            connection.ownership_generation = 1;
        }
        let incarnation = connection_incarnation(&manager, "leased").await;

        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *manager
            .disconnect_final_cas_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(DisconnectFinalCasHook {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        let disconnect = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .disconnect_if_owner(
                        "leased",
                        Some("conversation-1"),
                        Some("op-a"),
                        Some(1),
                        AcpDisconnectOrigin::ProviderUnmount,
                    )
                    .await
            })
        };

        reached.notified().await;
        {
            let mut connections = manager.connections.lock().await;
            let connection = connections.get_mut("leased").expect("surviving connection");
            connection.owner_operation_id = Some("op-b".into());
            connection.ownership_generation = 2;
        }
        resume.notify_one();
        disconnect.await.expect("disconnect join").unwrap();

        assert!(manager.connections.lock().await.contains_key("leased"));
        assert_eq!(
            evidence.peek("leased"),
            None,
            "a stale disconnect that loses the final CAS must not classify the survivor"
        );
        assert_registry_remains_routable(&manager, "leased", &incarnation).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::connection::{AgentConnection, RegisteredSpawnAttempt, SpawnHandshake};
    use crate::acp::delegation::route::{
        resolve_route, DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource,
        RouteDegradedReason, RouteResolutionInput, SuppressionCapability,
        ROUTE_ADAPTER_CONTRACT_VERSION,
    };
    use crate::acp::delegation::spawner::ConnectionSpawner;
    use crate::acp::plan_approval::PlanApprovalDecision;
    use crate::acp::session_attach::SessionAttachMode;
    use crate::acp::session_state::SessionState;
    use crate::acp::shared_session::{
        SharedInteractionRequest, SharedLaunchIdentity, SharedMutationGuard, SharedReserveRequest,
        SharedSessionKey, SharedSessionPhase,
    };
    use crate::acp::terminal_context::{AcpLaunchInputs, AcpRouteRequest};
    use crate::acp::types::ConnectionStatus;
    use crate::auto_title::{ConnectionLaunchContext, ConnectionPurpose};
    use crate::models::agent::BUILTIN_AGENT_TYPES;
    use crate::models::SystemTerminalSettings;
    use crate::web::event_bridge::{EventEmitter, WebEvent, WebEventBroadcaster};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;
    use tokio::sync::{broadcast, mpsc, RwLock};

    fn install_admission_insert_hold(
        manager: &ConnectionManager,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *manager
            .admission_insert_hold
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(AdmissionInsertHold {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        (reached, resume)
    }

    struct FakeSharedSpawnDriver {
        outcomes: StdMutex<VecDeque<tokio::sync::oneshot::Receiver<RouteBootstrapOutcome>>>,
        starts: AtomicUsize,
        start_log: Arc<StdMutex<Vec<String>>>,
        route_log: Arc<StdMutex<Vec<RouteAttemptTrace>>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RouteAttemptTrace {
        connection_id: String,
        agent_type: AgentType,
        effective: DelegationRoutePolicy,
        source: DelegationRouteSource,
    }

    impl FakeSharedSpawnDriver {
        fn pending() -> (Self, tokio::sync::oneshot::Sender<RouteBootstrapOutcome>) {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (
                Self {
                    outcomes: StdMutex::new(VecDeque::from([rx])),
                    starts: AtomicUsize::new(0),
                    start_log: Arc::new(StdMutex::new(Vec::new())),
                    route_log: Arc::new(StdMutex::new(Vec::new())),
                },
                tx,
            )
        }

        fn immediate_ready() -> Self {
            Self::immediate_ready_many(1)
        }

        fn immediate_ready_many(count: usize) -> Self {
            let mut outcomes = VecDeque::new();
            for _ in 0..count {
                let (tx, rx) = tokio::sync::oneshot::channel();
                tx.send(RouteBootstrapOutcome::Ready).unwrap();
                outcomes.push_back(rx);
            }
            Self {
                outcomes: StdMutex::new(outcomes),
                starts: AtomicUsize::new(0),
                start_log: Arc::new(StdMutex::new(Vec::new())),
                route_log: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn gated_many(
            count: usize,
        ) -> (
            Self,
            Vec<tokio::sync::oneshot::Sender<RouteBootstrapOutcome>>,
        ) {
            let mut outcomes = VecDeque::new();
            let mut gates = Vec::new();
            for _ in 0..count {
                let (tx, rx) = tokio::sync::oneshot::channel();
                gates.push(tx);
                outcomes.push_back(rx);
            }
            (
                Self {
                    outcomes: StdMutex::new(outcomes),
                    starts: AtomicUsize::new(0),
                    start_log: Arc::new(StdMutex::new(Vec::new())),
                    route_log: Arc::new(StdMutex::new(Vec::new())),
                },
                gates,
            )
        }

        fn fallback_sequence() -> (
            Self,
            tokio::sync::oneshot::Sender<RouteBootstrapOutcome>,
            Arc<StdMutex<Vec<String>>>,
        ) {
            let (first_tx, first_rx) = tokio::sync::oneshot::channel();
            let (second_tx, second_rx) = tokio::sync::oneshot::channel();
            second_tx.send(RouteBootstrapOutcome::Ready).unwrap();
            let start_log = Arc::new(StdMutex::new(Vec::new()));
            (
                Self {
                    outcomes: StdMutex::new(VecDeque::from([first_rx, second_rx])),
                    starts: AtomicUsize::new(0),
                    start_log: start_log.clone(),
                    route_log: Arc::new(StdMutex::new(Vec::new())),
                },
                first_tx,
                start_log,
            )
        }

        fn fallback_with_gated_replacement() -> (
            Self,
            tokio::sync::oneshot::Sender<RouteBootstrapOutcome>,
            tokio::sync::oneshot::Sender<RouteBootstrapOutcome>,
        ) {
            let (first_tx, first_rx) = tokio::sync::oneshot::channel();
            let (second_tx, second_rx) = tokio::sync::oneshot::channel();
            (
                Self {
                    outcomes: StdMutex::new(VecDeque::from([first_rx, second_rx])),
                    starts: AtomicUsize::new(0),
                    start_log: Arc::new(StdMutex::new(Vec::new())),
                    route_log: Arc::new(StdMutex::new(Vec::new())),
                },
                first_tx,
                second_tx,
            )
        }
    }

    #[async_trait::async_trait]
    impl SharedSpawnDriver for FakeSharedSpawnDriver {
        async fn start(
            &self,
            connection_id: String,
            launch: SharedConnectLaunch,
            existing_public_state: Option<Arc<RwLock<SessionState>>>,
        ) -> Result<RegisteredSpawnAttempt, AcpError> {
            let attempt = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
            self.start_log
                .lock()
                .unwrap()
                .push(format!("start-{attempt}"));
            self.route_log.lock().unwrap().push(RouteAttemptTrace {
                connection_id: connection_id.clone(),
                agent_type: launch.agent_type,
                effective: launch.launch_inputs.route_plan.effective,
                source: launch.launch_inputs.route_plan.source,
            });
            let connection_incarnation = format!("fake-incarnation-{attempt}");
            let state = match existing_public_state {
                Some(state) => {
                    let mut replacement = SessionState::new(
                        connection_id.clone(),
                        launch.agent_type,
                        launch.working_dir.clone().map(PathBuf::from),
                        "shared-server".into(),
                        launch.folder_id,
                    );
                    replacement.connection_incarnation = connection_incarnation.clone();
                    replacement.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
                    state
                        .write()
                        .await
                        .prepare_registered_replacement(replacement);
                    state
                }
                None => {
                    let mut state = SessionState::new(
                        connection_id.clone(),
                        launch.agent_type,
                        launch.working_dir.clone().map(PathBuf::from),
                        "shared-server".into(),
                        launch.folder_id,
                    );
                    state.connection_incarnation = connection_incarnation.clone();
                    state.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
                    Arc::new(RwLock::new(state))
                }
            };
            let (_session_started_tx, session_started_rx) = tokio::sync::oneshot::channel();
            let route_bootstrap_rx = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .expect("one route outcome per fake start");
            Ok(RegisteredSpawnAttempt {
                connection_id,
                connection_incarnation,
                state,
                emitter: EventEmitter::Noop,
                handshake: SpawnHandshake {
                    session_started_rx,
                    route_bootstrap_rx,
                },
                route_plan: launch.launch_inputs.route_plan,
                driver_start_tx: None,
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            })
        }
    }

    #[derive(Debug)]
    enum RegisteredSpawnFixture {
        Success,
    }

    impl RegisteredSpawnFixture {
        fn success() -> Self {
            Self::Success
        }
    }

    struct BlockedRegistrationDriver {
        registration_rx:
            tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<RegisteredSpawnFixture>>>,
        starts: AtomicUsize,
    }

    struct PanickingRegistrationDriver;

    struct ActivationObservingDriver {
        broker: Arc<std::sync::OnceLock<SharedSessionBroker>>,
        observations: tokio::sync::mpsc::UnboundedSender<(usize, Option<String>)>,
        starts: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SharedSpawnDriver for PanickingRegistrationDriver {
        async fn start(
            &self,
            _connection_id: String,
            _launch: SharedConnectLaunch,
            _existing_public_state: Option<Arc<RwLock<SessionState>>>,
        ) -> Result<RegisteredSpawnAttempt, AcpError> {
            panic!("intentional registered-spawn panic")
        }
    }

    #[async_trait::async_trait]
    impl SharedSpawnDriver for ActivationObservingDriver {
        async fn start(
            &self,
            connection_id: String,
            launch: SharedConnectLaunch,
            existing_public_state: Option<Arc<RwLock<SessionState>>>,
        ) -> Result<RegisteredSpawnAttempt, AcpError> {
            let attempt = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
            let connection_incarnation = format!("activation-incarnation-{attempt}");
            let state = match existing_public_state {
                Some(state) => {
                    let mut replacement = SessionState::new(
                        connection_id.clone(),
                        launch.agent_type,
                        launch.working_dir.clone().map(PathBuf::from),
                        "shared-server".into(),
                        launch.folder_id,
                    );
                    replacement.connection_incarnation = connection_incarnation.clone();
                    replacement.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
                    state
                        .write()
                        .await
                        .prepare_registered_replacement(replacement);
                    state
                }
                None => {
                    let mut state = SessionState::new(
                        connection_id.clone(),
                        launch.agent_type,
                        launch.working_dir.clone().map(PathBuf::from),
                        "shared-server".into(),
                        launch.folder_id,
                    );
                    state.connection_incarnation = connection_incarnation.clone();
                    state.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
                    Arc::new(RwLock::new(state))
                }
            };
            let (_session_started_tx, session_started_rx) = tokio::sync::oneshot::channel();
            let (route_tx, route_bootstrap_rx) = tokio::sync::oneshot::channel();
            let (driver_start_tx, driver_start_rx) = tokio::sync::oneshot::channel();
            let broker = self.broker.clone();
            let observations = self.observations.clone();
            let observed_connection_id = connection_id.clone();
            tokio::spawn(async move {
                driver_start_rx
                    .await
                    .expect("manager activates registered driver");
                let observed = broker
                    .get()
                    .expect("broker installed before connect")
                    .driver_incarnation_for_generation(&observed_connection_id, 1)
                    .await
                    .unwrap();
                observations.send((attempt, observed)).unwrap();
                let outcome = if attempt == 1 {
                    RouteBootstrapOutcome::RouteSpecific(
                        RouteDegradedReason::CompanionInitializationFailed,
                    )
                } else {
                    RouteBootstrapOutcome::Ready
                };
                route_tx.send(outcome).unwrap();
            });
            Ok(RegisteredSpawnAttempt {
                connection_id,
                connection_incarnation,
                state,
                emitter: EventEmitter::Noop,
                handshake: SpawnHandshake {
                    session_started_rx,
                    route_bootstrap_rx,
                },
                route_plan: launch.launch_inputs.route_plan,
                driver_start_tx: Some(driver_start_tx),
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            })
        }
    }

    #[async_trait::async_trait]
    impl SharedSpawnDriver for BlockedRegistrationDriver {
        async fn start(
            &self,
            connection_id: String,
            launch: SharedConnectLaunch,
            existing_public_state: Option<Arc<RwLock<SessionState>>>,
        ) -> Result<RegisteredSpawnAttempt, AcpError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let rx = self
                .registration_rx
                .lock()
                .await
                .take()
                .expect("one blocked registration");
            match rx.await {
                Ok(RegisteredSpawnFixture::Success) => {}
                Err(_) => return Err(AcpError::ProcessExited),
            }
            let driver = FakeSharedSpawnDriver::immediate_ready();
            driver
                .start(connection_id, launch, existing_public_state)
                .await
        }
    }

    fn manager_with_blocked_registration() -> (
        ConnectionManager,
        tokio::sync::oneshot::Sender<RegisteredSpawnFixture>,
    ) {
        let (registration_tx, registration_rx) = tokio::sync::oneshot::channel();
        let driver = BlockedRegistrationDriver {
            registration_rx: tokio::sync::Mutex::new(Some(registration_rx)),
            starts: AtomicUsize::new(0),
        };
        (
            ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver)),
            registration_tx,
        )
    }

    fn codeg_route_plan(source: DelegationRouteSource) -> DelegationRoutePlan {
        resolve_route(RouteResolutionInput {
            agent_type: AgentType::Codex,
            origin: DelegationConnectionOrigin::Root,
            session_override: (source == DelegationRouteSource::SessionOverride)
                .then_some(DelegationRoutePolicy::Codeg),
            global_policy: DelegationRoutePolicy::Codeg,
            delegation_enabled: true,
            suppression: SuppressionCapability::supported(ROUTE_ADAPTER_CONTRACT_VERSION),
            agent_mcp_supported: true,
            companion_binary_available: true,
        })
        .expect("valid Codeg route")
    }

    async fn shared_launch(conversation_id: i32, client: &str) -> SharedConnectLaunch {
        shared_launch_with_folder(conversation_id, 9, client).await
    }

    async fn shared_launch_with_folder(
        conversation_id: i32,
        folder_id: i32,
        client: &str,
    ) -> SharedConnectLaunch {
        shared_launch_for_agent(
            conversation_id,
            folder_id,
            client,
            AgentType::Codex,
            codeg_route_plan(DelegationRouteSource::GlobalDefault),
        )
        .await
    }

    async fn shared_launch_for_agent(
        conversation_id: i32,
        folder_id: i32,
        client: &str,
        agent_type: AgentType,
        route_plan: DelegationRoutePlan,
    ) -> SharedConnectLaunch {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let seeded_folder = crate::db::test_helpers::seed_folder(
            &db,
            &format!("/tmp/shared-root-{conversation_id}-{client}"),
        )
        .await;
        let seeded_conversation =
            crate::db::test_helpers::seed_conversation(&db, seeded_folder, agent_type).await;
        db.conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE conversation SET id = ? WHERE id = ?",
                [conversation_id.into(), seeded_conversation.into()],
            ))
            .await
            .expect("persist exact conversation key");

        let terminal_settings = SystemTerminalSettings::default();
        let mut launch_inputs =
            AcpLaunchInputs::with_placeholder_route(BTreeMap::new(), terminal_settings.clone());
        launch_inputs.route_plan = route_plan.clone();
        launch_inputs.route_capability.agent_mcp_supported =
            crate::acp::registry::get_agent_meta(agent_type).supports_mcp;
        SharedConnectLaunch {
            database: db.conn,
            key: SharedSessionKey::Conversation(conversation_id),
            conversation_id: Some(conversation_id),
            folder_id: Some(folder_id),
            launch_identity: SharedLaunchIdentity {
                agent_type,
                working_dir_fingerprint: String::new(),
                external_session_id: None,
                attach_mode: crate::acp::session_attach::SessionAttachMode::Default,
                route_fingerprint: route_plan.fingerprint.clone(),
                route_capability:
                    crate::acp::shared_session::SharedRouteCapability::from_route_plan(&route_plan),
                terminal_shell_fingerprint: crate::terminal::shell::terminal_shell_selection_key(
                    &terminal_settings,
                ),
                purpose: ConnectionPurpose::User,
            },
            agent_type,
            working_dir: None,
            external_session_id: None,
            launch_inputs,
            emitter: EventEmitter::Noop,
            preferred_mode_id: None,
            preferred_config_values: BTreeMap::new(),
            launch_context: ConnectionLaunchContext::default(),
            session_attach_mode: crate::acp::session_attach::SessionAttachMode::Default,
            device_id: "device-a".into(),
            client_instance_id: client.into(),
            request_id: format!("request-{client}"),
            retry_failed_generation: None,
        }
    }

    async fn registry_route_launch_for_agent(
        conversation_id: i32,
        client: &str,
        agent_type: AgentType,
        route_policy: DelegationRoutePolicy,
        session_override: Option<DelegationRoutePolicy>,
    ) -> SharedConnectLaunch {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let working_dir = format!("/tmp/shared-registry-{conversation_id}-{client}");
        let folder_id = crate::db::test_helpers::seed_folder(&db, &working_dir).await;
        let seeded_conversation = crate::commands::conversations::create_conversation_core(
            &db.conn,
            folder_id,
            agent_type,
            None,
            session_override,
        )
        .await
        .expect("seed route-aware conversation");
        db.conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE conversation SET id = ? WHERE id = ?",
                [conversation_id.into(), seeded_conversation.into()],
            ))
            .await
            .expect("persist exact route fixture conversation key");

        let runtime_env = BTreeMap::new();
        let mut route_capability = crate::acp::terminal_context::build_route_capability_snapshot(
            agent_type,
            None,
            &runtime_env,
        );
        route_capability.companion_binary_available = true;
        let runtime = crate::commands::delegation::DelegationRuntimeSnapshot {
            enabled: true,
            route_policy,
            stalled_after_seconds: 300,
        };
        let route_plan = crate::acp::terminal_context::resolve_connect_route_with_capability(
            &db.conn,
            agent_type,
            AcpRouteRequest::root(Some(conversation_id), None),
            &runtime,
            &route_capability,
        )
        .await
        .expect("registry route fixture resolves");
        let terminal_settings = SystemTerminalSettings::default();
        let launch_inputs = AcpLaunchInputs {
            runtime_env,
            terminal_settings: terminal_settings.clone(),
            route_plan: route_plan.clone(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: session_override,
            route_capability,
        };
        SharedConnectLaunch {
            database: db.conn,
            key: SharedSessionKey::Conversation(conversation_id),
            conversation_id: Some(conversation_id),
            folder_id: Some(folder_id),
            launch_identity: SharedLaunchIdentity {
                agent_type,
                working_dir_fingerprint: crate::parsers::normalize_path_for_matching(&working_dir),
                external_session_id: None,
                attach_mode: crate::acp::session_attach::SessionAttachMode::Default,
                route_fingerprint: route_plan.fingerprint.clone(),
                route_capability:
                    crate::acp::shared_session::SharedRouteCapability::from_route_plan(&route_plan),
                terminal_shell_fingerprint: crate::terminal::shell::terminal_shell_selection_key(
                    &terminal_settings,
                ),
                purpose: ConnectionPurpose::User,
            },
            agent_type,
            working_dir: Some(working_dir),
            external_session_id: None,
            launch_inputs,
            emitter: EventEmitter::Noop,
            preferred_mode_id: None,
            preferred_config_values: BTreeMap::new(),
            launch_context: ConnectionLaunchContext::default(),
            session_attach_mode: crate::acp::session_attach::SessionAttachMode::Default,
            device_id: "device-a".into(),
            client_instance_id: client.into(),
            request_id: format!("request-{client}"),
            retry_failed_generation: None,
        }
    }

    async fn wait_until_broker_reserved(manager: &ConnectionManager, conversation_id: i32) {
        manager
            .shared_session_broker()
            .wait_for_key_phase_for_test(
                &SharedSessionKey::Conversation(conversation_id),
                SharedSessionPhase::Reserved,
            )
            .await
            .expect("broker diagnostic watch reaches Reserved");
    }

    #[tokio::test]
    async fn shared_connect_returns_before_bootstrap_settles() {
        let (driver, gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            manager.connect_or_attach_shared(shared_launch(41, "client-a").await),
        )
        .await
        .expect("reservation must return without route readiness")
        .unwrap();
        assert_eq!(response.phase, SharedSessionPhase::Bootstrapping);
        assert!(manager.get_state(&response.connection_id).await.is_some());
        assert_eq!(manager.shared_spawn_count_for_test(), 1);
        gate.send(RouteBootstrapOutcome::Ready).unwrap();
        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_concurrent_same_conversation_starts_one_driver() {
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(
            FakeSharedSpawnDriver::immediate_ready(),
        ));
        let launches = futures::future::join_all(
            (0..10).map(|n| async move { shared_launch(55, &format!("c-{n}")).await }),
        )
        .await;
        let results = futures::future::join_all(launches.into_iter().map(|launch| {
            let manager = manager.clone_ref();
            async move { manager.connect_or_attach_shared(launch).await.unwrap() }
        }))
        .await;
        assert!(results
            .windows(2)
            .all(|pair| pair[0].connection_id == pair[1].connection_id));
        assert_eq!(manager.shared_spawn_count_for_test(), 1);
    }

    #[tokio::test]
    async fn shared_concurrent_distinct_roots_start_independent_drivers() {
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(
            FakeSharedSpawnDriver::immediate_ready_many(2),
        ));
        let (first_launch, second_launch) = tokio::join!(
            shared_launch(551, "client-a"),
            shared_launch(552, "client-b")
        );
        let (first, second) = tokio::join!(
            manager.connect_or_attach_shared(first_launch),
            manager.connect_or_attach_shared(second_launch)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.connection_id, second.connection_id);
        assert_eq!(manager.shared_spawn_count_for_test(), 2);
    }

    #[tokio::test]
    async fn shared_cancelled_creator_cannot_strand_reserved_or_block_attachers() {
        let (manager, registration_gate) = manager_with_blocked_registration();
        let creator_launch = shared_launch(56, "creator").await;
        let creator_manager = manager.clone_ref();
        let creator = tokio::spawn(async move {
            creator_manager
                .connect_or_attach_shared(creator_launch)
                .await
        });
        wait_until_broker_reserved(&manager, 56).await;
        creator.abort();
        let attacher_launch = shared_launch(56, "attacher").await;
        let attacher_manager = manager.clone_ref();
        let attacher = tokio::spawn(async move {
            attacher_manager
                .connect_or_attach_shared(attacher_launch)
                .await
        });
        registration_gate
            .send(RegisteredSpawnFixture::success())
            .unwrap();
        let response = tokio::time::timeout(Duration::from_millis(100), attacher)
            .await
            .expect("owned registration must outlive the cancelled HTTP caller")
            .unwrap()
            .unwrap();
        assert_ne!(response.phase, SharedSessionPhase::Reserved);
        assert!(manager.get_state(&response.connection_id).await.is_some());
    }

    #[tokio::test]
    async fn shared_registration_panic_supervisor_publishes_failed_state() {
        let manager =
            ConnectionManager::new_with_shared_spawn_driver(Arc::new(PanickingRegistrationDriver));
        let mut launch = shared_launch(60, "client").await;
        launch.external_session_id = Some("external-session-60".into());
        launch.launch_identity.external_session_id = launch.external_session_id.clone();
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            manager.connect_or_attach_shared(launch),
        )
        .await
        .expect("registration supervisor must settle a panicked owned task")
        .unwrap();
        assert_eq!(
            response.phase,
            SharedSessionPhase::Failed {
                error_code: "session_unavailable".into(),
                cleanup_complete: true,
            }
        );
        let state = manager.get_state(&response.connection_id).await.unwrap();
        let state = state.read().await;
        assert_eq!(state.status, ConnectionStatus::Error);
        assert_eq!(state.external_id.as_deref(), Some("external-session-60"));
    }

    #[tokio::test]
    async fn shared_persisted_registration_binds_ids_before_connect_response() {
        let (driver, _gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let response = manager
            .connect_or_attach_shared(shared_launch_with_folder(57, 9, "client").await)
            .await
            .unwrap();
        let state = manager.get_state(&response.connection_id).await.unwrap();
        let state = state.read().await;
        assert_eq!(state.conversation_id, Some(57));
        assert_eq!(state.folder_id, Some(9));
        assert_eq!(response.phase, SharedSessionPhase::Bootstrapping);
    }

    #[test]
    fn explicit_codeg_route_never_falls_back_after_companion_failure() {
        let plan = codeg_route_plan(DelegationRouteSource::SessionOverride);
        assert_eq!(
            shared_bootstrap_action(
                &plan,
                RouteBootstrapOutcome::RouteSpecific(
                    RouteDegradedReason::CompanionInitializationFailed,
                ),
            ),
            SharedBootstrapAction::Fail(
                crate::acp::shared_session::SharedSessionError::CompanionInitializationFailed,
            )
        );
    }

    #[test]
    fn every_required_companion_reason_maps_to_one_stable_public_failure() {
        for reason in [
            RouteDegradedReason::NativeSuppressionUnsupported,
            RouteDegradedReason::NativeSuppressionInvalid,
            RouteDegradedReason::CompanionBinaryUnavailable,
            RouteDegradedReason::AgentMcpUnsupported,
            RouteDegradedReason::CompanionInitializationFailed,
        ] {
            assert_eq!(
                map_route_failure(reason),
                crate::acp::shared_session::SharedSessionError::CompanionInitializationFailed
            );
        }
    }

    #[tokio::test]
    async fn shared_explicit_codeg_companion_failure_is_failed_without_fallback() {
        let (driver, gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let launch = shared_launch_for_agent(
            59,
            9,
            "client",
            AgentType::Codex,
            codeg_route_plan(DelegationRouteSource::SessionOverride),
        )
        .await;
        let response = manager.connect_or_attach_shared(launch).await.unwrap();
        gate.send(RouteBootstrapOutcome::RouteSpecific(
            RouteDegradedReason::CompanionInitializationFailed,
        ))
        .unwrap();
        let failed = SharedSessionPhase::Failed {
            error_code: "companion_initialization_failed".into(),
            cleanup_complete: true,
        };
        manager
            .wait_for_shared_phase(&response.connection_id, response.generation, failed.clone())
            .await
            .unwrap();
        assert_eq!(manager.shared_spawn_count_for_test(), 1);
        assert_eq!(
            manager
                .shared_session_broker()
                .diagnostic_for_connection(&response.connection_id)
                .await
                .unwrap()
                .phase,
            failed
        );
    }

    #[tokio::test]
    async fn typed_required_companion_outcome_wins_a_simultaneous_terminal_status() {
        let (driver, gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let launch = shared_launch_for_agent(
            590,
            9,
            "client",
            AgentType::Codex,
            codeg_route_plan(DelegationRouteSource::SessionOverride),
        )
        .await;
        let response = manager.connect_or_attach_shared(launch).await.unwrap();

        assert!(
            manager
                .emit_test_shared_driver_event(
                    &response.connection_id,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Error,
                    },
                )
                .await
        );
        gate.send(RouteBootstrapOutcome::RouteSpecific(
            RouteDegradedReason::CompanionInitializationFailed,
        ))
        .unwrap();

        let expected = SharedSessionPhase::Failed {
            error_code: "companion_initialization_failed".into(),
            cleanup_complete: true,
        };
        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                expected.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            manager
                .shared_session_broker()
                .diagnostic_for_connection(&response.connection_id)
                .await
                .unwrap()
                .phase,
            expected
        );
        assert_eq!(manager.shared_spawn_count_for_test(), 1);
    }

    #[tokio::test]
    async fn typed_permitted_fallback_wins_a_simultaneous_terminal_status() {
        let (driver, first_gate, start_log) = FakeSharedSpawnDriver::fallback_sequence();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let response = manager
            .connect_or_attach_shared(shared_launch(591, "client").await)
            .await
            .unwrap();

        assert!(
            manager
                .emit_test_shared_driver_event(
                    &response.connection_id,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Disconnected,
                    },
                )
                .await
        );
        first_gate
            .send(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            ))
            .unwrap();

        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();
        assert_eq!(manager.shared_spawn_count_for_test(), 2);
        assert_eq!(start_log.lock().unwrap().as_slice(), ["start-1", "start-2"]);
    }

    #[tokio::test]
    async fn ready_bootstrap_outcome_cannot_revive_a_terminal_driver_state() {
        let (driver, gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let response = manager
            .connect_or_attach_shared(shared_launch(593, "client").await)
            .await
            .unwrap();
        let (state, _) = manager
            .get_state_and_emitter(&response.connection_id)
            .await
            .expect("registered shared state");
        state.write().await.status = ConnectionStatus::Error;
        gate.send(RouteBootstrapOutcome::Ready).unwrap();

        let expected = SharedSessionPhase::Failed {
            error_code: "session_unavailable".into(),
            cleanup_complete: true,
        };
        tokio::time::timeout(
            Duration::from_secs(2),
            manager.wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                expected.clone(),
            ),
        )
        .await
        .expect("terminal bootstrap must settle instead of remaining bootstrapping")
        .unwrap();
        assert_eq!(
            manager
                .shared_session_broker()
                .diagnostic_for_connection(&response.connection_id)
                .await
                .unwrap()
                .phase,
            expected
        );
    }

    #[tokio::test]
    async fn terminal_status_after_bootstrap_is_still_classified_by_the_runtime_monitor() {
        let (driver, gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let response = manager
            .connect_or_attach_shared(shared_launch(592, "client").await)
            .await
            .unwrap();
        gate.send(RouteBootstrapOutcome::Ready).unwrap();
        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();

        assert!(
            manager
                .emit_test_shared_driver_event(
                    &response.connection_id,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Error,
                    },
                )
                .await
        );
        let failed = SharedSessionPhase::Failed {
            error_code: "session_unavailable".into(),
            cleanup_complete: true,
        };
        manager
            .wait_for_shared_phase(&response.connection_id, response.generation, failed.clone())
            .await
            .unwrap();
        assert_eq!(
            manager
                .shared_session_broker()
                .diagnostic_for_connection(&response.connection_id)
                .await
                .unwrap()
                .phase,
            failed
        );
    }

    #[tokio::test]
    async fn shared_allowed_fallback_reuses_public_state_after_old_attempt_is_absent() {
        let (driver, first_gate, start_log) = FakeSharedSpawnDriver::fallback_sequence();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let response = manager
            .connect_or_attach_shared(shared_launch(58, "client").await)
            .await
            .unwrap();
        let before = manager.get_state(&response.connection_id).await.unwrap();
        before.write().await.event_seq = 73;
        first_gate
            .send(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            ))
            .unwrap();
        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();
        let after = manager.get_state(&response.connection_id).await.unwrap();
        assert!(Arc::ptr_eq(&before, &after));
        assert_eq!(after.read().await.event_seq, 73);
        assert_eq!(manager.shared_spawn_count_for_test(), 2);
        assert_eq!(start_log.lock().unwrap().as_slice(), ["start-1", "start-2"]);
        assert_eq!(
            manager.shared_fallback_trace.lock().unwrap().as_slice(),
            ["old_driver_absent", "replacement_start"]
        );
    }

    #[tokio::test]
    async fn shared_old_settler_panic_after_replacement_does_not_fail_new_driver() {
        let (driver, first_gate, replacement_gate) =
            FakeSharedSpawnDriver::fallback_with_gated_replacement();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        manager
            .shared_settler_panic_after_replacement
            .store(true, Ordering::SeqCst);
        let response = manager
            .connect_or_attach_shared(shared_launch(580, "client").await)
            .await
            .unwrap();

        first_gate
            .send(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            ))
            .unwrap();
        tokio::time::timeout(
            Duration::from_millis(100),
            manager.shared_settler_supervisor_completed.notified(),
        )
        .await
        .expect("old settler supervisor must complete its fenced failure attempt");

        assert_eq!(
            manager
                .shared_session_broker
                .diagnostic_for_connection(&response.connection_id)
                .await
                .unwrap()
                .phase,
            SharedSessionPhase::Bootstrapping
        );
        replacement_gate.send(RouteBootstrapOutcome::Ready).unwrap();
        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_stale_bootstrap_supervisor_cannot_mutate_replacement_state() {
        let (driver, _gate) = FakeSharedSpawnDriver::pending();
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        let launch = shared_launch(581, "client").await;
        let response = manager
            .connect_or_attach_shared(launch.clone())
            .await
            .unwrap();
        let state = manager.get_state(&response.connection_id).await.unwrap();
        let mut replacement = SessionState::new(
            response.connection_id.clone(),
            AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        );
        replacement.connection_incarnation = "fake-incarnation-2".into();
        let permit = manager
            .shared_session_broker
            .begin_registered_replacement(
                &response.connection_id,
                response.generation,
                "fake-incarnation-1",
            )
            .await
            .unwrap();
        state
            .write()
            .await
            .prepare_registered_replacement(replacement);
        manager
            .shared_session_broker
            .commit_registered_replacement(
                &permit,
                "fake-incarnation-2".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        manager
            .fail_shared_generation(
                response.connection_id.clone(),
                response.generation,
                Some("fake-incarnation-1".into()),
                SharedSessionError::SessionUnavailable,
                launch,
            )
            .await;

        let state = state.read().await;
        assert_eq!(state.connection_incarnation, "fake-incarnation-2");
        assert_eq!(state.status, ConnectionStatus::Connecting);
        assert_eq!(
            state
                .shared_session
                .as_ref()
                .map(|projection| &projection.phase),
            Some(&SharedSessionPhase::Bootstrapping)
        );
    }

    #[tokio::test]
    async fn shared_fallback_activates_each_driver_after_broker_incarnation_install() {
        let broker = Arc::new(std::sync::OnceLock::new());
        let (observations_tx, mut observations_rx) = tokio::sync::mpsc::unbounded_channel();
        let driver = ActivationObservingDriver {
            broker: broker.clone(),
            observations: observations_tx,
            starts: AtomicUsize::new(0),
        };
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(driver));
        assert!(broker.set(manager.shared_session_broker()).is_ok());

        let response = manager
            .connect_or_attach_shared(shared_launch(582, "client").await)
            .await
            .unwrap();
        manager
            .wait_for_shared_phase(
                &response.connection_id,
                response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();

        assert_eq!(
            observations_rx.recv().await,
            Some((1, Some("activation-incarnation-1".into())))
        );
        assert_eq!(
            observations_rx.recv().await,
            Some((2, Some("activation-incarnation-2".into())))
        );
    }

    #[tokio::test]
    async fn shared_replacement_map_and_state_incarnations_match_on_first_exposure() {
        let manager = ConnectionManager::new();
        let agent_type =
            AgentType::custom("missing-replacement-fence").expect("valid custom agent id");
        let launch = shared_launch_for_agent(
            583,
            9,
            "replacement-client",
            agent_type,
            crate::acp::delegation::route::test_empty_route_plan(),
        )
        .await;
        let reservation = manager
            .shared_session_broker
            .reserve_or_attach(SharedReserveRequest {
                key: launch.key.clone(),
                connection_id: "replacement-map-state".into(),
                launch_identity: launch.launch_identity.clone(),
                client_instance_id: launch.client_instance_id.clone(),
                device_id: launch.device_id.clone(),
                request_id: launch.request_id.clone(),
                retry_failed_generation: None,
                now: tokio::time::Instant::now(),
                now_utc: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let state = Arc::new(RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            agent_type,
            None,
            "shared-server".into(),
            Some(9),
        )));
        state.write().await.connection_incarnation = "old-incarnation".into();
        manager
            .shared_session_broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "old-incarnation".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        manager.shared_launches.lock().await.insert(
            (
                reservation.attachment.connection_id.clone(),
                reservation.attachment.generation,
            ),
            launch.clone(),
        );
        let permit = manager
            .shared_session_broker
            .begin_registered_replacement(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "old-incarnation",
            )
            .await
            .unwrap();

        let replacement = manager
            .start_shared_attempt(
                reservation.attachment.connection_id.clone(),
                launch,
                Some(state.clone()),
            )
            .await
            .unwrap();
        {
            let map = manager.connections.lock().await;
            let connection = map
                .get(&reservation.attachment.connection_id)
                .expect("registered replacement is exposed in the manager map");
            let state_incarnation = connection
                .state
                .try_read()
                .expect("replacement state is prepared before map exposure")
                .connection_incarnation
                .clone();
            assert_eq!(connection.connection_incarnation, state_incarnation);
            assert_eq!(replacement.connection_incarnation, state_incarnation);
        }

        manager
            .shared_session_broker
            .commit_registered_replacement(
                &permit,
                replacement.connection_incarnation.clone(),
                replacement.state.clone(),
                replacement.emitter.clone(),
                replacement.child_pid.clone(),
            )
            .await
            .unwrap();
        manager
            .teardown_unexposed_attempt(&reservation.attachment.connection_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_rejected_replacement_permit_leaves_public_state_unchanged() {
        let broker = SharedSessionBroker::default();
        let launch = shared_launch(583, "client").await;
        let reservation = broker
            .reserve_or_attach(SharedReserveRequest {
                key: launch.key.clone(),
                connection_id: "rejected-replacement".into(),
                launch_identity: launch.launch_identity.clone(),
                client_instance_id: launch.client_instance_id.clone(),
                device_id: launch.device_id.clone(),
                request_id: launch.request_id.clone(),
                retry_failed_generation: None,
                now: tokio::time::Instant::now(),
                now_utc: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let state = Arc::new(RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        state.write().await.connection_incarnation = "driver-authoritative".into();
        broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-authoritative".into(),
                state.clone(),
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();

        assert!(matches!(
            broker
                .begin_registered_replacement(
                    &reservation.attachment.connection_id,
                    reservation.attachment.generation,
                    "driver-lost-race",
                )
                .await,
            Err(SharedSessionError::GenerationStale)
        ));

        assert_eq!(
            state.read().await.connection_incarnation,
            "driver-authoritative"
        );
    }

    #[tokio::test]
    async fn shared_teardown_waits_for_broker_retained_process_after_map_removal() {
        let manager = ConnectionManager::new();
        let launch = shared_launch(584, "client").await;
        let reservation = manager
            .shared_session_broker
            .reserve_or_attach(SharedReserveRequest {
                key: launch.key,
                connection_id: "already-unmapped-process".into(),
                launch_identity: launch.launch_identity,
                client_instance_id: launch.client_instance_id,
                device_id: launch.device_id,
                request_id: launch.request_id,
                retry_failed_generation: None,
                now: tokio::time::Instant::now(),
                now_utc: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let state = Arc::new(RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            AgentType::Codex,
            None,
            "shared-server".into(),
            Some(9),
        )));
        let child_pid = Arc::new(std::sync::atomic::AtomicU32::new(4242));
        manager
            .shared_session_broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "driver-incarnation".into(),
                state,
                EventEmitter::Noop,
                child_pid.clone(),
            )
            .await
            .unwrap();
        assert!(!manager
            .connections
            .lock()
            .await
            .contains_key(&reservation.attachment.connection_id));

        let mut teardown = Box::pin(manager.teardown_unexposed_for_test_with_waits(
            &reservation.attachment.connection_id,
            Duration::from_millis(100),
            Duration::from_millis(50),
        ));
        assert!(matches!(
            futures::poll!(teardown.as_mut()),
            std::task::Poll::Pending
        ));
        child_pid.store(0, Ordering::SeqCst);
        teardown.await.unwrap();
    }

    #[tokio::test]
    async fn application_shutdown_backstops_a_broker_retained_process_without_a_map_entry() {
        let manager = ConnectionManager::new();
        let launch = shared_launch(585, "client").await;
        let reservation = manager
            .shared_session_broker
            .reserve_or_attach(SharedReserveRequest {
                key: launch.key,
                connection_id: "shutdown-retained-process".into(),
                launch_identity: launch.launch_identity,
                client_instance_id: launch.client_instance_id,
                device_id: launch.device_id,
                request_id: launch.request_id,
                retry_failed_generation: None,
                now: tokio::time::Instant::now(),
                now_utc: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let state = Arc::new(RwLock::new(SessionState::new(
            reservation.attachment.connection_id.clone(),
            AgentType::Codex,
            None,
            "shared-server".into(),
            Some(585),
        )));
        let child_pid = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
        manager
            .shared_session_broker
            .install_registered(
                &reservation.attachment.connection_id,
                reservation.attachment.generation,
                "shutdown-driver".into(),
                state,
                EventEmitter::Noop,
                child_pid,
            )
            .await
            .unwrap();
        assert!(!manager
            .connections
            .lock()
            .await
            .contains_key(&reservation.attachment.connection_id));

        tokio::time::pause();
        let mut shutdown =
            Box::pin(manager.disconnect_all(AcpDisconnectOrigin::ApplicationShutdown));
        assert!(
            matches!(futures::poll!(shutdown.as_mut()), std::task::Poll::Pending),
            "a retained child PID must receive the same bounded shutdown grace/backstop"
        );
        tokio::time::advance(DISCONNECT_ALL_GRACE).await;
        assert_eq!(shutdown.await, 0);
        assert!(manager.shared_session_diagnostics().await.is_empty());
    }

    fn assert_server_shutting_down<T: std::fmt::Debug>(result: Result<T, AcpError>) {
        match result {
            Err(error) => {
                assert_eq!(
                    error.code(),
                    Some("server_shutting_down"),
                    "expected server_shutting_down, got {error:?}"
                );
            }
            Ok(value) => panic!("expected server_shutting_down, accepted {value:?}"),
        }
    }

    async fn spawn_direct_for_admission_test(
        manager: &ConnectionManager,
    ) -> Result<String, AcpError> {
        manager
            .spawn_agent(
                AgentType::Codex,
                None,
                None,
                AcpLaunchInputs::with_placeholder_route(
                    BTreeMap::new(),
                    SystemTerminalSettings::default(),
                ),
                "test-window".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                None,
                None,
            )
            .await
    }

    #[tokio::test]
    async fn shutdown_admission_race_rejects_new_spawns_and_drains_admitted_ones() {
        let driver = Arc::new(FakeSharedSpawnDriver::immediate_ready());
        let manager = ConnectionManager::new_with_shared_spawn_driver(driver);
        manager.enable_stub_direct_spawn_for_test();
        manager
            .insert_test_connection(
                "parent-for-child",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;

        let (reached, resume) = install_admission_insert_hold(&manager);
        let held_manager = manager.clone_ref();
        let held =
            tokio::spawn(async move { spawn_direct_for_admission_test(&held_manager).await });
        reached.notified().await;
        assert_eq!(manager.admission_in_flight_for_test(), 1);
        assert!(
            !manager
                .connections
                .lock()
                .await
                .values()
                .any(|connection| connection.id != "parent-for-child"),
            "admitted spawn must not insert before the hold is released"
        );

        manager.begin_shutdown();

        let cloned = manager.clone_ref();
        assert_server_shutting_down(spawn_direct_for_admission_test(&cloned).await);

        let shared = cloned.connect_or_attach_shared(shared_launch(801, "shutdown-client").await);
        assert_server_shutting_down(shared.await);

        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        let spawner = ConnectionManagerSpawner {
            manager: Arc::new(cloned.clone_ref()),
            db,
            data_dir: Arc::new(PathBuf::from("/tmp")),
            runtime: crate::commands::delegation::DelegationRuntimeSettings::default(),
        };
        let child = spawner
            .spawn(
                "parent-for-child",
                AgentType::Codex,
                None,
                None,
                BTreeMap::new(),
            )
            .await;
        match child {
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("shutting down") || message.contains("server_shutting_down"),
                    "delegated spawn must fail with server_shutting_down, got {message}"
                );
            }
            Ok(id) => panic!("expected delegated spawn to be fenced, accepted {id}"),
        }

        tokio::time::pause();
        let mut drain =
            Box::pin(cloned.drain_for_shutdown(AcpDisconnectOrigin::ApplicationShutdown));
        tokio::select! {
            biased;
            _ = drain.as_mut() => {
                panic!("drain returned while an admitted spawn was still in flight");
            }
            result = async {
                resume.notify_one();
                held.await.expect("held spawn join")
            } => {
                result.expect("admitted spawn must finish after shutdown");
            }
        }
        tokio::time::advance(DISCONNECT_ALL_GRACE).await;
        drain.await;
        assert!(
            manager.connections.lock().await.is_empty(),
            "drain must remove the parent and the admitted connection"
        );
        assert_eq!(manager.admission_in_flight_for_test(), 0);
    }

    #[tokio::test]
    async fn shutdown_admission_race_shared_connect_cannot_attach_after_shutdown() {
        let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(
            FakeSharedSpawnDriver::immediate_ready(),
        ));
        let existing = manager
            .connect_or_attach_shared(shared_launch(802, "creator").await)
            .await
            .unwrap();
        assert_ne!(existing.phase, SharedSessionPhase::Reserved);

        let (reached, resume) = install_admission_insert_hold(&manager);
        let held_manager = manager.clone_ref();
        let held = tokio::spawn(async move {
            held_manager
                .connect_or_attach_shared(shared_launch(802, "attacher").await)
                .await
        });
        reached.notified().await;

        manager.begin_shutdown();
        let mut wait = Box::pin(manager.wait_for_admissions());
        assert!(
            matches!(futures::poll!(wait.as_mut()), std::task::Poll::Pending),
            "wait_for_admissions returned while shared connect was still in flight"
        );
        assert_eq!(manager.admission_in_flight_for_test(), 1);

        resume.notify_one();
        assert_server_shutting_down(held.await.expect("held shared connect join"));
        wait.await;
        assert_eq!(manager.admission_in_flight_for_test(), 0);
    }

    #[tokio::test]
    // The guard intentionally spans the async launch loop: custom-agent hydration
    // replaces process-global state, so another registry test must not interleave.
    #[allow(clippy::await_holding_lock)]
    async fn shared_registry_drives_every_agent_through_one_spawn_path() {
        use crate::acp::custom_registry::{
            CustomAgentDef, CustomAgentSpec, CustomDistributionKind, NpxSpec,
        };

        let _registry_guard = crate::acp::custom_registry::hydrate_test_guard();
        let custom = AgentType::custom("shared-conformance").expect("valid fixture id");
        let definition = CustomAgentDef {
            registry_id: "shared-conformance".into(),
            name: "Shared Conformance".into(),
            description: "Task 12 shared-session route fixture".into(),
            version: "1.0.0".into(),
            distribution_kind: CustomDistributionKind::Npx,
            spec: CustomAgentSpec {
                npx: Some(NpxSpec {
                    package: "shared-conformance@1.0.0".into(),
                    cmd: Some("shared-conformance".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            icon_url: None,
            skills_shared_store: false,
            skills_dir: None,
            source: Default::default(),
            version_probe: None,
            supports_mcp: true,
        };
        assert!(crate::acp::custom_registry::hydrate(&[definition]).is_empty());
        let agent_types = crate::acp::registry::all_acp_agents();
        assert_eq!(agent_types.len(), BUILTIN_AGENT_TYPES.len() + 1);
        assert_eq!(agent_types.last(), Some(&custom));
        let expected_roots = agent_types.len();
        let (driver, readiness_gates) = FakeSharedSpawnDriver::gated_many(expected_roots);
        let driver = Arc::new(driver);
        let manager = ConnectionManager::new_with_shared_spawn_driver(driver.clone());
        let mut connection_ids = std::collections::HashSet::new();
        let mut required_companion_agents = Vec::new();
        let mut standard_agents = Vec::new();

        for (n, (agent_type, readiness_gate)) in
            agent_types.iter().copied().zip(readiness_gates).enumerate()
        {
            let launch = registry_route_launch_for_agent(
                1_000 + n as i32,
                &format!("agent-{n}"),
                agent_type,
                DelegationRoutePolicy::Codeg,
                None,
            )
            .await;
            let plan = launch.launch_inputs.route_plan.clone();
            if crate::acp::delegation::route::is_managed_agent(agent_type) {
                assert_eq!(
                    plan.effective,
                    DelegationRoutePolicy::Codeg,
                    "managed registry entries must exercise required-companion readiness"
                );
                assert_eq!(plan.source, DelegationRouteSource::GlobalDefault);
                required_companion_agents.push(agent_type);
            } else {
                assert_eq!(plan.effective, DelegationRoutePolicy::Native);
                assert_eq!(plan.source, DelegationRouteSource::GlobalDefault);
                standard_agents.push(agent_type);
            }
            let response = manager.connect_or_attach_shared(launch).await.unwrap();
            assert_eq!(response.phase, SharedSessionPhase::Bootstrapping);
            assert_eq!(
                manager
                    .shared_session_broker
                    .diagnostic_for_connection(&response.connection_id)
                    .await
                    .unwrap()
                    .phase,
                SharedSessionPhase::Bootstrapping,
                "agent {agent_type:?} must not publish Ready before its route readiness"
            );
            assert_eq!(
                driver.route_log.lock().unwrap().last(),
                Some(&RouteAttemptTrace {
                    connection_id: response.connection_id.clone(),
                    agent_type,
                    effective: plan.effective,
                    source: plan.source,
                })
            );
            readiness_gate.send(RouteBootstrapOutcome::Ready).unwrap();
            manager
                .wait_for_shared_phase(
                    &response.connection_id,
                    response.generation,
                    SharedSessionPhase::Ready,
                )
                .await
                .unwrap();
            assert!(connection_ids.insert(response.connection_id.clone()));
            assert_eq!(
                manager
                    .shared_session_broker
                    .diagnostic_for_connection(&response.connection_id)
                    .await
                    .unwrap()
                    .phase,
                SharedSessionPhase::Ready
            );
            let diagnostic = manager
                .shared_session_diagnostics()
                .await
                .into_iter()
                .find(|diagnostic| diagnostic.connection_id == response.connection_id)
                .expect("shared broker diagnostic");
            assert_eq!(diagnostic.lease_count, 1);
            assert_eq!(diagnostic.queue_depth, 0);
            assert_eq!(
                diagnostic.agent_category,
                if agent_type.is_custom() {
                    "custom".to_string()
                } else {
                    agent_type.as_wire().into_owned()
                }
            );
        }
        assert_eq!(required_companion_agents.len(), 4);
        assert_eq!(standard_agents.len(), expected_roots - 4);
        assert_eq!(connection_ids.len(), expected_roots);
        assert_eq!(manager.shared_spawn_count_for_test(), expected_roots);
        assert_eq!(
            manager.shared_registered_root_count_for_test(),
            expected_roots
        );
        assert_eq!(driver.route_log.lock().unwrap().len(), expected_roots);

        let (fallback_driver, first_gate, _) = FakeSharedSpawnDriver::fallback_sequence();
        let fallback_driver = Arc::new(fallback_driver);
        let fallback_manager =
            ConnectionManager::new_with_shared_spawn_driver(fallback_driver.clone());
        let fallback_response = fallback_manager
            .connect_or_attach_shared(
                registry_route_launch_for_agent(
                    2_000,
                    "global-fallback",
                    AgentType::Codex,
                    DelegationRoutePolicy::Codeg,
                    None,
                )
                .await,
            )
            .await
            .unwrap();
        first_gate
            .send(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            ))
            .unwrap();
        fallback_manager
            .wait_for_shared_phase(
                &fallback_response.connection_id,
                fallback_response.generation,
                SharedSessionPhase::Ready,
            )
            .await
            .unwrap();
        assert_eq!(fallback_manager.shared_spawn_count_for_test(), 2);
        assert_eq!(
            fallback_driver.route_log.lock().unwrap().as_slice(),
            [
                RouteAttemptTrace {
                    connection_id: fallback_response.connection_id.clone(),
                    agent_type: AgentType::Codex,
                    effective: DelegationRoutePolicy::Codeg,
                    source: DelegationRouteSource::GlobalDefault,
                },
                RouteAttemptTrace {
                    connection_id: fallback_response.connection_id.clone(),
                    agent_type: AgentType::Codex,
                    effective: DelegationRoutePolicy::Native,
                    source: DelegationRouteSource::SafeFallback,
                },
            ]
        );
        assert_eq!(
            fallback_manager
                .shared_fallback_trace
                .lock()
                .unwrap()
                .as_slice(),
            ["old_driver_absent", "replacement_start"]
        );

        let (override_driver, override_gate) = FakeSharedSpawnDriver::pending();
        let override_driver = Arc::new(override_driver);
        let override_manager =
            ConnectionManager::new_with_shared_spawn_driver(override_driver.clone());
        let override_response = override_manager
            .connect_or_attach_shared(
                registry_route_launch_for_agent(
                    2_001,
                    "session-override",
                    AgentType::Codex,
                    DelegationRoutePolicy::Native,
                    Some(DelegationRoutePolicy::Codeg),
                )
                .await,
            )
            .await
            .unwrap();
        override_gate
            .send(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            ))
            .unwrap();
        let failed = SharedSessionPhase::Failed {
            error_code: "companion_initialization_failed".into(),
            cleanup_complete: true,
        };
        override_manager
            .wait_for_shared_phase(
                &override_response.connection_id,
                override_response.generation,
                failed.clone(),
            )
            .await
            .unwrap();
        assert_eq!(override_manager.shared_spawn_count_for_test(), 1);
        assert_eq!(
            override_driver.route_log.lock().unwrap().as_slice(),
            [RouteAttemptTrace {
                connection_id: override_response.connection_id.clone(),
                agent_type: AgentType::Codex,
                effective: DelegationRoutePolicy::Codeg,
                source: DelegationRouteSource::SessionOverride,
            }]
        );
        assert!(override_manager
            .shared_fallback_trace
            .lock()
            .unwrap()
            .is_empty());
        assert_eq!(
            override_manager
                .shared_session_broker
                .diagnostic_for_connection(&override_response.connection_id)
                .await
                .unwrap()
                .phase,
            failed
        );
        crate::acp::custom_registry::hydrate(&[]);
    }

    #[tokio::test]
    async fn shared_legacy_build_failure_with_dedup_lock_returns_typed_fatal_promptly() {
        let manager = ConnectionManager::new();
        let agent_type = AgentType::custom("missing-shared-fixture").expect("valid fixture id");
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            manager.spawn_agent(
                agent_type,
                None,
                Some("external-session".into()),
                AcpLaunchInputs::with_placeholder_route(
                    BTreeMap::new(),
                    SystemTerminalSettings::default(),
                ),
                "test-window".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                None,
                None,
            ),
        )
        .await
        .expect("typed Fatal must bypass the session-start handshake timeout");
        assert!(matches!(result, Err(AcpError::SdkNotInstalled(_))));
        assert!(manager.connections.lock().await.is_empty());
    }

    struct TestNoPlanApprovals;

    #[async_trait::async_trait]
    impl SessionPlanApprovalAccess for TestNoPlanApprovals {
        async fn register_plan_approval(
            &self,
            _parent_connection_id: &str,
            _tool_call_id: String,
            _plan_markdown: String,
        ) -> Option<RegisteredPlanApproval> {
            None
        }

        async fn cancel_plan_approvals_by_parent(&self, _parent_connection_id: &str) {}
    }

    fn no_plan_approvals() -> Arc<dyn SessionPlanApprovalAccess> {
        Arc::new(TestNoPlanApprovals)
    }

    struct AllAgentsAvailable;

    #[async_trait::async_trait]
    impl crate::acp::connection::AgentAvailabilityLookup for AllAgentsAvailable {
        async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn internal_probe_launch_context_tags_internal_probe_purpose() {
        // Policy used by `probe_agent_options`: InternalProbe, no inherited
        // locale (effective English via connection launch unwrap_or).
        let ctx = internal_probe_launch_context();
        assert_eq!(ctx.purpose, ConnectionPurpose::InternalProbe);
        assert_eq!(ctx.inherited_locale, None);
    }

    #[tokio::test]
    async fn delegated_child_inherits_parent_effective_locale() {
        // Real manager/database path: parent locale is ZhCn; the spawn-owned
        // parent launch snapshot (consumed by ConnectionManagerSpawner::spawn)
        // must inherit it onto the child; delegated send must persist the
        // broker task as first_user_text under that locale. Does not spawn a
        // real external agent.
        use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
        use crate::auto_title::{enable_title_api_for_test, title_key};
        use crate::db::entities::auto_title_job;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = Arc::new(test_helpers::fresh_in_memory_db().await);
        let _suite = title_key::test_hooks::SuiteGuard::enter();
        enable_title_api_for_test(&db.conn).await;

        let mgr = Arc::new(ConnectionManager::new());
        let parent_id = "deleg-parent-locale";
        let child_id = "deleg-child-locale";
        let parent_workdir = PathBuf::from("/tmp/deleg-parent-locale");
        let _parent_rx = mgr
            .insert_test_connection_live(
                parent_id,
                AgentType::ClaudeCode,
                Some(parent_workdir.clone()),
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state(parent_id).await.unwrap();
            let mut s = state.write().await;
            s.effective_locale = AppLocale::ZhCn;
            s.purpose = ConnectionPurpose::User;
        }

        let spawner = ConnectionManagerSpawner {
            manager: mgr.clone(),
            db: db.clone(),
            data_dir: Arc::new(PathBuf::from("/tmp")),
            runtime: crate::commands::delegation::DelegationRuntimeSettings::default(),
        };
        // Production spawn-owned resolver: must read live parent state and build
        // Delegation + parent effective_locale (not English default).
        let snapshot = spawner
            .resolve_parent_spawn_launch_snapshot(parent_id)
            .await
            .expect("parent spawn launch snapshot");
        assert_eq!(
            snapshot.launch_context.purpose,
            ConnectionPurpose::Delegation
        );
        assert_eq!(
            snapshot.launch_context.inherited_locale,
            Some(AppLocale::ZhCn),
            "delegated child must inherit parent effective_locale, not English default"
        );
        assert_eq!(
            snapshot.parent_working_dir.as_deref(),
            Some(parent_workdir.to_string_lossy().as_ref())
        );
        assert_eq!(snapshot.owner_window_label, "test-window");
        assert_eq!(snapshot.owner_operation_id, None);
        assert_eq!(snapshot.ownership_generation, 0);

        let mut child_rx = mgr
            .insert_test_connection_live(
                child_id,
                AgentType::Codex,
                Some(PathBuf::from("/tmp/deleg-child-locale")),
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state(child_id).await.unwrap();
            let mut s = state.write().await;
            s.purpose = snapshot.launch_context.purpose;
            s.effective_locale = snapshot
                .launch_context
                .inherited_locale
                .unwrap_or(AppLocale::En);
        }

        let parent_conversation = {
            let folder_id = test_helpers::seed_folder(&db, "/tmp/deleg-parent-locale").await;
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent conversation")
        };

        let task = "delegated broker task body".to_string();
        let accepted = spawner
            .send_prompt_linked_for_delegation(
                child_id,
                task.clone(),
                DelegationLink {
                    parent_conversation_id: parent_conversation.id,
                    parent_tool_use_id: "tu-locale".into(),
                    delegation_call_id: "call-locale".into(),
                },
                None,
            )
            .await
            .expect("delegated send");
        let conversation_id = accepted.child_conversation_id;
        assert!(
            accepted.prompt_accepted_at.timestamp() > 0,
            "accepted path must sample prompt_accepted_at"
        );

        // Drain the enqueued command so the receiver stays live for the assert.
        let _ = child_rx.try_recv();

        let job = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .expect("query job")
            .expect("job enrolled");
        assert_eq!(
            job.first_user_text.as_deref(),
            Some(task.as_str()),
            "broker task must be the first-user-text source"
        );
        assert_eq!(
            job.locale.as_deref(),
            Some("zh_cn"),
            "capture locale must resolve to the inherited child locale"
        );
        {
            let state = mgr.get_state(child_id).await.unwrap();
            let s = state.read().await;
            assert_eq!(s.effective_locale, AppLocale::ZhCn);
            assert_eq!(s.purpose, ConnectionPurpose::Delegation);
        }
    }

    /// Accept path samples `prompt_accepted_at` and must not re-read a stale
    /// generation-1 `delegation_started_at` from the conversation row.
    #[tokio::test]
    async fn stale_gen1_conversation_timestamp_not_reread() {
        use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
        use crate::db::test_helpers;
        use chrono::{Duration, Utc};
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let db = Arc::new(test_helpers::fresh_in_memory_db().await);
        let mgr = Arc::new(ConnectionManager::new());
        let child_id = "deleg-child-stale-ts";
        let child_workdir = PathBuf::from("/tmp/deleg-child-stale-ts");
        let mut child_rx = mgr
            .insert_test_connection_live(
                child_id,
                AgentType::Codex,
                Some(child_workdir),
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state(child_id).await.unwrap();
            let mut s = state.write().await;
            s.purpose = ConnectionPurpose::Delegation;
        }

        let folder_id = test_helpers::seed_folder(&db, "/tmp/deleg-stale-ts").await;
        let parent_conversation =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent conversation");

        let stale_at = Utc::now() - Duration::hours(2);
        let child_row = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_conversation.id,
                parent_tool_use_id: "tu-stale-ts".into(),
                delegation_call_id: "call-stale-ts".into(),
            }),
        )
        .await
        .expect("child row");
        {
            let model = conversation::Entity::find_by_id(child_row.id)
                .one(&db.conn)
                .await
                .expect("load child")
                .expect("child exists");
            let mut active: conversation::ActiveModel = model.into();
            active.delegation_started_at = Set(Some(stale_at));
            active
                .update(&db.conn)
                .await
                .expect("seed stale started_at");
        }
        {
            let state = mgr.get_state(child_id).await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = Some(child_row.id);
        }

        let spawner = ConnectionManagerSpawner {
            manager: mgr.clone(),
            db: db.clone(),
            data_dir: Arc::new(PathBuf::from("/tmp")),
            runtime: crate::commands::delegation::DelegationRuntimeSettings::default(),
        };
        let before = Utc::now();
        let accepted = spawner
            .send_prompt_linked_for_delegation(
                child_id,
                "task body for stale timestamp".into(),
                DelegationLink {
                    parent_conversation_id: parent_conversation.id,
                    parent_tool_use_id: "tu-stale-ts".into(),
                    delegation_call_id: "call-stale-ts".into(),
                },
                None,
            )
            .await
            .expect("accept must succeed without conversation timestamp lookup");
        let after = Utc::now();
        assert_eq!(accepted.child_conversation_id, child_row.id);
        assert!(
            accepted.prompt_accepted_at >= before && accepted.prompt_accepted_at <= after,
            "prompt_accepted_at must be freshly sampled, not stale gen1 row value {stale_at}"
        );
        assert_ne!(
            accepted.prompt_accepted_at, stale_at,
            "must not re-read stale conversation.delegation_started_at"
        );
        // Durable row still holds the stale value (promote projects later).
        let row = conversation_service::get_by_id(&db.conn, child_row.id)
            .await
            .expect("reload child");
        assert_eq!(row.delegation_started_at, Some(stale_at));
        let _ = child_rx.try_recv();
    }

    /// Production legacy path (`prebound_child = None`) resolves folder from
    /// the child connection working_dir via RegistrationOnly — must not
    /// ForceOpen an absent path just because a hidden child conversation is
    /// created.
    #[tokio::test]
    async fn manager_legacy_delegation_child_keeps_working_dir_folder_closed() {
        use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
        use crate::db::entities::folder;
        use crate::db::test_helpers;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let db = Arc::new(test_helpers::fresh_in_memory_db().await);
        let mgr = Arc::new(ConnectionManager::new());
        let child_id = "deleg-child-reg-only";
        // Absent path: not pre-seeded open. ensure_folder must create closed.
        let workdir = PathBuf::from("/tmp/codeg-mgr-reg-only-absent");
        let mut child_rx = mgr
            .insert_test_connection_live(
                child_id,
                AgentType::Codex,
                Some(workdir.clone()),
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state(child_id).await.unwrap();
            let mut s = state.write().await;
            s.purpose = ConnectionPurpose::Delegation;
        }

        let parent_folder = test_helpers::seed_folder(&db, "/tmp/codeg-mgr-reg-only-parent").await;
        let parent_conversation = conversation_service::create(
            &db.conn,
            parent_folder,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .expect("parent conversation");

        let spawner = ConnectionManagerSpawner {
            manager: mgr.clone(),
            db: db.clone(),
            data_dir: Arc::new(PathBuf::from("/tmp")),
            runtime: crate::commands::delegation::DelegationRuntimeSettings::default(),
        };

        let accepted = spawner
            .send_prompt_linked_for_delegation(
                child_id,
                "reg-only manager task".into(),
                DelegationLink {
                    parent_conversation_id: parent_conversation.id,
                    parent_tool_use_id: "tu-mgr-reg-only".into(),
                    delegation_call_id: "call-mgr-reg-only".into(),
                },
                None, // legacy path → ensure_folder(RegistrationOnly)
            )
            .await
            .expect("delegated send must succeed");
        assert!(accepted.child_conversation_id > 0);
        let _ = child_rx.try_recv();

        let path = workdir.to_string_lossy().to_string();
        let row = folder::Entity::find()
            .filter(folder::Column::Path.eq(&path))
            .one(&db.conn)
            .await
            .expect("query folder")
            .expect("working_dir folder must exist after legacy reserve");
        assert!(
            !row.is_open,
            "manager legacy child reserve must not ForceOpen working_dir folder"
        );
        assert!(row.deleted_at.is_none());
    }

    /// Pre-existing closed folder stays closed through the same production path.
    #[tokio::test]
    async fn manager_legacy_delegation_child_preserves_closed_working_dir() {
        use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
        use crate::db::entities::folder;
        use crate::db::service::folder_service;
        use crate::db::test_helpers;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let db = Arc::new(test_helpers::fresh_in_memory_db().await);
        let path = "/tmp/codeg-mgr-reg-only-closed";
        let folder_id = test_helpers::seed_folder(&db, path).await;
        folder_service::set_folder_open(&db.conn, folder_id, false)
            .await
            .expect("close folder");
        assert!(
            !folder::Entity::find_by_id(folder_id)
                .one(&db.conn)
                .await
                .expect("load")
                .expect("folder")
                .is_open
        );

        let mgr = Arc::new(ConnectionManager::new());
        let child_id = "deleg-child-reg-only-closed";
        let mut child_rx = mgr
            .insert_test_connection_live(
                child_id,
                AgentType::Codex,
                Some(PathBuf::from(path)),
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state(child_id).await.unwrap();
            let mut s = state.write().await;
            s.purpose = ConnectionPurpose::Delegation;
        }

        let parent_folder =
            test_helpers::seed_folder(&db, "/tmp/codeg-mgr-reg-only-closed-parent").await;
        let parent_conversation = conversation_service::create(
            &db.conn,
            parent_folder,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .expect("parent conversation");

        let spawner = ConnectionManagerSpawner {
            manager: mgr.clone(),
            db: db.clone(),
            data_dir: Arc::new(PathBuf::from("/tmp")),
            runtime: crate::commands::delegation::DelegationRuntimeSettings::default(),
        };

        spawner
            .send_prompt_linked_for_delegation(
                child_id,
                "reg-only closed task".into(),
                DelegationLink {
                    parent_conversation_id: parent_conversation.id,
                    parent_tool_use_id: "tu-mgr-reg-closed".into(),
                    delegation_call_id: "call-mgr-reg-closed".into(),
                },
                None,
            )
            .await
            .expect("delegated send");
        let _ = child_rx.try_recv();

        let row = folder::Entity::find()
            .filter(folder::Column::Path.eq(path))
            .one(&db.conn)
            .await
            .expect("query")
            .expect("folder");
        assert_eq!(row.id, folder_id);
        assert!(
            !row.is_open,
            "manager legacy path must not ForceOpen an already-closed folder"
        );
    }

    #[test]
    fn is_reserved_turn_id_matches_only_the_parser_namespace() {
        // Rejected: the parsers' `turn-<digits>` ids (an untrusted client id of
        // this shape would collide with a persisted transcript turn).
        assert!(is_reserved_turn_id("turn-0"));
        assert!(is_reserved_turn_id("turn-42"));
        // Accepted: anything else, including the real UI sender id shape and the
        // connection-scoped fallback shape.
        assert!(!is_reserved_turn_id("optimistic-9f3c1a2b"));
        assert!(!is_reserved_turn_id("user-conn-7"));
        assert!(!is_reserved_turn_id("turn-")); // no number
        assert!(!is_reserved_turn_id("turn-1a")); // not all digits
        assert!(!is_reserved_turn_id("turnabout-1"));
        assert!(!is_reserved_turn_id(""));
    }

    fn fake_connection(id: &str, conv_id: Option<i32>) -> AgentConnection {
        let (tx, _rx, _liveness_rx) = connection_channel(1);
        let mut state = SessionState::new(
            id.to_string(),
            crate::models::agent::AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = conv_id;
        state.status = ConnectionStatus::Connected;
        AgentConnection {
            id: id.to_string(),
            agent_type: crate::models::agent::AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation: state.connection_incarnation.clone(),
            tool_lease_registry: state.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .0,
            observed_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .1,
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Spawn a two-level process tree: `sh` (the stand-in for the agent CLI)
    /// backgrounds a `sleep` grandchild (the stand-in for the agent's own
    /// children — an MCP server, a forked `node`) and records its pid. The
    /// grandchild is what the backstop assertions are about: it is the process
    /// that gets reparented and lingers when only the direct child is killed.
    ///
    /// Returns the direct child — keep it alive, dropping a `Child` does NOT
    /// kill it — and the grandchild pid.
    #[cfg(unix)]
    async fn spawn_process_tree(pidfile: &std::path::Path) -> (std::process::Child, i32) {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "sleep 30 & echo $! > '{}'; wait",
                pidfile.display()
            ))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh");
        for _ in 0..150 {
            if let Ok(raw) = std::fs::read_to_string(pidfile) {
                if let Ok(pid) = raw.trim().parse::<i32>() {
                    return (child, pid);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Bail-out path still reaps the tree, so a failing test can't leave a
        // `sleep` behind for 30s.
        let _ = kill_tree::blocking::kill_tree(child.id());
        let _ = child.wait();
        panic!("grandchild never recorded its pid");
    }

    /// True once `pid` is gone. `kill(pid, 0)` sends no signal, it only probes
    /// existence; polling keeps the assertion from racing kernel teardown.
    #[cfg(unix)]
    async fn wait_until_dead(pid: i32) -> bool {
        for _ in 0..150 {
            // SAFETY: signal 0 only probes for existence; it sends no signal.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[cfg(unix)]
    fn is_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only probes for existence; it sends no signal.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Quit has to kill the agent's whole process TREE. Killing just the direct
    /// child leaves its children reparented and lingering — that orphan window
    /// is the entire reason the backstop exists.
    ///
    /// The test connection's command receiver is dropped, so the graceful path
    /// is unavailable and the kill can only come from the backstop.
    /// Unix-only (relies on `sh` / `kill(2)`).
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_all_backstop_kills_the_whole_agent_process_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        let conn = fake_connection("conn-tree", None);
        conn.child_pid
            .store(child.id(), std::sync::atomic::Ordering::SeqCst);
        mgr.connections
            .lock()
            .await
            .insert("conn-tree".to_string(), conn);

        assert_eq!(
            mgr.disconnect_all(AcpDisconnectOrigin::ApplicationShutdown)
                .await,
            1
        );
        assert!(
            wait_until_dead(gpid).await,
            "grandchild {gpid} survived — the quit backstop did not kill the tree"
        );
        let _ = child.wait();
    }

    /// A connection still `Connecting` when quit begins publishes its pid AFTER
    /// the map is drained. Reading the pids up front would see `0` there and
    /// skip it, leaking exactly the orphan this exists to kill — so the load
    /// has to happen after the grace window, from the live cell.
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_all_backstop_reaches_a_child_that_spawns_during_the_grace_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        let conn = fake_connection("conn-late", None);
        // Still 0 at drain time, exactly like a connection whose agent process
        // hasn't launched yet.
        let cell = Arc::clone(&conn.child_pid);
        mgr.connections
            .lock()
            .await
            .insert("conn-late".to_string(), conn);

        let pid = child.id();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cell.store(pid, std::sync::atomic::Ordering::SeqCst);
        });

        assert_eq!(
            mgr.disconnect_all(AcpDisconnectOrigin::ApplicationShutdown)
                .await,
            1
        );
        assert!(
            wait_until_dead(gpid).await,
            "grandchild {gpid} survived — a pid published during the grace window was missed"
        );
        let _ = child.wait();
    }

    /// The mirror image: once the agent process has been reaped, the `on_exit`
    /// callback zeroes the cell and the backstop must leave that pid alone.
    /// Without the clear, a quit fires `kill_tree` at a pid whose process is
    /// already dead and reaped — and if the OS recycled that number, the victim
    /// is an unrelated process tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_all_backstop_leaves_a_cleared_pid_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        let conn = fake_connection("conn-cleared", None);
        conn.child_pid
            .store(child.id(), std::sync::atomic::Ordering::SeqCst);
        let cell = Arc::clone(&conn.child_pid);
        mgr.connections
            .lock()
            .await
            .insert("conn-cleared".to_string(), conn);

        // Stands in for the driver unwinding mid-window: the process is gone
        // and the guard has zeroed the cell.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cell.store(0, std::sync::atomic::Ordering::SeqCst);
        });

        assert_eq!(
            mgr.disconnect_all(AcpDisconnectOrigin::ApplicationShutdown)
                .await,
            1
        );
        // Settle: a wrongly-issued SIGTERM would have landed by now.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            is_alive(gpid),
            "backstop killed a tree whose pid the driver had already cleared"
        );

        let _ = kill_tree::blocking::kill_tree(child.id());
        let _ = child.wait();
    }

    /// Build a broadcaster + subscribed receiver. Subscribing here (not lazily
    /// inside the test) ensures events emitted between construction and the
    /// first `recv` are buffered rather than dropped.
    fn make_test_broadcaster() -> (Arc<WebEventBroadcaster>, broadcast::Receiver<WebEvent>) {
        let bcast = Arc::new(WebEventBroadcaster::new());
        let rx = bcast.subscribe();
        (bcast, rx)
    }

    /// Thin wrapper around `ConnectionManager::insert_test_connection` so the
    /// existing in-crate tests keep their `insert_fake_connection(mgr, ...)`
    /// call shape after the public test helper landed.
    async fn insert_fake_connection(
        mgr: &ConnectionManager,
        id: &str,
        agent_type: crate::models::agent::AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) {
        mgr.insert_test_connection(id, agent_type, working_dir, emitter)
            .await;
    }

    /// Production CancelHost is reachable and full-stamp wait cancel works.
    #[tokio::test]
    async fn production_cancel_host_wait_uses_full_stamp_and_cause() {
        use crate::acp::connection::{connection_channel, ConnectionControl};
        use crate::acp::delegation::wait_cancel::{
            cancel_cause_of, cancel_flag_set, new_wait_cancel_channel,
        };
        use crate::acp::tool_watchdog::{
            wait_stamp_from_lease, CancelCause, CancelHost, WaitCancelHandle, WaitOwner,
            WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "prod-wait-full-stamp";
        // Live control lane so CancelTurn can be delivered.
        let (control_tx, mut control_rx, _control_liveness) =
            connection_channel::<ConnectionControl>(4);
        {
            use crate::acp::session_state::SessionState;
            let (cmd_tx, _cmd_rx, _) = connection_channel(1);
            let mut state = SessionState::new(
                conn_id.to_string(),
                AgentType::ClaudeCode,
                None,
                "test-window".to_string(),
                None,
            );
            state.status = ConnectionStatus::Connected;
            state.tool_lease_registry = mgr.tool_lease_registry.clone();
            state.mcp_cancel_registry = mgr.mcp_cancel_registry.clone();
            state.conversation_id = Some(42);
            state.active_turn_generation = Some(3);
            state.turn_in_flight = true;
            let incarnation = state.connection_incarnation.clone();
            let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
            let route_plan = crate::acp::delegation::route::test_empty_route_plan();
            let (spawn_config, observed_config) = matching_config_pair(
                String::new(),
                terminal_shell.selection_key.clone(),
                route_plan.fingerprint.clone(),
            );
            let conn = AgentConnection {
                id: conn_id.to_string(),
                agent_type: AgentType::ClaudeCode,
                status: ConnectionStatus::Connected,
                owner_window_label: "test-window".to_string(),
                owner_operation_id: None,
                ownership_generation: 0,
                connection_incarnation: incarnation,
                tool_lease_registry: mgr.tool_lease_registry.clone(),
                parent_connection_id: None,
                cmd_tx,
                control_tx,
                task_abort: None,
                state: Arc::new(tokio::sync::RwLock::new(state)),
                emitter: EventEmitter::Noop,
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell,
                route_plan,
                origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
                route_preference: None,
                route_capability:
                    crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            };
            mgr.connections
                .lock()
                .await
                .insert(conn_id.to_string(), conn);
        }

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };

        // Register a wait with the full stamp the host will rebuild from the lease.
        let wait_id = "wait-prod-1";
        let lease_stamp = crate::acp::tool_watchdog::LeaseStamp {
            lease_id: "lease-wait".into(),
            version: 1,
            connection_id: conn_id.into(),
            connection_incarnation: incarnation.clone(),
            turn_generation: 3,
            tool_call_id: Some("tool-status".into()),
        };
        let full = wait_stamp_from_lease(&lease_stamp, wait_id, 42);
        let (tx, rx) = new_wait_cancel_channel();
        mgr.wait_cancel_registry()
            .register(WaitCancelHandle {
                stamp: full.clone(),
                owner: WaitOwner::Listener,
                cancel: tx,
                task_ids: vec![],
            })
            .await
            .unwrap();

        // Reduced stamp must fail (stale).
        let reduced = crate::acp::tool_watchdog::WaitStamp {
            parent_tool_use_id: None,
            ..full.clone()
        };
        assert_eq!(
            mgr.wait_cancel_registry()
                .cancel(&reduced, CancelCause::AutoTimeout)
                .await,
            crate::acp::tool_watchdog::WaitCancelResult::Stale
        );
        assert!(!cancel_flag_set(&rx));

        // Manager path with UserStop cause (full stamp).
        mgr.cancel_delegation_wait_if_verified(&lease_stamp, wait_id, CancelCause::UserStop)
            .await
            .expect("full stamp cancel");
        assert!(cancel_flag_set(&rx));
        assert_eq!(cancel_cause_of(&rx), Some(CancelCause::UserStop));

        // Production host: CancelTurn (not unqualified Cancel) with AutoTimeout.
        let host = mgr.production_cancel_host();
        let turn_stamp = crate::acp::tool_watchdog::LeaseStamp {
            lease_id: "lease-turn".into(),
            version: 1,
            connection_id: conn_id.into(),
            connection_incarnation: incarnation,
            turn_generation: 3,
            tool_call_id: None,
        };
        host.cancel_turn(&turn_stamp, CancelCause::AutoTimeout)
            .await
            .expect("cancel turn accepted");
        match control_rx.recv().await {
            Some(ConnectionControl::CancelTurn {
                turn_generation: 3,
                cause: CancelCause::AutoTimeout,
            }) => {}
            Some(ConnectionControl::Cancel) => {
                panic!("expected CancelTurn, got unqualified Cancel")
            }
            Some(_) => panic!("expected CancelTurn AutoTimeout, got other control"),
            None => panic!("control channel closed before CancelTurn"),
        }

        // Stale generation rejected before send.
        let stale = crate::acp::tool_watchdog::LeaseStamp {
            turn_generation: 99,
            ..turn_stamp.clone()
        };
        assert!(host
            .cancel_turn(&stale, CancelCause::AutoTimeout)
            .await
            .is_err());

        // scan_and_execute with no overdue leases is a no-op.
        let report = mgr
            .scan_and_execute_cancellations(
                WatchdogInstant {
                    mono: Instant::now(),
                    wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                },
                Duration::from_millis(10),
            )
            .await;
        assert_eq!(report.escalations_spawned, 0);
    }

    /// Multiple ClaimCancel actions spawn escalations without awaiting them so
    /// the scan stays responsive (one stuck cancel cannot block the loop).
    #[tokio::test]
    async fn scan_and_execute_cancellations_runs_escalations_concurrently() {
        use crate::acp::tool_watchdog::{
            CancellationCapability, RegisterTool, ToolCategory, TurnStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        // Tiny grace so scan claims immediately after warning publish.
        mgr.tool_lease_registry
            .apply_settings(crate::acp::tool_watchdog::ToolWatchdogSettings {
                enabled: true,
                warning_after_seconds: 60,
                grace_seconds: 60,
            })
            .await;

        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = TurnStamp {
            connection_id: "scan-concurrent".into(),
            connection_incarnation: "inc".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        mgr.tool_lease_registry.start_turn(turn.clone(), t0).await;
        for tool in ["a", "b", "c"] {
            let stamp = mgr
                .tool_lease_registry
                .register_tool(RegisterTool {
                    turn: turn.clone(),
                    tool_call_id: tool.into(),
                    category: ToolCategory::Other,
                    at: t0,
                })
                .await
                .unwrap();
            let _ = mgr
                .tool_lease_registry
                .bind_capability(&stamp, CancellationCapability::Turn)
                .await;
        }

        // Advance past warning + full grace so scan claims all three.
        let warn_at = t0.advanced(60);
        let actions = mgr.tool_lease_registry.scan(warn_at).await;
        for action in actions {
            if let crate::acp::tool_watchdog::RegistryAction::PublishWarning { stamp, .. } = action
            {
                let _ = mgr
                    .tool_lease_registry
                    .warning_published(&stamp.lease_id, stamp.version, warn_at)
                    .await;
            }
        }
        let cancel_at = warn_at.advanced(60);

        // Scan must return after spawning, not after convergence budgets complete.
        let t_start = Instant::now();
        let report = mgr
            .scan_and_execute_cancellations(cancel_at, Duration::from_millis(200))
            .await;
        let elapsed = t_start.elapsed();
        assert_eq!(
            report.escalations_spawned, 3,
            "all three ClaimCancel actions must be spawned"
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "scan must not await escalations; took {elapsed:?}"
        );
        // Give background tasks a moment to start (spawn independent, not joined).
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    /// Production scan must call `warning_published` so leases enter Grace and a
    /// later scan can ClaimCancel. Two controlled-clock passes — never warn+cancel
    /// on the same pass.
    #[tokio::test]
    async fn scan_and_execute_advances_warning_to_grace_then_claim_cancel() {
        use crate::acp::tool_watchdog::{
            CancellationCapability, RegisterTool, ToolCategory, ToolLeasePhase, ToolWatchdogPhase,
            ToolWatchdogSettings, TurnStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        mgr.tool_lease_registry
            .apply_settings(ToolWatchdogSettings {
                enabled: true,
                warning_after_seconds: 60,
                grace_seconds: 60,
            })
            .await;

        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = TurnStamp {
            connection_id: "scan-warn-grace-cancel".into(),
            connection_incarnation: "inc".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        mgr.tool_lease_registry.start_turn(turn.clone(), t0).await;
        let stamp = mgr
            .tool_lease_registry
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-overdue".into(),
                category: ToolCategory::Other,
                at: t0,
            })
            .await
            .unwrap();
        let _ = mgr
            .tool_lease_registry
            .bind_capability(&stamp, CancellationCapability::Turn)
            .await;

        // Pass 1: past warning threshold → PublishWarning → warning_published → Grace.
        // Must not ClaimCancel on the same pass.
        let warn_at = t0.advanced(60);
        let report1 = mgr
            .scan_and_execute_cancellations(warn_at, Duration::from_millis(10))
            .await;
        assert_eq!(
            report1.warnings.len(),
            1,
            "first scan must publish/acknowledge the warning"
        );
        assert_eq!(report1.warnings[0].1.phase, ToolWatchdogPhase::Grace);
        assert_eq!(
            report1.escalations_spawned, 0,
            "must not cancel on the same pass as warning"
        );
        assert_eq!(
            mgr.tool_lease_registry.lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Grace),
            "lease must enter Grace after production scan acknowledges warning"
        );

        // Still inside grace: no claim.
        let mid = mgr
            .scan_and_execute_cancellations(warn_at.advanced(59), Duration::from_millis(10))
            .await;
        assert!(mid.warnings.is_empty());
        assert_eq!(mid.escalations_spawned, 0, "no cancel before grace ends");

        // Pass 2: past grace deadline → ClaimCancel → spawn escalate.
        let cancel_at = warn_at.advanced(60);
        let report2 = mgr
            .scan_and_execute_cancellations(cancel_at, Duration::from_millis(30))
            .await;
        assert!(
            report2.warnings.is_empty(),
            "second scan must not re-warn a Grace lease"
        );
        assert_eq!(
            report2.escalations_spawned, 1,
            "second scan must spawn ClaimCancel after Grace"
        );
    }

    /// User cancel must return the atomic Cancelling projection even when a
    /// concurrent complete settles/removes the lease before cancel returns.
    #[tokio::test]
    async fn user_cancel_returns_claim_projection_when_complete_races() {
        use crate::acp::tool_watchdog::{
            CancellationCapability, RegisterTool, ToolCategory, ToolLeaseKey, ToolWatchdogPhase,
            ToolWatchdogSettings, TurnStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        mgr.tool_lease_registry
            .apply_settings(ToolWatchdogSettings {
                enabled: true,
                warning_after_seconds: 60,
                grace_seconds: 60,
            })
            .await;

        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = TurnStamp {
            connection_id: "cancel-race".into(),
            connection_incarnation: "inc".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        mgr.tool_lease_registry.start_turn(turn.clone(), t0).await;
        let stamp = mgr
            .tool_lease_registry
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-race".into(),
                category: ToolCategory::Other,
                at: t0,
            })
            .await
            .unwrap();
        let _ = mgr
            .tool_lease_registry
            .bind_capability(&stamp, CancellationCapability::Turn)
            .await;
        let warn_at = t0.advanced(60);
        let actions = mgr.tool_lease_registry.scan(warn_at).await;
        for action in actions {
            if let crate::acp::tool_watchdog::RegistryAction::PublishWarning { stamp, .. } = action
            {
                let _ = mgr
                    .tool_lease_registry
                    .warning_published(&stamp.lease_id, stamp.version, warn_at)
                    .await;
            }
        }
        let grace = mgr
            .tool_lease_registry
            .live_projection(&stamp.lease_id)
            .await
            .expect("grace");
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        let lease_id = grace.lease_id.clone();
        let version = grace.version;
        let key = ToolLeaseKey {
            connection_id: turn.connection_id.clone(),
            connection_incarnation: turn.connection_incarnation.clone(),
            turn_generation: turn.turn_generation,
            tool_call_id: "tool-race".into(),
        };
        let reg = mgr.tool_lease_registry.clone();
        let complete_task = async move {
            loop {
                if reg.lease_phase(&lease_id).await.is_some_and(|p| {
                    matches!(p, crate::acp::tool_watchdog::ToolLeasePhase::Cancelling)
                }) {
                    let _ = reg.complete_tool(&key).await;
                    return;
                }
                tokio::task::yield_now().await;
            }
        };
        let cancel_task = mgr.tool_watchdog_user_cancel(&grace.lease_id, version);
        let (cancel_result, _) = tokio::join!(cancel_task, complete_task);
        let projection = cancel_result.expect("successful claim must not map to stale");
        assert_eq!(projection.phase, ToolWatchdogPhase::Cancelling);
        assert_eq!(projection.version, version + 1);
    }

    /// Production escalate path records cancellation_failure from real host
    /// operation outcomes (missing connection → turn/disconnect fail).
    #[tokio::test]
    async fn escalate_records_cancellation_failure_from_host_outcomes() {
        use crate::acp::tool_watchdog::{
            CancelCause, CancellationCapability, RegisterTool, ToolCategory, TurnStamp,
            WatchdogInstant, WatchdogMetricLabel,
        };
        use chrono::{DateTime, Utc};
        use std::time::Duration;
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = TurnStamp {
            connection_id: "no-such-conn".into(),
            connection_incarnation: "inc".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        mgr.tool_lease_registry.start_turn(turn.clone(), t0).await;
        let stamp = mgr
            .tool_lease_registry
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-fail".into(),
                category: ToolCategory::Other,
                at: t0,
            })
            .await
            .unwrap();
        let stamp = mgr
            .tool_lease_registry
            .bind_capability(&stamp, CancellationCapability::Turn)
            .await
            .unwrap();
        let (claim, _) = mgr
            .tool_lease_registry
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::AutoTimeout)
            .await
            .unwrap();

        let before = mgr.tool_watchdog_metrics.snapshot();
        let report = mgr
            .escalate_claimed_lease(&claim, Duration::from_millis(20))
            .await;
        assert!(report.turn_failed || report.disconnect_failed);
        assert!(report.had_operation_failure());
        mgr.tool_watchdog_metrics
            .record_escalation(WatchdogMetricLabel::new(None, ToolCategory::Other), &report);
        let after = mgr.tool_watchdog_metrics.snapshot();
        assert_eq!(
            after.cancellation_failure_total,
            before.cancellation_failure_total + 1
        );
        assert!(
            after.turn_fallback_total + after.disconnect_fallback_total
                > before.turn_fallback_total + before.disconnect_fallback_total
        );
    }

    /// Saturated control lane must not hang turn-cancel forever: admit times out,
    /// escalation reaches disconnect/settlement, and cancellation_failure advances.
    #[tokio::test]
    async fn saturated_turn_control_lane_escalation_terminates_with_failure_metric() {
        use crate::acp::connection::{connection_channel, ConnectionControl};
        use crate::acp::tool_watchdog::{
            CancelCause, CancellationCapability, RegisterTool, ToolCategory, TurnStamp,
            WatchdogInstant, WatchdogMetricLabel, CONTROL_LANE_ADMIT_TIMEOUT,
        };
        use chrono::{DateTime, Utc};
        use std::time::Duration;
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "sat-turn-control";
        // Capacity 1, no consumer: fill so CancelTurn admission blocks without a bound.
        let (control_tx, _control_rx, _control_liveness) =
            connection_channel::<ConnectionControl>(1);
        control_tx
            .try_send(ConnectionControl::Cancel)
            .expect("prime full control lane");
        {
            use crate::acp::session_state::SessionState;
            let (cmd_tx, _cmd_rx, _) = connection_channel(1);
            let mut state = SessionState::new(
                conn_id.to_string(),
                AgentType::ClaudeCode,
                None,
                "test-window".to_string(),
                None,
            );
            state.status = ConnectionStatus::Connected;
            state.tool_lease_registry = mgr.tool_lease_registry.clone();
            state.mcp_cancel_registry = mgr.mcp_cancel_registry.clone();
            state.conversation_id = Some(7);
            state.active_turn_generation = Some(1);
            state.turn_in_flight = true;
            let incarnation = state.connection_incarnation.clone();
            let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
            let route_plan = crate::acp::delegation::route::test_empty_route_plan();
            let (spawn_config, observed_config) = matching_config_pair(
                String::new(),
                terminal_shell.selection_key.clone(),
                route_plan.fingerprint.clone(),
            );
            let conn = AgentConnection {
                id: conn_id.to_string(),
                agent_type: AgentType::ClaudeCode,
                status: ConnectionStatus::Connected,
                owner_window_label: "test-window".to_string(),
                owner_operation_id: None,
                ownership_generation: 0,
                connection_incarnation: incarnation,
                tool_lease_registry: mgr.tool_lease_registry.clone(),
                parent_connection_id: None,
                cmd_tx,
                control_tx,
                task_abort: None,
                state: Arc::new(tokio::sync::RwLock::new(state)),
                emitter: EventEmitter::Noop,
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell,
                route_plan,
                origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
                route_preference: None,
                route_capability:
                    crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            };
            mgr.connections
                .lock()
                .await
                .insert(conn_id.to_string(), conn);
        }

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };

        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-23T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = TurnStamp {
            connection_id: conn_id.into(),
            connection_incarnation: incarnation,
            session_id: "sess".into(),
            turn_generation: 1,
        };
        mgr.tool_lease_registry.start_turn(turn.clone(), t0).await;
        let stamp = mgr
            .tool_lease_registry
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: "tool-sat".into(),
                category: ToolCategory::Other,
                at: t0,
            })
            .await
            .unwrap();
        let stamp = mgr
            .tool_lease_registry
            .bind_capability(&stamp, CancellationCapability::Turn)
            .await
            .unwrap();
        let (claim, _) = mgr
            .tool_lease_registry
            .claim_cancel(&stamp.lease_id, stamp.version, CancelCause::AutoTimeout)
            .await
            .unwrap();

        let before = mgr.tool_watchdog_metrics.snapshot();
        let convergence = Duration::from_millis(40);
        // Must finish: admit timeout(s) + short convergence + disconnect admit.
        let outer = CONTROL_LANE_ADMIT_TIMEOUT * 4 + convergence + Duration::from_millis(500);
        let started = std::time::Instant::now();
        let report = tokio::time::timeout(outer, mgr.escalate_claimed_lease(&claim, convergence))
            .await
            .expect("escalation must terminate when control lane is saturated");
        assert!(
            started.elapsed() < outer,
            "escalation wall time must stay bounded"
        );
        assert!(
            report.turn_failed,
            "CancelTurn admit timeout must mark turn stage failed"
        );
        assert!(report.had_operation_failure());
        assert_eq!(
            report.stage,
            crate::acp::tool_watchdog::EscalationStage::Disconnect,
            "must continue to disconnect/settlement after turn admit failure"
        );
        assert!(
            !mgr.tool_lease_registry.is_live(&claim.stamp.lease_id).await,
            "lease must settle rather than strand as Cancelling"
        );

        mgr.tool_watchdog_metrics.record_escalation(
            WatchdogMetricLabel::new(Some(AgentType::ClaudeCode), ToolCategory::Other),
            &report,
        );
        let after = mgr.tool_watchdog_metrics.snapshot();
        assert_eq!(
            after.cancellation_failure_total,
            before.cancellation_failure_total + 1
        );
    }

    /// Manager disconnect must clear the registry before map removal is visible
    /// so a concurrent scan never observes map-missing + lease-live.
    #[tokio::test]
    async fn disconnect_clears_registry_before_map_invisible_to_scan() {
        use crate::acp::tool_watchdog::{
            turn_stamp, LeaseAttribution, ToolCategory, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "watchdog-disc-race";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp(conn_id, &incarnation, "sess-1", 1);
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-race", ToolCategory::Other, t0)
            .await
            .expect("register lease");
        let lease_id = stamp.lease_id.clone();

        let violated = Arc::new(AtomicBool::new(false));
        let mgr_scan = mgr.clone_ref();
        let reg = mgr.tool_lease_registry();
        let flag = Arc::clone(&violated);
        let conn_id_scan = conn_id.to_string();
        let scanner = tokio::spawn(async move {
            for _ in 0..50_000 {
                let in_map = mgr_scan.get_state(&conn_id_scan).await.is_some();
                let lease_alive = reg.lease_phase(&lease_id).await.is_some();
                // Forbidden window: routing gone while incarnation lease still live.
                if !in_map && lease_alive {
                    flag.store(true, Ordering::SeqCst);
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        mgr.disconnect(conn_id)
            .await
            .expect("disconnect must succeed");

        // After disconnect returns both map and lease must be clear.
        assert!(mgr.get_state(conn_id).await.is_none());
        assert!(attr.registry().lease_phase(&stamp.lease_id).await.is_none());
        let scan_actions = attr.registry().scan(t0.advanced(10_000)).await;
        assert!(
            scan_actions.is_empty(),
            "scan after manager disconnect must not act on the incarnation"
        );

        // Give the concurrent scanner a moment to finish its last iteration.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        scanner.abort();
        let _ = scanner.await;
        assert!(
            !violated.load(Ordering::SeqCst),
            "scan must never observe map-invisible + lease-live during disconnect"
        );
    }

    /// Task 5 r3 I1: fence admission before map remove so a late tool event
    /// (register after clear, before Disconnect is observed) cannot recreate
    /// a map-invisible live lease.
    #[tokio::test]
    async fn disconnect_fences_admission_before_late_tool_reregister() {
        use crate::acp::tool_watchdog::{
            turn_stamp, LeaseAttribution, ToolCategory, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "watchdog-disc-fence";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp(conn_id, &incarnation, "sess-1", 1);
        attr.start_turn(turn.clone(), t0).await;
        let _ = attr
            .register_or_touch_tool(&turn, "tool-pre", ToolCategory::Other, t0)
            .await
            .expect("register");

        let resurrected = Arc::new(AtomicBool::new(false));
        let reg = mgr.tool_lease_registry();
        let flag = Arc::clone(&resurrected);
        let turn_late = turn.clone();
        // Simulate connection-loop tool event racing disconnect: repeatedly try
        // to re-register after clear while map may still be present.
        let racer = tokio::spawn(async move {
            let attr = LeaseAttribution::new(reg);
            for _ in 0..50_000 {
                if attr
                    .register_or_touch_tool(
                        &turn_late,
                        "tool-after-fence",
                        ToolCategory::Other,
                        t0.advanced(1),
                    )
                    .await
                    .is_some()
                {
                    // Only count as violation if incarnation is already fenced
                    // (clear started) — a success before fence is the initial window.
                    if attr
                        .registry()
                        .is_fenced(&turn_late.connection_id, &turn_late.connection_incarnation)
                        .await
                    {
                        flag.store(true, Ordering::SeqCst);
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
        });

        mgr.disconnect(conn_id)
            .await
            .expect("disconnect must succeed");

        assert!(
            attr.registry().is_fenced(conn_id, &incarnation).await,
            "disconnect must fence the incarnation"
        );
        assert!(
            attr.register_or_touch_tool(
                &turn,
                "tool-post-disconnect",
                ToolCategory::Other,
                t0.advanced(2),
            )
            .await
            .is_none(),
            "post-disconnect register must no-op"
        );
        let actions = attr.registry().scan(t0.advanced(10_000)).await;
        assert!(
            actions.is_empty(),
            "no actionable leases after fenced disconnect: {actions:?}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        racer.abort();
        let _ = racer.await;
        assert!(
            !resurrected.load(Ordering::SeqCst),
            "tool event after fence must not recreate a lease"
        );
    }

    /// Exact-match bind: pre-created wait-tool lease + full stamp → Bound.
    /// Never invents a lease via `register_or_touch_tool`.
    #[tokio::test]
    async fn bind_delegation_wait_binds_exact_precreated_lease() {
        use crate::acp::delegation::listener::ParentSessionLookup;
        use crate::acp::tool_watchdog::{
            classify_tool_category, tool_lease_key, turn_stamp, BindDelegationWaitResult,
            CancellationCapability, LeaseAttribution, WaitStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "wait-bind-exact";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        {
            let state = mgr.get_state(conn_id).await.expect("state");
            let mut s = state.write().await;
            s.conversation_id = Some(42);
            s.external_id = Some("sess-wait".into());
            s.active_turn_generation = Some(1);
            s.turn_in_flight = true;
        }

        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp(conn_id, &incarnation, "sess-wait", 1);
        attr.start_turn(turn.clone(), t0).await;
        let outcome = attr
            .register_or_touch_tool(
                &turn,
                "status-wait-tool",
                classify_tool_category("other", Some("delegation")),
                t0,
            )
            .await
            .expect("pre-create wait tool lease");
        let _ = outcome;

        let expected = WaitStamp {
            wait_id: "wait-1".into(),
            connection_id: conn_id.into(),
            connection_incarnation: incarnation.clone(),
            turn_generation: 1,
            parent_conversation_id: 42,
            parent_tool_use_id: Some("status-wait-tool".into()),
        };
        let lookup = ConnectionManagerParentLookup {
            manager: Arc::new(mgr.clone_ref()),
        };
        assert_eq!(
            lookup.bind_delegation_wait(conn_id, &expected).await,
            BindDelegationWaitResult::Bound
        );
        let lease = attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "status-wait-tool"))
            .await
            .expect("lease still live");
        assert_eq!(
            attr.registry().lease_capability(&lease.lease_id).await,
            Some(CancellationCapability::DelegationWait {
                wait_id: "wait-1".into()
            })
        );
    }

    /// Absent lease → WaitToolLeaseMismatch (no register_or_touch invent).
    #[tokio::test]
    async fn bind_delegation_wait_absent_lease_is_mismatch_not_invent() {
        use crate::acp::delegation::listener::ParentSessionLookup;
        use crate::acp::tool_watchdog::{
            tool_lease_key, turn_stamp, BindDelegationWaitResult, LeaseAttribution, WaitStamp,
            WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "wait-bind-absent";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        {
            let state = mgr.get_state(conn_id).await.expect("state");
            let mut s = state.write().await;
            s.conversation_id = Some(7);
            s.external_id = Some("sess".into());
            s.active_turn_generation = Some(1);
            s.turn_in_flight = true;
        }
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp(conn_id, &incarnation, "sess", 1);
        attr.start_turn(turn.clone(), t0).await;

        let expected = WaitStamp {
            wait_id: "wait-1".into(),
            connection_id: conn_id.into(),
            connection_incarnation: incarnation,
            turn_generation: 1,
            parent_conversation_id: 7,
            parent_tool_use_id: Some("missing-wait-tool".into()),
        };
        let lookup = ConnectionManagerParentLookup {
            manager: Arc::new(mgr.clone_ref()),
        };
        assert_eq!(
            lookup.bind_delegation_wait(conn_id, &expected).await,
            BindDelegationWaitResult::WaitToolLeaseMismatch
        );
        assert!(
            attr.registry()
                .tool_stamp(&tool_lease_key(&turn, "missing-wait-tool"))
                .await
                .is_none(),
            "bind must not invent a lease"
        );
    }

    /// Reused tool id across turns: older stamp vs newer live turn → WaitStampStale.
    #[tokio::test]
    async fn bind_delegation_wait_stale_turn_rejects() {
        use crate::acp::delegation::listener::ParentSessionLookup;
        use crate::acp::tool_watchdog::{
            classify_tool_category, turn_stamp, BindDelegationWaitResult, LeaseAttribution,
            WaitStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "wait-bind-stale";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        {
            let state = mgr.get_state(conn_id).await.expect("state");
            let mut s = state.write().await;
            s.conversation_id = Some(9);
            s.external_id = Some("sess".into());
            s.active_turn_generation = Some(2);
            s.turn_in_flight = true;
        }
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        // Live turn gen 2 with a lease for the reused tool id.
        let turn = turn_stamp(conn_id, &incarnation, "sess", 2);
        attr.start_turn(turn.clone(), t0).await;
        attr.register_or_touch_tool(
            &turn,
            "reused-tool",
            classify_tool_category("other", Some("delegation")),
            t0,
        )
        .await
        .expect("lease");

        // Expected stamp is from turn gen 1 (stale).
        let expected = WaitStamp {
            wait_id: "wait-old".into(),
            connection_id: conn_id.into(),
            connection_incarnation: incarnation,
            turn_generation: 1,
            parent_conversation_id: 9,
            parent_tool_use_id: Some("reused-tool".into()),
        };
        let lookup = ConnectionManagerParentLookup {
            manager: Arc::new(mgr.clone_ref()),
        };
        assert_eq!(
            lookup.bind_delegation_wait(conn_id, &expected).await,
            BindDelegationWaitResult::WaitStampStale
        );
    }

    /// Blank/missing tool id → WaitToolIdMissing; never scans active_tool_calls.
    #[tokio::test]
    async fn bind_delegation_wait_missing_tool_id_no_scan() {
        use crate::acp::delegation::listener::ParentSessionLookup;
        use crate::acp::tool_watchdog::{
            turn_stamp, BindDelegationWaitResult, LeaseAttribution, WaitStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        let conn_id = "wait-bind-missing-id";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        {
            let state = mgr.get_state(conn_id).await.expect("state");
            let mut s = state.write().await;
            s.conversation_id = Some(1);
            s.external_id = Some("sess".into());
            s.active_turn_generation = Some(1);
            s.turn_in_flight = true;
            // Seed a status-looking tool so a scan would find it — must not.
            let mut scannable = crate::acp::session_state::ToolCallState::default();
            scannable.id = "scannable-status".into();
            scannable.kind = crate::acp::session_state::ToolKind::Other;
            scannable.label = "codeg-mcp__get_delegation_status".into();
            scannable.status = crate::acp::session_state::ToolCallStatus::InProgress;
            s.active_tool_calls
                .insert("scannable-status".into(), scannable);
        }
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        attr.start_turn(turn_stamp(conn_id, &incarnation, "sess", 1), t0)
            .await;

        let expected = WaitStamp {
            wait_id: "wait-1".into(),
            connection_id: conn_id.into(),
            connection_incarnation: incarnation,
            turn_generation: 1,
            parent_conversation_id: 1,
            parent_tool_use_id: None,
        };
        let lookup = ConnectionManagerParentLookup {
            manager: Arc::new(mgr.clone_ref()),
        };
        assert_eq!(
            lookup.bind_delegation_wait(conn_id, &expected).await,
            BindDelegationWaitResult::WaitToolIdMissing
        );
    }

    /// Identity-less rewrite tool ids may carry host padding. resolve_wait_tool_id
    /// must keep those original bytes so bind/lease lookup and renewal targets
    /// align on the same opaque key (trim only rejects blank).
    #[tokio::test]
    async fn padded_rewrite_tool_id_bind_and_renewal_align_lease_keys() {
        use crate::acp::delegation::listener::ParentSessionLookup;
        use crate::acp::delegation::wait_cancel::{new_wait_cancel_channel, WaitCancelRegistry};
        use crate::acp::tool_watchdog::{
            classify_tool_category, tool_lease_key, turn_stamp, BindDelegationWaitResult,
            CancellationCapability, LeaseAttribution, ToolLeasePhase, WaitCancelHandle, WaitOwner,
            WaitStamp, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        // Host rewrite id with surrounding whitespace (opaque; not trimmed).
        let padded_rewrite = "  rewrite-status-padded  ";
        assert_ne!(padded_rewrite, padded_rewrite.trim());

        let mgr = ConnectionManager::new();
        let conn_id = "wait-bind-padded-rewrite";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get(conn_id).unwrap().connection_incarnation.clone()
        };
        {
            let state = mgr.get_state(conn_id).await.expect("state");
            let mut s = state.write().await;
            s.conversation_id = Some(42);
            s.external_id = Some("sess-pad".into());
            s.active_turn_generation = Some(1);
            s.turn_in_flight = true;
        }

        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp(conn_id, &incarnation, "sess-pad", 1);
        attr.start_turn(turn.clone(), t0).await;

        // Lease key uses the raw rewrite id bytes (as ACP announced them).
        let outcome = attr
            .register_or_touch_tool(
                &turn,
                padded_rewrite,
                classify_tool_category("other", Some("delegation")),
                t0,
            )
            .await
            .expect("pre-create padded rewrite lease");
        let wait_lease_id = outcome.lease_id.clone();

        // Wait stamp carries the same bytes resolve_wait_tool_id must return
        // for a blank request + padded rewrite fallback.
        let expected = WaitStamp {
            wait_id: "wait-padded-rewrite".into(),
            connection_id: conn_id.into(),
            connection_incarnation: incarnation.clone(),
            turn_generation: 1,
            parent_conversation_id: 42,
            parent_tool_use_id: Some(padded_rewrite.into()),
        };
        let lookup = ConnectionManagerParentLookup {
            manager: Arc::new(mgr.clone_ref()),
        };
        assert_eq!(
            lookup.bind_delegation_wait(conn_id, &expected).await,
            BindDelegationWaitResult::Bound,
            "bind must hit lease keyed by original padded rewrite bytes"
        );
        // Trimmed id must miss the lease (would be WaitToolLeaseMismatch).
        let trimmed_expected = WaitStamp {
            parent_tool_use_id: Some(padded_rewrite.trim().into()),
            ..expected.clone()
        };
        assert_eq!(
            lookup
                .bind_delegation_wait(conn_id, &trimmed_expected)
                .await,
            BindDelegationWaitResult::WaitToolLeaseMismatch,
            "trimmed rewrite id must not match padded lease key"
        );

        let lease = attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, padded_rewrite))
            .await
            .expect("lease still live under padded key");
        assert_eq!(
            attr.registry().lease_capability(&lease.lease_id).await,
            Some(CancellationCapability::DelegationWait {
                wait_id: "wait-padded-rewrite".into()
            })
        );

        // Renewal path: exact_match → record progress under the same raw id.
        let wait_cancel = WaitCancelRegistry::new();
        let (tx, _rx) = new_wait_cancel_channel();
        wait_cancel
            .register(WaitCancelHandle {
                stamp: expected.clone(),
                owner: WaitOwner::Listener,
                cancel: tx,
                task_ids: vec!["task-pad".into()],
            })
            .await
            .unwrap();
        let targets = wait_cancel
            .exact_match_progress_targets("task-pad", conn_id, &incarnation, 1)
            .await;
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].wait_tool_call_id.as_str(),
            padded_rewrite,
            "exact_match renew targets must preserve padded rewrite bytes"
        );

        let at = WatchdogInstant {
            mono: t0.mono + std::time::Duration::from_secs(590),
            wall: t0.wall + chrono::Duration::seconds(590),
        };
        let cleared = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &turn,
                "launch-missing",
                "task-pad",
                at,
            )
            .await;
        assert!(
            cleared.is_empty(),
            "renewal against padded rewrite key must succeed without demotion clear"
        );
        assert_eq!(
            attr.registry().lease_phase(&wait_lease_id).await,
            Some(ToolLeasePhase::Running),
            "padded rewrite wait lease must stay Running after renewal"
        );
        // Trimmed key must not resolve the live lease.
        assert!(
            attr.registry()
                .tool_stamp(&tool_lease_key(&turn, padded_rewrite.trim()))
                .await
                .is_none(),
            "trimmed rewrite id is a different lease key"
        );
    }

    #[tokio::test]
    async fn disconnect_if_owner_stamps_and_cas_skips_stale_after_rebind() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "leased-conn",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("leased-conn").unwrap();
            conn.owner_window_label = "conversation-1".into();
            conn.owner_operation_id = Some("opA".into());
            conn.ownership_generation = 1;
        }

        // Matching lease disconnect removes the connection.
        mgr.disconnect_if_owner(
            "leased-conn",
            Some("conversation-1"),
            Some("opA"),
            Some(1),
            AcpDisconnectOrigin::LegacyUnspecified,
        )
        .await
        .expect("matching lease disconnect");
        assert!(
            mgr.connections.lock().await.get("leased-conn").is_none(),
            "matching disconnect should remove"
        );

        // Re-insert and rebind to a newer incarnation.
        insert_fake_connection(
            &mgr,
            "leased-conn",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("leased-conn").unwrap();
            conn.owner_window_label = "conversation-1".into();
            conn.owner_operation_id = Some("opB".into());
            conn.ownership_generation = 2;
        }

        // Stale disconnect from incarnation A must not kill owner B.
        mgr.disconnect_if_owner(
            "leased-conn",
            Some("conversation-1"),
            Some("opA"),
            Some(1),
            AcpDisconnectOrigin::LegacyUnspecified,
        )
        .await
        .expect("stale is success no-op");
        {
            let map = mgr.connections.lock().await;
            let conn = map.get("leased-conn").expect("still present");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("opB"));
            assert_eq!(conn.ownership_generation, 2);
        }

        // Bare disconnect (no lease) remains unconditional.
        mgr.disconnect_if_owner(
            "leased-conn",
            None,
            None,
            None,
            AcpDisconnectOrigin::LegacyUnspecified,
        )
        .await
        .expect("legacy disconnect");
        assert!(mgr.connections.lock().await.get("leased-conn").is_none());
    }

    #[tokio::test]
    async fn disconnect_by_owner_window_and_operation_reaps_stamped_cold_conn() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "cold-op",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("cold-op").unwrap();
            conn.owner_window_label = "conversation-9".into();
            conn.owner_operation_id = Some("op-cold".into());
        }
        // Unrelated connection with no op stamp must not be reaped for this op.
        insert_fake_connection(
            &mgr,
            "main-no-op",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("main-no-op").unwrap();
            conn.owner_window_label = "conversation-9".into();
            conn.owner_operation_id = None;
        }

        let n = mgr
            .disconnect_by_owner_window_and_operation("conversation-9", "op-cold")
            .await;
        assert_eq!(n, 1);
        let map = mgr.connections.lock().await;
        assert!(map.get("cold-op").is_none());
        assert!(
            map.get("main-no-op").is_some(),
            "unstamped connection survives operation-scoped reap"
        );
    }

    // --- Pop-out close: idle residual + op-scoped reverse (Task 1) ---

    async fn stamp_owner(
        mgr: &ConnectionManager,
        id: &str,
        label: &str,
        op: &str,
        generation: u64,
        conversation_id: Option<i32>,
    ) {
        let mut map = mgr.connections.lock().await;
        let conn = map.get_mut(id).expect("connection present");
        conn.owner_window_label = label.into();
        conn.owner_operation_id = Some(op.into());
        conn.ownership_generation = generation;
        let mut st = conn.state.write().await;
        st.owner_window_label = label.into();
        if let Some(cid) = conversation_id {
            st.conversation_id = Some(cid);
        }
        st.status = ConnectionStatus::Connected;
    }

    #[test]
    fn is_idle_for_residual_true_only_when_connected_without_busy() {
        use crate::acp::session_state::PendingPermissionState;
        let now = chrono::Utc::now();
        let mut state = SessionState::new(
            "idle-pred".into(),
            AgentType::ClaudeCode,
            None,
            "conversation-1".into(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        assert!(
            is_idle_for_residual(&state, now),
            "Connected with no permission/background must be idle for residual"
        );

        state.status = ConnectionStatus::Prompting;
        assert!(!is_idle_for_residual(&state, now), "Prompting is busy");

        state.status = ConnectionStatus::Connected;
        state.pending_permission = Some(PendingPermissionState {
            request_id: "req".into(),
            tool_call_id: "tc".into(),
            tool_call: serde_json::json!({}),
            options: vec![],
            created_at: now,
            queued: 0,
        });
        assert!(
            !is_idle_for_residual(&state, now),
            "pending_permission is busy"
        );

        state.pending_permission = None;
        state.background_outstanding = 1;
        state.background_activity_at = Some(now);
        assert!(
            !is_idle_for_residual(&state, now),
            "active background work is busy"
        );

        // TOCTOU predicate: a busy transition after snapshot must revalidate false.
        state.background_outstanding = 0;
        state.background_activity_at = None;
        assert!(is_idle_for_residual(&state, now));
        state.status = ConnectionStatus::Prompting;
        assert!(
            !is_idle_for_residual(&state, now),
            "TOCTOU revalidate: idle→Prompting must fail residual predicate"
        );
    }

    #[tokio::test]
    async fn disconnect_idle_reaps_only_idle_matching_stamp() {
        use crate::acp::session_state::PendingPermissionState;
        let mgr = ConnectionManager::new();
        for id in ["idle-1", "busy-prompt", "busy-perm", "busy-bg"] {
            insert_fake_connection(&mgr, id, AgentType::ClaudeCode, None, EventEmitter::Noop).await;
            stamp_owner(&mgr, id, "conversation-1", "op-1", 1, Some(1)).await;
        }
        {
            let state = mgr.get_state("busy-prompt").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }
        {
            let state = mgr.get_state("busy-perm").await.unwrap();
            state.write().await.pending_permission = Some(PendingPermissionState {
                request_id: "req-1".into(),
                tool_call_id: "tc-1".into(),
                tool_call: serde_json::json!({ "toolCallId": "tc-1" }),
                options: vec![],
                created_at: chrono::Utc::now(),
                queued: 0,
            });
        }
        {
            let state = mgr.get_state("busy-bg").await.unwrap();
            let mut s = state.write().await;
            s.background_outstanding = 1;
            s.background_activity_at = Some(chrono::Utc::now());
        }

        let n = mgr
            .disconnect_idle_by_owner_window_and_operation("conversation-1", "op-1")
            .await;
        assert_eq!(n, 1, "only the idle Connected connection is reaped");
        let map = mgr.connections.lock().await;
        assert!(map.get("idle-1").is_none());
        assert!(map.get("busy-prompt").is_some());
        assert!(map.get("busy-perm").is_some());
        assert!(map.get("busy-bg").is_some());
    }

    #[tokio::test]
    async fn disconnect_idle_skips_wrong_op_and_wrong_label() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "op-a",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "op-a", "conversation-1", "op-A", 1, Some(1)).await;
        insert_fake_connection(
            &mgr,
            "op-b",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "op-b", "conversation-1", "op-B", 2, Some(1)).await;
        insert_fake_connection(
            &mgr,
            "other-label",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "other-label", "conversation-2", "op-A", 1, Some(2)).await;

        let n = mgr
            .disconnect_idle_by_owner_window_and_operation("conversation-1", "op-A")
            .await;
        assert_eq!(n, 1);
        let map = mgr.connections.lock().await;
        assert!(map.get("op-a").is_none());
        assert!(
            map.get("op-b").is_some(),
            "op B on same label must survive op A residual (ABA)"
        );
        assert!(
            map.get("other-label").is_some(),
            "wrong label must not be reaped"
        );
    }

    #[tokio::test]
    async fn disconnect_idle_op_a_does_not_reap_op_b_on_same_label() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(&mgr, "a", AgentType::ClaudeCode, None, EventEmitter::Noop).await;
        stamp_owner(&mgr, "a", "conversation-9", "op-A", 1, Some(9)).await;
        insert_fake_connection(&mgr, "b", AgentType::ClaudeCode, None, EventEmitter::Noop).await;
        stamp_owner(&mgr, "b", "conversation-9", "op-B", 2, Some(9)).await;

        let n = mgr
            .disconnect_idle_by_owner_window_and_operation("conversation-9", "op-A")
            .await;
        assert_eq!(n, 1);
        let map = mgr.connections.lock().await;
        assert!(map.get("a").is_none());
        assert!(map.get("b").is_some());
    }

    /// Phase-3 skip (busy / no write lock) must not leave a permanent tool-lease
    /// fence — otherwise the surviving busy connection's watchdog is broken.
    #[tokio::test]
    async fn disconnect_idle_skip_does_not_permanently_fence_survivor() {
        use crate::acp::tool_watchdog::{
            turn_stamp, LeaseAttribution, ToolCategory, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "busy-survivor",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "busy-survivor", "conversation-1", "op-1", 1, Some(1)).await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get("busy-survivor")
                .unwrap()
                .connection_incarnation
                .clone()
        };
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp("busy-survivor", &incarnation, "sess-1", 1);
        attr.start_turn(turn.clone(), t0).await;
        let _ = attr
            .register_or_touch_tool(&turn, "tool-live", ToolCategory::Other, t0)
            .await
            .expect("register while alive");

        // Busy → exclusive residual skips remove; must not fence admission.
        {
            let state = mgr.get_state("busy-survivor").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }

        let n = mgr
            .disconnect_idle_by_owner_window_and_operation("conversation-1", "op-1")
            .await;
        assert_eq!(n, 0, "busy must not be reaped");
        assert!(
            !attr
                .registry()
                .is_fenced("busy-survivor", &incarnation)
                .await,
            "skipped residual must not permanently fence a surviving connection"
        );
        assert!(
            attr.register_or_touch_tool(
                &turn,
                "tool-after-skip",
                ToolCategory::Other,
                t0.advanced(1),
            )
            .await
            .is_some(),
            "watchdog tool admission must remain open after residual skip"
        );
        {
            let map = mgr.connections.lock().await;
            assert!(map.get("busy-survivor").is_some());
        }
    }

    /// Holding the session write lock forces phase-3 skip (try_write fails).
    /// Residual must not fence the connection that remains in the map.
    #[tokio::test]
    async fn disconnect_idle_write_lock_skip_does_not_fence() {
        use crate::acp::tool_watchdog::{
            turn_stamp, LeaseAttribution, ToolCategory, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "lock-held",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "lock-held", "conversation-2", "op-2", 1, Some(2)).await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get("lock-held").unwrap().connection_incarnation.clone()
        };
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp("lock-held", &incarnation, "sess-2", 1);
        attr.start_turn(turn.clone(), t0).await;

        // Hold exclusive write for the whole residual pass so try_read/try_write skip.
        let state = mgr.get_state("lock-held").await.unwrap();
        let _write_guard = state.write().await;

        let n = mgr
            .disconnect_idle_by_owner_window_and_operation("conversation-2", "op-2")
            .await;
        assert_eq!(n, 0);
        drop(_write_guard);

        assert!(
            !attr.registry().is_fenced("lock-held", &incarnation).await,
            "write-lock skip must not leave a permanent fence"
        );
        assert!(
            attr.register_or_touch_tool(
                &turn,
                "tool-after-lock",
                ToolCategory::Other,
                t0.advanced(1),
            )
            .await
            .is_some(),
            "admission must work after residual skipped due to write lock"
        );
        {
            let map = mgr.connections.lock().await;
            assert!(map.get("lock-held").is_some());
        }
    }

    /// Successful idle residual still fences and clears leases for removed conns.
    #[tokio::test]
    async fn disconnect_idle_success_fences_removed_connection() {
        use crate::acp::tool_watchdog::{
            turn_stamp, LeaseAttribution, ToolCategory, WatchdogInstant,
        };
        use chrono::{DateTime, Utc};
        use tokio::time::Instant;

        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "idle-reaped",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "idle-reaped", "conversation-4", "op-4", 1, Some(4)).await;

        let incarnation = {
            let map = mgr.connections.lock().await;
            map.get("idle-reaped")
                .unwrap()
                .connection_incarnation
                .clone()
        };
        let attr = LeaseAttribution::new(mgr.tool_lease_registry());
        let t0 = WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let turn = turn_stamp("idle-reaped", &incarnation, "sess-4", 1);
        attr.start_turn(turn.clone(), t0).await;
        let _ = attr
            .register_or_touch_tool(&turn, "tool-pre", ToolCategory::Other, t0)
            .await
            .expect("register");

        let n = mgr
            .disconnect_idle_by_owner_window_and_operation("conversation-4", "op-4")
            .await;
        assert_eq!(n, 1);
        assert!(
            attr.registry().is_fenced("idle-reaped", &incarnation).await,
            "successful residual must fence the reaped incarnation"
        );
        assert!(
            attr.register_or_touch_tool(&turn, "tool-post", ToolCategory::Other, t0.advanced(1),)
                .await
                .is_none(),
            "post-reap register must no-op"
        );
    }

    #[tokio::test]
    async fn rebind_rejects_when_root_operation_id_mismatches() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "live-opb",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "live-opb", "conversation-9", "op-B", 3, Some(9)).await;

        let err = mgr
            .rebind_connection_owner_window(
                9,
                Some("live-opb"),
                "conversation-9",
                "main",
                "op-A", // delayed close for older incarnation
                Some(3),
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("owner operation CAS"),
            "error must include owner operation CAS for Superseded mapping, got: {msg}"
        );
        {
            let map = mgr.connections.lock().await;
            let conn = map
                .get("live-opb")
                .expect("must not move wrong incarnation");
            assert_eq!(conn.owner_window_label, "conversation-9");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-B"));
            assert_eq!(conn.ownership_generation, 3);
        }
    }

    /// Forward rebind (main → conversation-*) must stamp a new operation_id
    /// even when the live stamp is a different op — op CAS is reverse-only.
    #[tokio::test]
    async fn rebind_forward_main_op_a_to_conversation_op_b_succeeds() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "main-opa",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // After reverse or prior pop-out residual: live on main with op-A stamp.
        stamp_owner(&mgr, "main-opa", "main", "op-A", 2, Some(7)).await;

        let result = mgr
            .rebind_connection_owner_window(
                7,
                Some("main-opa"),
                "main",
                "conversation-7",
                "op-B", // new forward pop-out operation
                Some(2),
            )
            .await
            .expect("forward rebind must not apply reverse-only op CAS");
        assert_eq!(result.rebound_count, 1);
        assert_eq!(result.ownership_generation, 3);
        assert_eq!(result.operation_id, "op-B");
        {
            let map = mgr.connections.lock().await;
            let conn = map.get("main-opa").expect("connection present");
            assert_eq!(conn.owner_window_label, "conversation-7");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-B"));
            assert_eq!(conn.ownership_generation, 3);
            let st = conn.state.try_read().expect("state");
            assert_eq!(st.owner_window_label, "conversation-7");
        }
    }

    #[tokio::test]
    async fn rebind_stamped_moves_busy_child_when_root_already_main() {
        let mgr = ConnectionManager::new();
        // Root already reverse-rebound to main (same op stamp retained).
        insert_fake_connection(
            &mgr,
            "root-main",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "root-main", "main", "op-1", 5, Some(1)).await;
        // Late busy child still on conversation label + same op.
        insert_fake_connection(
            &mgr,
            "late-child",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "late-child", "conversation-1", "op-1", 4, Some(99)).await;
        {
            let state = mgr.get_state("late-child").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }

        let (n, max_gen) = mgr
            .rebind_stamped_connections_owner_window("conversation-1", "op-1", "main")
            .await;
        assert_eq!(n, 1, "busy late child must still rebind by stamp");
        assert_eq!(max_gen, Some(5), "must report post-rebind generation");
        {
            let map = mgr.connections.lock().await;
            let child = map.get("late-child").expect("child present");
            assert_eq!(child.owner_window_label, "main");
            assert_eq!(child.owner_operation_id.as_deref(), Some("op-1"));
            assert_eq!(
                child.ownership_generation, 5,
                "generation must advance on stamped residual rebind"
            );
            let root = map.get("root-main").unwrap();
            assert_eq!(
                root.owner_window_label, "main",
                "already-main root must not be matched by from_label"
            );
            assert_eq!(root.ownership_generation, 5);
            let st = child.state.try_read().expect("state");
            assert_eq!(st.owner_window_label, "main");
            assert_eq!(st.status, ConnectionStatus::Prompting);
        }
    }

    #[test]
    fn cold_connect_reuse_allowed_only_for_same_incarnation() {
        assert!(cold_connect_reuse_allowed(
            "conversation-1",
            Some("opA"),
            "conversation-1",
            "opA"
        ));
        // Main-owned / unstamped must not be faked as cold.
        assert!(!cold_connect_reuse_allowed(
            "main",
            None,
            "conversation-1",
            "opA"
        ));
        assert!(!cold_connect_reuse_allowed(
            "conversation-1",
            None,
            "conversation-1",
            "opA"
        ));
        // Different op or label refuses.
        assert!(!cold_connect_reuse_allowed(
            "conversation-1",
            Some("opB"),
            "conversation-1",
            "opA"
        ));
        assert!(!cold_connect_reuse_allowed(
            "conversation-2",
            Some("opA"),
            "conversation-1",
            "opA"
        ));
    }

    /// Manager-level: `spawn_agent` with `owner_operation_id` must refuse to
    /// reuse a main-owned session (no cold stamp), and must reuse only when
    /// the existing connection already carries the same incarnation lease.
    #[tokio::test]
    async fn spawn_agent_cold_dedup_rejects_main_owned_and_reuses_same_incarnation() {
        use crate::acp::terminal_context::AcpLaunchInputs;
        use crate::models::SystemTerminalSettings;

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let working_dir = PathBuf::from("/tmp/cold-dedup-spawn");
        let session_ext = "ext-cold-dedup";
        let main_id = "main-owned-sess";
        let cold_id = "same-incarnation-sess";

        // --- main-owned existing session: cold connect must refuse reuse ---
        insert_fake_connection(
            &mgr,
            main_id,
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut(main_id).unwrap();
            conn.owner_window_label = "main".into();
            conn.owner_operation_id = None;
            let mut s = conn.state.write().await;
            s.external_id = Some(session_ext.into());
            s.status = ConnectionStatus::Connected;
        }

        let inputs = AcpLaunchInputs::with_placeholder_route(
            BTreeMap::new(),
            SystemTerminalSettings {
                default_shell: Some("missing-shell".into()),
            },
        );
        let err = mgr
            .spawn_agent(
                AgentType::ClaudeCode,
                Some(working_dir.to_string_lossy().into_owned()),
                Some(session_ext.into()),
                inputs,
                "conversation-cold".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                Some("op-cold".into()),
                None,
            )
            .await
            .expect_err("main-owned session must not be cold-reused");
        let msg = err.to_string();
        assert!(
            msg.contains("not owned by this") || msg.contains("refuse cold"),
            "expected cold-dedup refusal, got: {msg}"
        );
        assert!(
            mgr.connections.lock().await.get(main_id).is_some(),
            "main-owned connection must remain after refused cold reuse"
        );

        // --- same-incarnation: reuse preserves the existing lease stamp ---
        insert_fake_connection(
            &mgr,
            cold_id,
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            // Remove main entry so find_connection_for_reuse hits cold_id only
            // for a second session key, or use a distinct session id.
            map.remove(main_id);
            let conn = map.get_mut(cold_id).unwrap();
            conn.owner_window_label = "conversation-cold".into();
            conn.owner_operation_id = Some("op-cold".into());
            conn.ownership_generation = 3;
            let mut s = conn.state.write().await;
            s.external_id = Some("ext-same-op".into());
            s.status = ConnectionStatus::Connected;
        }

        let inputs2 = AcpLaunchInputs::with_placeholder_route(
            BTreeMap::new(),
            SystemTerminalSettings {
                default_shell: Some("missing-shell".into()),
            },
        );
        let reused = mgr
            .spawn_agent(
                AgentType::ClaudeCode,
                Some(working_dir.to_string_lossy().into_owned()),
                Some("ext-same-op".into()),
                inputs2,
                "conversation-cold".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                Some("op-cold".into()),
                None,
            )
            .await
            .expect("same-incarnation cold reuse must succeed");
        assert_eq!(reused, cold_id);
        {
            let map = mgr.connections.lock().await;
            let conn = map.get(cold_id).expect("lease preserved");
            assert_eq!(conn.owner_window_label, "conversation-cold");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-cold"));
            assert_eq!(
                conn.ownership_generation, 3,
                "reuse must not rewrite the existing ownership generation"
            );
        }
    }

    #[tokio::test]
    async fn disconnect_if_owner_cas_skips_reused_main_connection() {
        // Abort-path regression: bare disconnect would kill main's session;
        // disconnect_if_owner with the cold op is a no-op on main-owned.
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "main-reuse",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("main-reuse").unwrap();
            conn.owner_window_label = "main".into();
            conn.owner_operation_id = None;
        }
        mgr.disconnect_if_owner(
            "main-reuse",
            Some("conversation-1"),
            Some("op-cold"),
            None,
            AcpDisconnectOrigin::LegacyUnspecified,
        )
        .await
        .expect("stale lease is success no-op");
        assert!(
            mgr.connections.lock().await.get("main-reuse").is_some(),
            "main-owned connection must survive cold abort CAS"
        );
    }

    /// Child registered *after* parent rebind must adopt the post-rebind
    /// `(label, generation, operation_id)` under the connections lock.
    /// Unrelated other roots under `main` remain untouched.
    #[tokio::test]
    async fn child_registration_after_rebind_adopts_parent_ownership() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "parent-root",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        insert_fake_connection(
            &mgr,
            "unrelated-main",
            AgentType::Codex,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            for id in ["parent-root", "unrelated-main"] {
                let conn = map.get_mut(id).unwrap();
                conn.owner_window_label = "main".into();
                conn.owner_operation_id = None;
                conn.ownership_generation = 0;
            }
            {
                let mut st = map.get_mut("parent-root").unwrap().state.write().await;
                st.conversation_id = Some(42);
                st.owner_window_label = "main".into();
            }
            {
                let mut st2 = map.get_mut("unrelated-main").unwrap().state.write().await;
                st2.conversation_id = Some(99);
                st2.owner_window_label = "main".into();
            }
        }

        let rebind = mgr
            .rebind_connection_owner_window(
                42,
                Some("parent-root"),
                "main",
                "conversation-42",
                "op-pop",
                None,
            )
            .await
            .expect("rebind parent");
        assert_eq!(rebind.ownership_generation, 1);

        mgr.insert_test_child_adopting_parent_ownership(
            "child-after",
            "parent-root",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;

        {
            let map = mgr.connections.lock().await;
            let child = map.get("child-after").expect("child present");
            assert_eq!(child.owner_window_label, "conversation-42");
            assert_eq!(child.owner_operation_id.as_deref(), Some("op-pop"));
            assert_eq!(child.ownership_generation, 1);
            assert_eq!(child.parent_connection_id.as_deref(), Some("parent-root"));
            let unrelated = map.get("unrelated-main").expect("unrelated present");
            assert_eq!(unrelated.owner_window_label, "main");
            assert_eq!(unrelated.owner_operation_id, None);
            assert_eq!(unrelated.ownership_generation, 0);
        }
    }

    /// Child registered *before* rebind (not yet in active_delegations) must
    /// still be rebound via `parent_connection_id` edge. Unrelated main root
    /// stays put.
    #[tokio::test]
    async fn child_registered_before_rebind_updates_via_parent_link() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "parent-root",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        insert_fake_connection(
            &mgr,
            "unrelated-main",
            AgentType::Codex,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            for id in ["parent-root", "unrelated-main"] {
                let conn = map.get_mut(id).unwrap();
                conn.owner_window_label = "main".into();
                conn.owner_operation_id = None;
                conn.ownership_generation = 0;
            }
            {
                let mut st = map.get_mut("parent-root").unwrap().state.write().await;
                st.conversation_id = Some(7);
                st.owner_window_label = "main".into();
            }
            {
                let mut st2 = map.get_mut("unrelated-main").unwrap().state.write().await;
                st2.conversation_id = Some(8);
                st2.owner_window_label = "main".into();
            }
        }

        // Child becomes visible while parent is still main-owned — no
        // active_delegations entry yet (broker link races rebind).
        mgr.insert_test_child_adopting_parent_ownership(
            "child-before",
            "parent-root",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let map = mgr.connections.lock().await;
            let child = map.get("child-before").unwrap();
            assert_eq!(child.owner_window_label, "main");
            assert_eq!(child.ownership_generation, 0);
        }

        let rebind = mgr
            .rebind_connection_owner_window(
                7,
                Some("parent-root"),
                "main",
                "conversation-7",
                "op-before",
                None,
            )
            .await
            .expect("rebind");
        assert_eq!(rebind.ownership_generation, 1);
        assert!(
            rebind.rebound_count >= 2,
            "root + unlinked child must both rebind, got {}",
            rebind.rebound_count
        );

        {
            let map = mgr.connections.lock().await;
            let child = map.get("child-before").unwrap();
            assert_eq!(child.owner_window_label, "conversation-7");
            assert_eq!(child.owner_operation_id.as_deref(), Some("op-before"));
            assert_eq!(child.ownership_generation, 1);
            let unrelated = map.get("unrelated-main").unwrap();
            assert_eq!(unrelated.owner_window_label, "main");
            assert_eq!(unrelated.ownership_generation, 0);
        }
    }

    /// Deterministic interleaving: delayed child registration after rebind
    /// adopts current parent ownership (barrier via oneshot).
    #[tokio::test]
    async fn concurrent_child_spawn_rebind_barrier_adopts_parent() {
        let mgr = Arc::new(ConnectionManager::new());
        insert_fake_connection(
            &mgr,
            "parent-root",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        insert_fake_connection(
            &mgr,
            "unrelated-main",
            AgentType::Codex,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            for id in ["parent-root", "unrelated-main"] {
                let conn = map.get_mut(id).unwrap();
                conn.owner_window_label = "main".into();
                conn.owner_operation_id = None;
                conn.ownership_generation = 0;
            }
            {
                let mut st = map.get_mut("parent-root").unwrap().state.write().await;
                st.conversation_id = Some(55);
                st.owner_window_label = "main".into();
            }
            {
                let mut st2 = map.get_mut("unrelated-main").unwrap().state.write().await;
                st2.conversation_id = Some(56);
                st2.owner_window_label = "main".into();
            }
        }

        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let mgr_child = Arc::clone(&mgr);
        let child_task = tokio::spawn(async move {
            // Simulate in-flight spawn: snapshot would be stale; wait for rebind.
            let _ = gate_rx.await;
            mgr_child
                .insert_test_child_adopting_parent_ownership(
                    "child-race",
                    "parent-root",
                    AgentType::ClaudeCode,
                    None,
                    EventEmitter::Noop,
                )
                .await;
        });

        // Parent rebind completes while child is still "in flight".
        let rebind = mgr
            .rebind_connection_owner_window(
                55,
                Some("parent-root"),
                "main",
                "conversation-55",
                "op-race",
                None,
            )
            .await
            .expect("rebind during child spawn");
        assert_eq!(rebind.ownership_generation, 1);

        // Release child registration; must adopt post-rebind ownership.
        gate_tx.send(()).expect("release child gate");
        child_task.await.expect("child task");

        {
            let map = mgr.connections.lock().await;
            let child = map.get("child-race").expect("child registered");
            assert_eq!(child.owner_window_label, "conversation-55");
            assert_eq!(child.owner_operation_id.as_deref(), Some("op-race"));
            assert_eq!(child.ownership_generation, 1);
            let unrelated = map.get("unrelated-main").unwrap();
            assert_eq!(unrelated.owner_window_label, "main");
            assert_eq!(unrelated.ownership_generation, 0);
        }
    }

    /// Pre-ready claim failure: detached reverse advances gen, then abort reverse
    /// with a stale expected gen must still succeed as idempotent AlreadyMain
    /// (same op on main) and return the live post-reverse generation.
    #[tokio::test]
    async fn rebind_already_at_target_same_op_is_idempotent_before_gen_cas() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "live-1",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("live-1").unwrap();
            conn.owner_window_label = "main".into();
            conn.owner_operation_id = Some("op-B".into());
            conn.ownership_generation = 4;
            let mut st = conn.state.write().await;
            st.conversation_id = Some(7);
            st.owner_window_label = "main".into();
        }

        // Abort reverse with stale expected gen (forward was 3; reverse already
        // advanced to 4 on main with same op).
        let rev = mgr
            .rebind_connection_owner_window(
                7,
                Some("live-1"),
                "conversation-7",
                "main",
                "op-B",
                Some(3),
            )
            .await
            .expect("idempotent reverse while already on main");
        assert_eq!(rev.ownership_generation, 4);
        assert_eq!(rev.rebound_count, 0);
        {
            let map = mgr.connections.lock().await;
            let conn = map.get("live-1").unwrap();
            assert_eq!(conn.owner_window_label, "main");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-B"));
            assert_eq!(conn.ownership_generation, 4);
        }
    }

    fn install_rebind_after_snapshot_hook(
        manager: &ConnectionManager,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *manager
            .rebind_after_snapshot_hook
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(RebindAfterSnapshotHook {
            reached: reached.clone(),
            resume: resume.clone(),
        });
        (reached, resume)
    }

    /// Snapshot includes a child; replacing that ID with a new incarnation
    /// before the final map lock must fail closed and leave the replacement
    /// owner untouched.
    #[tokio::test]
    async fn rebind_fails_when_child_id_replaced_with_new_incarnation_after_snapshot() {
        let mgr = Arc::new(ConnectionManager::new());
        insert_fake_connection(
            &mgr,
            "root-cas",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "root-cas", "conversation-1", "op-1", 1, Some(1)).await;
        mgr.insert_test_child_adopting_parent_ownership(
            "child-reuse",
            "root-cas",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "child-reuse", "conversation-1", "op-1", 1, Some(2)).await;

        let (reached, resume) = install_rebind_after_snapshot_hook(&mgr);
        let rebind = {
            let mgr = Arc::clone(&mgr);
            tokio::spawn(async move {
                mgr.rebind_connection_owner_window(
                    1,
                    Some("root-cas"),
                    "conversation-1",
                    "main",
                    "op-1",
                    Some(1),
                )
                .await
            })
        };
        reached.notified().await;
        insert_fake_connection(
            &mgr,
            "child-reuse",
            AgentType::Codex,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(
            &mgr,
            "child-reuse",
            "conversation-99",
            "op-new",
            7,
            Some(99),
        )
        .await;
        let replacement_incarnation = {
            let map = mgr.connections.lock().await;
            map.get("child-reuse")
                .expect("replacement")
                .connection_incarnation
                .clone()
        };
        resume.notify_one();
        let err = tokio::time::timeout(Duration::from_secs(5), rebind)
            .await
            .expect("rebind must finish")
            .expect("rebind join")
            .expect_err("ID reuse must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("incarnation CAS"),
            "coded incarnation CAS error required, got: {msg}"
        );
        {
            let map = mgr.connections.lock().await;
            let root = map.get("root-cas").expect("root present");
            assert_eq!(root.owner_window_label, "conversation-1");
            assert_eq!(root.owner_operation_id.as_deref(), Some("op-1"));
            assert_eq!(root.ownership_generation, 1);
            let replacement = map.get("child-reuse").expect("replacement present");
            assert_eq!(replacement.owner_window_label, "conversation-99");
            assert_eq!(replacement.owner_operation_id.as_deref(), Some("op-new"));
            assert_eq!(replacement.ownership_generation, 7);
            assert_eq!(replacement.connection_incarnation, replacement_incarnation);
        }
    }

    /// Snapshot includes root, a mutating child, and a sibling. Changing only
    /// the child's owner/generation after snapshot (same incarnation) must
    /// fail the whole rebind; root and sibling stay unmodified.
    #[tokio::test]
    async fn rebind_fails_when_child_owner_generation_changes_after_snapshot() {
        let mgr = Arc::new(ConnectionManager::new());
        insert_fake_connection(
            &mgr,
            "root-cas",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "root-cas", "conversation-1", "op-1", 1, Some(1)).await;
        for child_id in ["child-mut", "child-sib"] {
            mgr.insert_test_child_adopting_parent_ownership(
                child_id,
                "root-cas",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
            stamp_owner(&mgr, child_id, "conversation-1", "op-1", 1, Some(2)).await;
        }
        let child_incarnation = {
            let map = mgr.connections.lock().await;
            map.get("child-mut")
                .expect("child")
                .connection_incarnation
                .clone()
        };

        let (reached, resume) = install_rebind_after_snapshot_hook(&mgr);
        let rebind = {
            let mgr = Arc::clone(&mgr);
            tokio::spawn(async move {
                mgr.rebind_connection_owner_window(
                    1,
                    Some("root-cas"),
                    "conversation-1",
                    "main",
                    "op-1",
                    Some(1),
                )
                .await
            })
        };
        reached.notified().await;
        {
            let mut map = mgr.connections.lock().await;
            let child = map.get_mut("child-mut").expect("child present");
            child.owner_window_label = "conversation-99".into();
            child.ownership_generation = 99;
            assert_eq!(child.connection_incarnation, child_incarnation);
        }
        resume.notify_one();
        let err = tokio::time::timeout(Duration::from_secs(5), rebind)
            .await
            .expect("rebind must finish")
            .expect("rebind join")
            .expect_err("child owner/generation drift must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("owner label CAS") || msg.contains("generation CAS"),
            "coded owner/generation CAS error required, got: {msg}"
        );
        {
            let map = mgr.connections.lock().await;
            let root = map.get("root-cas").expect("root present");
            assert_eq!(root.owner_window_label, "conversation-1");
            assert_eq!(root.owner_operation_id.as_deref(), Some("op-1"));
            assert_eq!(root.ownership_generation, 1);
            let sibling = map.get("child-sib").expect("sibling present");
            assert_eq!(sibling.owner_window_label, "conversation-1");
            assert_eq!(sibling.owner_operation_id.as_deref(), Some("op-1"));
            assert_eq!(sibling.ownership_generation, 1);
            let child = map.get("child-mut").expect("mutated child present");
            assert_eq!(child.owner_window_label, "conversation-99");
            assert_eq!(child.ownership_generation, 99);
            assert_eq!(child.connection_incarnation, child_incarnation);
        }
    }

    /// Snapshot shows the root already on the target owner+op; replacing the
    /// root before returning success must re-check the live entry and fail.
    #[tokio::test]
    async fn rebind_idempotent_early_return_rechecks_live_root_after_snapshot() {
        let mgr = Arc::new(ConnectionManager::new());
        insert_fake_connection(
            &mgr,
            "live-1",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        stamp_owner(&mgr, "live-1", "main", "op-B", 4, Some(7)).await;

        let (reached, resume) = install_rebind_after_snapshot_hook(&mgr);
        let rebind = {
            let mgr = Arc::clone(&mgr);
            tokio::spawn(async move {
                mgr.rebind_connection_owner_window(
                    7,
                    Some("live-1"),
                    "conversation-7",
                    "main",
                    "op-B",
                    Some(3),
                )
                .await
            })
        };
        reached.notified().await;
        insert_fake_connection(&mgr, "live-1", AgentType::Codex, None, EventEmitter::Noop).await;
        stamp_owner(&mgr, "live-1", "conversation-7", "op-other", 1, Some(7)).await;
        resume.notify_one();
        let err = tokio::time::timeout(Duration::from_secs(5), rebind)
            .await
            .expect("rebind must finish")
            .expect("rebind join")
            .expect_err("stale idempotent snapshot must not succeed");
        let msg = err.to_string();
        assert!(
            msg.contains("CAS"),
            "coded CAS error required after live re-check, got: {msg}"
        );
        {
            let map = mgr.connections.lock().await;
            let replacement = map.get("live-1").expect("replacement present");
            assert_eq!(replacement.owner_window_label, "conversation-7");
            assert_eq!(replacement.owner_operation_id.as_deref(), Some("op-other"));
            assert_eq!(replacement.ownership_generation, 1);
        }
    }

    #[tokio::test]
    async fn refresh_connection_staleness_flags_only_drifted_running_sessions() {
        let mgr = ConnectionManager::new();
        // Test connections spawn with an empty agent fingerprint (insert_test_connection).
        insert_fake_connection(&mgr, "c1", AgentType::Codex, None, EventEmitter::Noop).await;
        // A different agent type that must stay untouched.
        insert_fake_connection(&mgr, "c2", AgentType::ClaudeCode, None, EventEmitter::Noop).await;

        // A real config change for Codex (fresh fp differs from the "" spawn fp).
        let mut fresh = HashMap::new();
        fresh.insert(AgentType::Codex, "codex-v2".to_string());
        let n = mgr
            .refresh_connection_staleness(&fresh, ConfigStaleKind::AgentConfig)
            .await;
        assert_eq!(n, 1, "only the Codex session is stale");
        assert!(
            mgr.get_state("c1").await.unwrap().read().await.config_stale,
            "Codex session flagged stale"
        );
        assert!(
            !mgr.get_state("c2").await.unwrap().read().await.config_stale,
            "ClaudeCode session untouched (agent not in the fresh set)"
        );

        // Re-running with the SAME fingerprint keeps it stale but is idempotent.
        let n2 = mgr
            .refresh_connection_staleness(&fresh, ConfigStaleKind::AgentConfig)
            .await;
        assert_eq!(n2, 1);

        // Reverting Codex back to its spawn fingerprint ("") clears staleness.
        let mut reverted = HashMap::new();
        reverted.insert(AgentType::Codex, String::new());
        let n3 = mgr
            .refresh_connection_staleness(&reverted, ConfigStaleKind::AgentConfig)
            .await;
        assert_eq!(n3, 0, "reverted config is no longer stale");
        assert!(
            !mgr.get_state("c1").await.unwrap().read().await.config_stale,
            "staleness cleared after revert"
        );
    }

    /// Seed a single Codex connection whose spawn and observed components both
    /// start at the given agent / shell fingerprints.
    async fn manager_with_fingerprints(agent_fp: &str, shell_fp: &str) -> ConnectionManager {
        let mgr = ConnectionManager::new();
        insert_fake_connection(&mgr, "c1", AgentType::Codex, None, EventEmitter::Noop).await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut("c1").unwrap();
            let (spawn_config, observed_config) =
                matching_config_pair(agent_fp.to_string(), shell_fp.to_string(), String::new());
            conn.spawn_config = spawn_config;
            conn.observed_config = observed_config;
        }
        mgr
    }

    #[tokio::test]
    async fn shell_change_marks_all_running_connections_stale() {
        let mgr = manager_with_fingerprints("agent-v1", "shell-v1").await;
        let count = mgr.refresh_terminal_shell_staleness("shell-v2").await;
        assert_eq!(count, 1);
        let state = mgr.get_state("c1").await.unwrap();
        let state = state.read().await;
        assert!(state.config_stale);
        assert_eq!(
            state.config_stale_kind,
            Some(ConfigStaleKind::TerminalShell)
        );
    }

    #[tokio::test]
    async fn reverting_shell_keeps_agent_config_drift_visible() {
        let mgr = manager_with_fingerprints("agent-v1", "shell-v1").await;
        let mut fresh = HashMap::new();
        fresh.insert(AgentType::Codex, "agent-v2".to_string());
        mgr.refresh_connection_staleness(&fresh, ConfigStaleKind::AgentConfig)
            .await;
        mgr.refresh_terminal_shell_staleness("shell-v2").await;
        mgr.refresh_terminal_shell_staleness("shell-v1").await;

        let state = mgr.get_state("c1").await.unwrap();
        let state = state.read().await;
        assert!(state.config_stale);
        assert_eq!(state.config_stale_kind, Some(ConfigStaleKind::AgentConfig));
    }

    #[tokio::test]
    async fn no_op_shell_save_emits_no_new_stale_event() {
        let mgr = manager_with_fingerprints("agent-v1", "shell-v1").await;
        let mut receiver = subscribe_conn_stream(&mgr, "c1").await;
        assert_eq!(mgr.refresh_terminal_shell_staleness("shell-v1").await, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), receiver.recv())
                .await
                .is_err()
        );
    }

    fn synthetic_connection_with_fingerprints(
        agent: &str,
        shell: &str,
        route: &str,
    ) -> AgentConnection {
        let (tx, _rx, _liveness_rx) = connection_channel(1);
        let mut state = SessionState::new(
            "synth".into(),
            AgentType::Codex,
            None,
            "test-window".into(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        let (spawn_config, observed_config) =
            matching_config_pair(agent.to_string(), shell.to_string(), route.to_string());
        AgentConnection {
            id: "synth".into(),
            agent_type: AgentType::Codex,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".into(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation: state.connection_incarnation.clone(),
            tool_lease_registry: state.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config,
            observed_config,
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    #[test]
    fn reuse_requires_route_compatibility_and_stale_priority_is_stable() {
        assert_eq!(
            route_reuse_decision("route-a", "route-a", "conn-1"),
            RouteReuseDecision::Reuse
        );
        assert_eq!(
            route_reuse_decision("route-a", "route-b", "conn-1"),
            RouteReuseDecision::Conflict {
                existing_connection_id: "conn-1".into(),
            }
        );

        let mut conn = synthetic_connection_with_fingerprints("agent-v1", "shell-v1", "route-v1");
        conn.observed_config.fingerprint.agent_config = "agent-v2".into();
        assert_eq!(
            effective_stale_kind(&conn),
            Some(ConfigStaleKind::AgentConfig)
        );
        conn.observed_config.fingerprint.delegation_route = "route-v2".into();
        assert_eq!(
            effective_stale_kind(&conn),
            Some(ConfigStaleKind::DelegationRoute)
        );
        conn.observed_config.fingerprint.terminal_shell = "shell-v2".into();
        assert_eq!(
            effective_stale_kind(&conn),
            Some(ConfigStaleKind::TerminalShell)
        );
    }

    async fn manager_stale_kind(mgr: &ConnectionManager, id: &str) -> Option<ConfigStaleKind> {
        let state = mgr.get_state(id).await.unwrap();
        let kind = state.read().await.config_stale_kind;
        kind
    }

    async fn seed_route_root(
        mgr: &ConnectionManager,
        id: &str,
        preference: Option<crate::acp::delegation::route::DelegationRoutePolicy>,
        fingerprint: &str,
    ) {
        use crate::acp::delegation::route::{
            DelegationConnectionOrigin, DelegationRoutePolicy, DelegationRouteSource,
            NativeSuppressionPlan, ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        insert_fake_connection(mgr, id, AgentType::Codex, None, EventEmitter::Noop).await;
        let mut map = mgr.connections.lock().await;
        let conn = map.get_mut(id).unwrap();
        let (spawn_config, observed_config) =
            matching_config_pair("agent-v1", "shell-v1", fingerprint.to_string());
        conn.spawn_config = spawn_config;
        conn.observed_config = observed_config;
        conn.origin = DelegationConnectionOrigin::Root;
        conn.route_preference = preference;
        conn.route_plan = crate::acp::delegation::route::DelegationRoutePlan {
            managed: true,
            requested: preference.unwrap_or(DelegationRoutePolicy::Codeg),
            effective: preference.unwrap_or(DelegationRoutePolicy::Codeg),
            source: if preference.is_some() {
                DelegationRouteSource::SessionOverride
            } else {
                DelegationRouteSource::GlobalDefault
            },
            native_suppression: if preference == Some(DelegationRoutePolicy::Native) {
                NativeSuppressionPlan::None
            } else {
                NativeSuppressionPlan::CodexMultiAgentFalse
            },
            expose_codeg_delegation: preference != Some(DelegationRoutePolicy::Native),
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: fingerprint.to_string(),
        };
    }

    #[tokio::test]
    async fn route_setting_revert_clears_root_staleness_and_never_marks_child() {
        use crate::acp::delegation::route::{
            comparison_route_fingerprint, DelegationConnectionOrigin, DelegationRoutePolicy,
        };

        let codeg_fp = comparison_route_fingerprint(
            AgentType::Codex,
            DelegationConnectionOrigin::Root,
            None,
            DelegationRoutePolicy::Codeg,
            true,
            &crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
        );
        let mgr = ConnectionManager::new();
        seed_route_root(&mgr, "root", None, &codeg_fp).await;
        // Forced child with matching Codeg fingerprint.
        insert_fake_connection(&mgr, "child", AgentType::Codex, None, EventEmitter::Noop).await;
        {
            let mut map = mgr.connections.lock().await;
            let child = map.get_mut("child").unwrap();
            let (spawn_config, observed_config) =
                matching_config_pair("agent-v1", "shell-v1", codeg_fp.clone());
            child.spawn_config = spawn_config;
            child.observed_config = observed_config;
            child.origin = DelegationConnectionOrigin::CodegChild;
            child.route_preference = None;
        }

        mgr.refresh_delegation_route_staleness(DelegationRoutePolicy::Native, true)
            .await;
        assert_eq!(
            manager_stale_kind(&mgr, "root").await,
            Some(ConfigStaleKind::DelegationRoute)
        );
        assert_eq!(manager_stale_kind(&mgr, "child").await, None);

        mgr.refresh_delegation_route_staleness(DelegationRoutePolicy::Codeg, true)
            .await;
        assert_eq!(manager_stale_kind(&mgr, "root").await, None);
    }

    #[tokio::test]
    async fn global_route_refresh_respects_each_root_override() {
        use crate::acp::delegation::route::{
            comparison_route_fingerprint, DelegationConnectionOrigin, DelegationRoutePolicy,
        };

        let codeg_fp = comparison_route_fingerprint(
            AgentType::Codex,
            DelegationConnectionOrigin::Root,
            None,
            DelegationRoutePolicy::Codeg,
            true,
            &crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
        );
        let native_fp = comparison_route_fingerprint(
            AgentType::Codex,
            DelegationConnectionOrigin::Root,
            Some(DelegationRoutePolicy::Native),
            DelegationRoutePolicy::Codeg,
            true,
            &crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
        );
        let mgr = ConnectionManager::new();
        seed_route_root(&mgr, "inherited", None, &codeg_fp).await;
        seed_route_root(
            &mgr,
            "native-override",
            Some(DelegationRoutePolicy::Native),
            &native_fp,
        )
        .await;

        mgr.refresh_delegation_route_staleness(DelegationRoutePolicy::Native, true)
            .await;
        assert_eq!(
            manager_stale_kind(&mgr, "inherited").await,
            Some(ConfigStaleKind::DelegationRoute)
        );
        assert_eq!(manager_stale_kind(&mgr, "native-override").await, None);
    }

    #[tokio::test]
    async fn draft_route_change_marks_stale_without_mutating_launch_plan() {
        use crate::acp::delegation::route::{
            comparison_route_fingerprint, DelegationConnectionOrigin, DelegationRoutePolicy,
        };

        let codeg_fp = comparison_route_fingerprint(
            AgentType::Codex,
            DelegationConnectionOrigin::Root,
            Some(DelegationRoutePolicy::Codeg),
            DelegationRoutePolicy::Codeg,
            true,
            &crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
        );
        let mgr = ConnectionManager::new();
        seed_route_root(&mgr, "draft", Some(DelegationRoutePolicy::Codeg), &codeg_fp).await;
        let before = {
            let map = mgr.connections.lock().await;
            map.get("draft").unwrap().route_plan.clone()
        };

        mgr.set_draft_delegation_route_preference(
            "draft",
            Some(DelegationRoutePolicy::Native),
            DelegationRoutePolicy::Codeg,
            true,
        )
        .await
        .unwrap();

        assert_eq!(
            manager_stale_kind(&mgr, "draft").await,
            Some(ConfigStaleKind::DelegationRoute)
        );
        let after = {
            let map = mgr.connections.lock().await;
            map.get("draft").unwrap().route_plan.clone()
        };
        assert_eq!(after, before);
    }

    /// Subscribe directly to the per-connection event stream. Phase 4b
    /// removed the dual-broadcast through the global `WebEventBroadcaster`
    /// for ACP events; the per-connection stream is now the only delivery
    /// path tests can observe. Subscribe BEFORE triggering the producing
    /// call so events emitted between subscribe and recv buffer rather
    /// than drop.
    async fn subscribe_conn_stream(
        mgr: &ConnectionManager,
        conn_id: &str,
    ) -> broadcast::Receiver<std::sync::Arc<crate::acp::types::EventEnvelope>> {
        let state = mgr
            .get_state(conn_id)
            .await
            .expect("connection should be registered");
        let stream = state.read().await.event_stream();
        stream.subscribe()
    }

    /// Receive the first envelope from a per-connection stream. Times out
    /// after 200 ms to keep tests honest.
    async fn recv_first_acp_event(
        rx: &mut broadcast::Receiver<std::sync::Arc<crate::acp::types::EventEnvelope>>,
    ) -> crate::acp::types::EventEnvelope {
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for acp event")
            .expect("per-connection stream closed");
        (*evt).clone()
    }

    /// Drain the connection's command receiver (non-blocking) and return one
    /// entry per enqueued `Prompt` command: its attached `user_message` payload
    /// (the cross-client broadcast the loop emits before the agent request),
    /// flattened to `(message_id, text blocks)`. The inner `Option` is `None`
    /// for a `Prompt` carrying no user message (delegation child / unbound).
    /// The vec length is the number of `Prompt` commands enqueued — useful for
    /// asserting the concurrency gate stopped a second one. Call after the
    /// producing await.
    fn drain_prompt_user_messages(
        cmd_rx: &mut mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
    ) -> Vec<Option<(String, Vec<String>)>> {
        let mut out = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let crate::acp::connection::ConnectionCommand::Prompt { user_message, .. } = cmd {
                out.push(user_message.map(|(id, blocks)| {
                    let texts = blocks
                        .iter()
                        .filter_map(|b| match b {
                            crate::acp::types::UserMessageBlock::Text { text } => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<String>>();
                    (id, texts)
                }));
            }
        }
        out
    }

    /// A minimal non-empty prompt for tests that exercise linking / status /
    /// caller-id behavior and don't care about the prompt content. (Empty
    /// prompts are now rejected before any side effects, so these tests must
    /// pass real content to reach the link path.)
    fn one_text_block() -> Vec<PromptInputBlock> {
        vec![PromptInputBlock::Text {
            text: "test prompt".into(),
        }]
    }

    /// Live command receiver + linked conversation + enrolled auto-title job.
    /// Uses `insert_test_connection_live` so `reserve()` can succeed or block.
    struct PromptAdmissionFixture {
        db: AppDatabase,
        manager: ConnectionManager,
        connection_id: String,
        conversation_id: i32,
        #[allow(dead_code)]
        folder_id: i32,
        command_receiver: mpsc::Receiver<ConnectionCommand>,
    }

    impl PromptAdmissionFixture {
        async fn state(&self) -> Arc<RwLock<SessionState>> {
            self.manager
                .get_state(&self.connection_id)
                .await
                .expect("fixture connection state")
        }

        async fn fail_next_capture_transaction(&self) {
            use sea_orm::{ConnectionTrait, DbBackend, Statement};
            self.db
                .conn
                .execute(Statement::from_string(
                    DbBackend::Sqlite,
                    "CREATE TRIGGER fail_title_capture BEFORE UPDATE ON auto_title_jobs \
                     BEGIN SELECT RAISE(ABORT, 'capture failure'); END"
                        .to_owned(),
                ))
                .await
                .expect("install capture failure trigger");
        }

        async fn job_first_user_text(&self) -> Option<String> {
            use crate::db::entities::auto_title_job;
            use sea_orm::EntityTrait;
            auto_title_job::Entity::find_by_id(self.conversation_id)
                .one(&self.db.conn)
                .await
                .expect("query job")
                .and_then(|j| j.first_user_text)
        }

        async fn job_locale(&self) -> Option<String> {
            use crate::db::entities::auto_title_job;
            use sea_orm::EntityTrait;
            auto_title_job::Entity::find_by_id(self.conversation_id)
                .one(&self.db.conn)
                .await
                .expect("query job")
                .and_then(|j| j.locale)
        }
    }

    async fn prompt_admission_fixture() -> PromptAdmissionFixture {
        use crate::auto_title::{enable_title_api_for_test, title_key};
        use crate::db::test_helpers;
        use crate::models::system::AppLocale;

        let db = test_helpers::fresh_in_memory_db().await;
        // Hold suite only through enrollment; capture tests operate on the job
        // row without re-checking keyring presence for On.
        let _suite = title_key::test_hooks::SuiteGuard::enter();
        enable_title_api_for_test(&db.conn).await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/prompt-admission").await;
        let conversation =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("create conversation with job enrollment");

        let manager = ConnectionManager::new();
        let connection_id = "admission-conn".to_string();
        let command_receiver = manager
            .insert_test_connection_live(
                &connection_id,
                AgentType::ClaudeCode,
                Some(PathBuf::from("/tmp/prompt-admission")),
                EventEmitter::Noop,
            )
            .await;

        {
            let state = manager.get_state(&connection_id).await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = Some(conversation.id);
            s.folder_id = Some(folder_id);
            s.purpose = crate::auto_title::ConnectionPurpose::User;
            s.effective_locale = AppLocale::En;
            s.active_turn = None;
        }

        // Drop suite before returning so callers are not blocked on the exclusive lock.
        drop(_suite);

        PromptAdmissionFixture {
            db,
            manager,
            connection_id,
            conversation_id: conversation.id,
            folder_id,
            command_receiver,
        }
    }

    #[tokio::test]
    async fn capture_failure_prevents_enqueue_and_fast_completion_cannot_win() {
        use crate::auto_title::PromptCaptureContext;
        use crate::models::system::AppLocale;

        let mut fixture = prompt_admission_fixture().await;
        fixture.fail_next_capture_transaction().await;
        let result = fixture
            .manager
            .send_prompt(
                &fixture.db,
                &fixture.connection_id,
                one_text_block(),
                Some(PromptCaptureContext::new(
                    Some("visible".into()),
                    Some(AppLocale::ZhCn),
                )),
            )
            .await;
        assert!(result.is_err(), "capture failure must reject the send");
        assert!(
            fixture.command_receiver.try_recv().is_err(),
            "failed capture must not enqueue a Prompt command"
        );
        {
            let state_arc = fixture.state().await;
            let state = state_arc.read().await;
            assert!(
                state.active_turn.is_none(),
                "failed capture must leave active_turn unset"
            );
            assert!(
                !state.turn_in_flight,
                "failed capture must leave turn_in_flight clear"
            );
        }
        assert_eq!(
            fixture.job_first_user_text().await,
            None,
            "aborted capture must not persist first_user_text"
        );
    }

    #[tokio::test]
    async fn cancelled_while_reserving_stages_no_title_context() {
        use crate::auto_title::PromptCaptureContext;
        use crate::models::system::AppLocale;

        let fixture = prompt_admission_fixture().await;
        // Fill the live channel (capacity 4) so the next reserve() blocks.
        let tx = fixture
            .manager
            .connections
            .lock()
            .await
            .get(&fixture.connection_id)
            .unwrap()
            .cmd_tx
            .clone();
        for _ in 0..4 {
            tx.send(ConnectionCommand::Prompt {
                blocks: one_text_block(),
                user_message: None,
                mark_awaiting_reply: false,
                bypass_autonomous_hold: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        }

        let send_fut = fixture.manager.send_prompt(
            &fixture.db,
            &fixture.connection_id,
            one_text_block(),
            Some(PromptCaptureContext::new(
                Some("cancelled-visible".into()),
                Some(AppLocale::Ja),
            )),
        );
        let timed_out = tokio::time::timeout(std::time::Duration::from_millis(50), send_fut).await;
        assert!(
            timed_out.is_err(),
            "send must still be blocked on channel reserve"
        );

        {
            let state_arc = fixture.state().await;
            let state = state_arc.read().await;
            assert!(
                state.active_turn.is_none(),
                "cancellation during reserve stages no active_turn"
            );
            assert!(
                !state.turn_in_flight,
                "cancellation during reserve must not set turn_in_flight"
            );
        }
        assert_eq!(
            fixture.job_first_user_text().await,
            None,
            "cancellation during reserve stages no capture write"
        );
    }

    #[tokio::test]
    async fn accepted_prompt_persists_capture_before_immediate_completion() {
        use crate::auto_title::PromptCaptureContext;
        use crate::models::system::AppLocale;

        let mut fixture = prompt_admission_fixture().await;
        let result = fixture
            .manager
            .send_prompt(
                &fixture.db,
                &fixture.connection_id,
                one_text_block(),
                Some(PromptCaptureContext::new(
                    Some("persist-before-complete".into()),
                    Some(AppLocale::ZhCn),
                )),
            )
            .await;
        assert!(result.is_ok(), "accepted send: {result:?}");

        // Capture must already be durable before the agent can process the
        // enqueued command (immediate-completion race).
        assert_eq!(
            fixture.job_first_user_text().await.as_deref(),
            Some("persist-before-complete")
        );
        assert_eq!(fixture.job_locale().await.as_deref(), Some("zh_cn"));
        {
            let state_arc = fixture.state().await;
            let state = state_arc.read().await;
            assert!(state.turn_in_flight);
            let active = state
                .active_turn
                .as_ref()
                .expect("accepted prompt sets active_turn");
            assert_eq!(active.locale, AppLocale::ZhCn);
            assert!(!active.token.is_empty());
            assert_eq!(state.effective_locale, AppLocale::ZhCn);
            assert_eq!(state.parent_turn_generation, 1);
            assert_eq!(state.active_turn_generation, Some(1));
        }

        // Immediate completion path: receive the queued command with no delay.
        let cmd = fixture
            .command_receiver
            .try_recv()
            .expect("prompt must already be enqueued after successful admission");
        let ConnectionCommand::Prompt {
            turn_generation, ..
        } = cmd
        else {
            panic!("expected prompt command");
        };
        assert_eq!(turn_generation, 1);
    }

    #[tokio::test]
    async fn linked_and_already_linked_sends_share_capture_once() {
        use crate::auto_title::{enable_title_api_for_test, title_key, PromptCaptureContext};
        use crate::db::test_helpers;
        use crate::models::system::AppLocale;

        let db = test_helpers::fresh_in_memory_db().await;
        let _suite = title_key::test_hooks::SuiteGuard::enter();
        enable_title_api_for_test(&db.conn).await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/share-capture-once").await;
        let mgr = ConnectionManager::new();
        let conn_id = "share-once-conn";
        let mut cmd_rx = mgr
            .insert_test_connection_live(
                conn_id,
                AgentType::ClaudeCode,
                Some(PathBuf::from("/tmp/share-capture-once")),
                EventEmitter::Noop,
            )
            .await;

        // First linked send (Branch B create + single admission capture).
        let first = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "first linked task".into(),
                }],
                Some(folder_id),
                None,
                None,
                Some(PromptCaptureContext::new(
                    Some("first linked task".into()),
                    Some(AppLocale::En),
                )),
            )
            .await
            .expect("first linked send");
        let conversation_id = first.expect("conversation id bound");

        use crate::db::entities::auto_title_job;
        use sea_orm::EntityTrait;
        let job_after_first = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job after first send");
        assert_eq!(
            job_after_first.first_user_text.as_deref(),
            Some("first linked task")
        );
        assert_eq!(job_after_first.locale.as_deref(), Some("en"));

        // Drain + clear gate so the already-linked path can admit again.
        let _ = cmd_rx.try_recv();
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            let mut s = state.write().await;
            s.turn_in_flight = false;
            s.active_turn = None;
        }

        // Second already-linked send shares the same capture hook once:
        // locale refreshes, first_user_text stays write-once.
        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "second linked task".into(),
            }],
            Some(folder_id),
            None,
            None,
            Some(PromptCaptureContext::new(
                Some("second linked task".into()),
                Some(AppLocale::Ja),
            )),
        )
        .await
        .expect("already-linked send");

        let job_after_second = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job after second send");
        assert_eq!(
            job_after_second.first_user_text.as_deref(),
            Some("first linked task"),
            "first_user_text is write-once across linked + already-linked"
        );
        assert_eq!(
            job_after_second.locale.as_deref(),
            Some("ja"),
            "locale refreshes on the second admission"
        );
    }

    #[tokio::test]
    async fn reserve_failure_stages_no_capture() {
        use crate::auto_title::PromptCaptureContext;
        use crate::models::system::AppLocale;

        // Dropped command receiver: reserve() fails with ProcessExited.
        let fixture_db = {
            use crate::auto_title::{enable_title_api_for_test, title_key};
            use crate::db::test_helpers;
            let db = test_helpers::fresh_in_memory_db().await;
            let _suite = title_key::test_hooks::SuiteGuard::enter();
            enable_title_api_for_test(&db.conn).await;
            let folder_id = test_helpers::seed_folder(&db, "/tmp/reserve-fail").await;
            let conversation = conversation_service::create(
                &db.conn,
                folder_id,
                AgentType::ClaudeCode,
                None,
                None,
            )
            .await
            .unwrap();
            let mgr = ConnectionManager::new();
            let conn_id = "reserve-fail-conn";
            mgr.insert_test_connection(conn_id, AgentType::ClaudeCode, None, EventEmitter::Noop)
                .await;
            {
                let state = mgr.get_state(conn_id).await.unwrap();
                let mut s = state.write().await;
                s.conversation_id = Some(conversation.id);
                s.folder_id = Some(folder_id);
            }
            let err = mgr
                .send_prompt(
                    &db,
                    conn_id,
                    one_text_block(),
                    Some(PromptCaptureContext::new(
                        Some("never-written".into()),
                        Some(AppLocale::Ko),
                    )),
                )
                .await
                .expect_err("dropped receiver must fail reserve");
            assert!(
                matches!(err, AcpError::ProcessExited),
                "expected ProcessExited, got {err:?}"
            );
            let state = mgr.get_state(conn_id).await.unwrap();
            assert!(state.read().await.active_turn.is_none());
            assert!(!state.read().await.turn_in_flight);

            use crate::db::entities::auto_title_job;
            use sea_orm::EntityTrait;
            let job = auto_title_job::Entity::find_by_id(conversation.id)
                .one(&db.conn)
                .await
                .unwrap()
                .expect("job");
            assert_eq!(job.first_user_text, None);
            assert_eq!(job.locale, None);
            db
        };
        drop(fixture_db);
    }

    #[tokio::test]
    async fn unlinked_send_bypasses_capture() {
        use crate::auto_title::PromptCaptureContext;
        use crate::db::test_helpers;
        use crate::models::system::AppLocale;

        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let mut rx = mgr
            .insert_test_connection_live(
                "unlinked-conn",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;

        mgr.send_prompt(
            &db,
            "unlinked-conn",
            one_text_block(),
            Some(PromptCaptureContext::new(
                Some("should-not-need-job".into()),
                Some(AppLocale::Fr),
            )),
        )
        .await
        .expect("unlinked send succeeds without capture");

        assert!(matches!(
            rx.try_recv().expect("enqueued"),
            ConnectionCommand::Prompt { .. }
        ));
        let state = mgr.get_state("unlinked-conn").await.unwrap();
        let s = state.read().await;
        assert!(
            s.active_turn.is_none(),
            "unlinked path must not set active_turn"
        );
        assert!(s.turn_in_flight);
        assert_eq!(s.effective_locale, AppLocale::En);
    }

    #[tokio::test]
    async fn ordinary_send_prompt_rejects_internal_title_purpose() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let _rx = mgr
            .insert_test_connection_live(
                "reject-internal-title",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state("reject-internal-title").await.unwrap();
            state.write().await.purpose = ConnectionPurpose::InternalTitle;
        }
        let err = mgr
            .send_prompt(&db, "reject-internal-title", one_text_block(), None)
            .await
            .expect_err("ordinary send must reject InternalTitle");
        assert!(
            err.to_string().contains("hidden generation")
                || err.to_string().contains("InternalTitle")
                || err.to_string().contains("send_prompt_unlinked_internal"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn ordinary_send_prompt_rejects_internal_translate_purpose() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let _rx = mgr
            .insert_test_connection_live(
                "reject-internal-translate",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state("reject-internal-translate").await.unwrap();
            state.write().await.purpose = ConnectionPurpose::InternalTranslate;
        }
        let err = mgr
            .send_prompt(&db, "reject-internal-translate", one_text_block(), None)
            .await
            .expect_err("ordinary send must reject InternalTranslate");
        assert!(
            err.to_string().contains("hidden generation")
                || err.to_string().contains("send_prompt_unlinked_internal"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn identity_and_subscribe_recovers_session_started_before_subscribe_for_internal_title() {
        let mgr = ConnectionManager::new();
        let _rx = mgr
            .insert_test_connection_live("id-sub-conn", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        {
            let state = mgr.get_state("id-sub-conn").await.unwrap();
            state.write().await.purpose = ConnectionPurpose::InternalTitle;
        }
        // Apply SessionStarted before subscribe — snapshot branch must see it.
        {
            let state = mgr.get_state("id-sub-conn").await.unwrap();
            emit_with_state(
                &state,
                &EventEmitter::Noop,
                AcpEvent::SessionStarted {
                    session_id: "ext-pre".into(),
                },
            )
            .await;
        }
        let (id, _rx) = mgr
            .identity_and_subscribe("id-sub-conn")
            .await
            .expect("identity");
        assert_eq!(id.as_deref(), Some("ext-pre"));
    }

    #[tokio::test]
    async fn noop_emitter_keeps_internal_title_events_off_transport_and_lifecycle_bus() {
        // Noop has no ACP bus / transport target — do not use an unattached
        // bus as "proof" of isolation (that bus was never on the emit path).
        assert!(
            EventEmitter::Noop.acp_event_bus().is_none(),
            "EventEmitter::Noop must expose no ACP internal bus"
        );

        let mgr = ConnectionManager::new();
        let _rx = mgr
            .insert_test_connection_live(
                "noop-internal-title",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state("noop-internal-title").await.unwrap();
            state.write().await.purpose = ConnectionPurpose::InternalTitle;
        }
        let state = mgr.get_state("noop-internal-title").await.unwrap();
        let (_id, mut private_rx) = mgr
            .identity_and_subscribe("noop-internal-title")
            .await
            .expect("subscribe");

        emit_with_state(
            &state,
            &EventEmitter::Noop,
            AcpEvent::ContentDelta {
                text: "title delta".into(),
                parent_tool_use_id: None,
            },
        )
        .await;

        // Private stream receives events for the title runner.
        let first = private_rx.try_recv().expect("private ContentDelta");
        assert!(matches!(first.payload, AcpEvent::ContentDelta { .. }));
    }

    #[tokio::test]
    async fn internal_helper_rejects_user_and_delegation_accepts_internal() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let mut rx = mgr
            .insert_test_connection_live(
                "internal-helper-conn",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;

        // Default purpose is User → reject.
        let err = mgr
            .send_prompt_unlinked_internal("internal-helper-conn", one_text_block())
            .await
            .expect_err("User purpose rejected");
        assert!(
            matches!(err, AcpError::Protocol(_)) || err.to_string().contains("internal"),
            "unexpected error: {err:?}"
        );

        {
            let state = mgr.get_state("internal-helper-conn").await.unwrap();
            state.write().await.purpose = crate::auto_title::ConnectionPurpose::Delegation;
        }
        let err = mgr
            .send_prompt_unlinked_internal("internal-helper-conn", one_text_block())
            .await
            .expect_err("Delegation purpose rejected");
        assert!(
            matches!(err, AcpError::Protocol(_)) || err.to_string().contains("internal"),
            "unexpected error: {err:?}"
        );

        {
            let state = mgr.get_state("internal-helper-conn").await.unwrap();
            state.write().await.purpose = crate::auto_title::ConnectionPurpose::InternalProbe;
        }
        mgr.send_prompt_unlinked_internal("internal-helper-conn", one_text_block())
            .await
            .expect("InternalProbe accepted");
        assert!(matches!(
            rx.try_recv().expect("probe enqueued"),
            ConnectionCommand::Prompt { .. }
        ));
        {
            let state = mgr.get_state("internal-helper-conn").await.unwrap();
            let mut s = state.write().await;
            s.turn_in_flight = false;
            s.purpose = crate::auto_title::ConnectionPurpose::InternalTitle;
        }
        mgr.send_prompt_unlinked_internal("internal-helper-conn", one_text_block())
            .await
            .expect("InternalTitle accepted");
        assert!(matches!(
            rx.try_recv().expect("title enqueued"),
            ConnectionCommand::Prompt { .. }
        ));
        {
            let state = mgr.get_state("internal-helper-conn").await.unwrap();
            let mut s = state.write().await;
            s.turn_in_flight = false;
            s.purpose = crate::auto_title::ConnectionPurpose::InternalTranslate;
        }
        mgr.send_prompt_unlinked_internal("internal-helper-conn", one_text_block())
            .await
            .expect("InternalTranslate accepted");
        assert!(matches!(
            rx.try_recv().expect("translate enqueued"),
            ConnectionCommand::Prompt { .. }
        ));
        // Internal path never stages title capture context.
        let state = mgr.get_state("internal-helper-conn").await.unwrap();
        assert!(state.read().await.active_turn.is_none());
        let _ = db; // keep db alive for API symmetry with other admission tests
    }

    #[tokio::test]
    async fn prompt_wrappers_encode_user_facing_and_background_attention() {
        use crate::db::test_helpers;

        let mgr = ConnectionManager::new();
        let mut rx = mgr
            .insert_test_connection_live("policy-conn", AgentType::Codex, None, EventEmitter::Noop)
            .await;

        let policy_db = crate::db::test_helpers::fresh_in_memory_db().await;
        mgr.send_prompt(&policy_db, "policy-conn", one_text_block(), None)
            .await
            .expect("UI prompt");
        let ConnectionCommand::Prompt {
            mark_awaiting_reply,
            ..
        } = rx.recv().await.unwrap()
        else {
            panic!("expected prompt command");
        };
        assert!(mark_awaiting_reply);

        {
            let state = mgr.get_state("policy-conn").await.unwrap();
            state.write().await.turn_in_flight = false;
        }
        mgr.send_prompt_background(&policy_db, "policy-conn", one_text_block())
            .await
            .expect("background prompt");
        let ConnectionCommand::Prompt {
            mark_awaiting_reply,
            ..
        } = rx.recv().await.unwrap()
        else {
            panic!("expected background prompt command");
        };
        assert!(!mark_awaiting_reply);

        // Automation uses the linked-background public API (hard-codes
        // mark_awaiting_reply=false). Exercise that path against a live
        // connection + real in-memory conversation, not the private impl.
        {
            let state = mgr.get_state("policy-conn").await.unwrap();
            state.write().await.turn_in_flight = false;
        }
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/policy-linked-bg").await;
        let conversation =
            conversation_service::create(&db.conn, folder_id, AgentType::Codex, None, None)
                .await
                .expect("seed conversation");
        mgr.send_prompt_linked_background(
            &db,
            "policy-conn",
            one_text_block(),
            Some(folder_id),
            Some(conversation.id),
            None,
        )
        .await
        .expect("linked background prompt");
        let ConnectionCommand::Prompt {
            mark_awaiting_reply,
            ..
        } = rx.recv().await.unwrap()
        else {
            panic!("expected linked background prompt command");
        };
        assert!(
            !mark_awaiting_reply,
            "send_prompt_linked_background must enqueue mark_awaiting_reply=false"
        );
    }

    #[tokio::test]
    async fn archived_workflow_prompt_surfaces_fence_root_and_bound_child_without_side_effects() {
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, PaginatorTrait, Set};

        use crate::db::entities::delegation_task_run::{AdmissionClass, DelegationRunStatus};
        use crate::db::entities::delegation_workflow::{
            self, CompletionProtocolMode, WorkflowState,
        };
        use crate::db::entities::{
            conversation, delegation_attention_request, delegation_lineage_budget,
            delegation_plan_round_authorization, delegation_task_run, delegation_work_unit_budget,
            delegation_workflow_manifest_revision, delegation_workflow_run_binding,
            recovery_authorization,
        };
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/archived-background-prompt").await;
        let root = seed_conversation(&db, folder, AgentType::Codex).await;
        let now = Utc::now();
        delegation_workflow::ActiveModel {
            workflow_id: Set("archived-background-prompt".into()),
            parent_conversation_id: Set(root),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(2),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v2".into()),
            publication_token: Set("archived-background-prompt-token".into()),
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
        let child = seed_conversation(&db, folder, AgentType::Codex).await;
        delegation_task_run::ActiveModel {
            task_id: Set("archived-background-child-task".into()),
            root_task_id: Set("archived-background-child-task".into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(root),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set("codex".into()),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set("archived-background-child-task".into()),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set("archived-background-child-task".into()),
            workflow_id: Set("archived-background-prompt".into()),
            node_id: Set("task-1".into()),
            manifest_revision: Set(1),
            lineage_ordinal: Set(1),
            summary_validated: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let manager = ConnectionManager::new();
        let mut receiver = manager
            .insert_test_connection_live(
                "archived-background",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;
        let state = manager.get_state("archived-background").await.unwrap();
        state.write().await.conversation_id = Some(root);
        let mut child_receiver = manager
            .insert_test_connection_live(
                "archived-bound-child",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;
        let child_state = manager.get_state("archived-bound-child").await.unwrap();
        child_state.write().await.conversation_id = Some(child);

        let before = (
            conversation::Entity::find().count(&db.conn).await.unwrap(),
            delegation_task_run::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_lineage_budget::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_work_unit_budget::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_plan_round_authorization::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            recovery_authorization::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_attention_request::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_workflow_manifest_revision::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
        );
        for (connection_id, conversation_id, conversation_state) in [
            ("archived-background", root, &state),
            ("archived-bound-child", child, &child_state),
        ] {
            let results = [
                manager
                    .send_prompt(&db, connection_id, one_text_block(), None)
                    .await,
                manager
                    .send_prompt_background(&db, connection_id, one_text_block())
                    .await,
                manager
                    .send_prompt_linked(
                        &db,
                        connection_id,
                        one_text_block(),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .map(|_| ()),
                manager
                    .send_prompt_linked_background(
                        &db,
                        connection_id,
                        one_text_block(),
                        None,
                        None,
                        None,
                    )
                    .await
                    .map(|_| ()),
                crate::chat_channel::session_commands::send_prompt_linked_for_chat(
                    &db.conn,
                    &manager,
                    connection_id,
                    conversation_id,
                    "archived chat prompt",
                )
                .await,
            ];
            for error in results.into_iter().map(Result::unwrap_err) {
                assert!(matches!(
                    error,
                    AcpError::WorkflowV2Retired {
                        source_conversation_id: Some(id),
                        successor_conversation_id: None,
                        can_create_simple_successor: false,
                    } if id == root
                ));
            }
            assert!(!conversation_state.read().await.turn_in_flight);
        }
        assert!(receiver.try_recv().is_err());
        assert!(child_receiver.try_recv().is_err());
        let after = (
            conversation::Entity::find().count(&db.conn).await.unwrap(),
            delegation_task_run::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_lineage_budget::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_work_unit_budget::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_plan_round_authorization::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            recovery_authorization::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_attention_request::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_workflow_manifest_revision::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
        );
        assert_eq!(after, before);
    }

    /// Insert a connection with a LIVE command receiver so `send_prompt_inner`'s
    /// enqueue SUCCEEDS (the UserMessage broadcast is deferred until after a
    /// successful enqueue). Returns the receiver — keep it in scope for the
    /// test, otherwise the channel closes and the send fails.
    async fn insert_live_connection(
        mgr: &ConnectionManager,
        conn_id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
    ) -> tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand> {
        use crate::acp::connection::AgentConnection;
        use crate::acp::session_state::SessionState;
        let (tx, rx, _liveness_rx) = connection_channel(4);
        let mut state = SessionState::new(
            conn_id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: conn_id.to_string(),
            agent_type,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation: state.connection_incarnation.clone(),
            tool_lease_registry: state.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .0,
            observed_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .1,
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        mgr.connections
            .lock()
            .await
            .insert(conn_id.to_string(), conn);
        rx
    }

    /// Put a turn in flight on a connection inserted by
    /// [`insert_live_connection`] (which starts them `Connected`).
    async fn mark_prompting(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.expect("connection");
        state.write().await.status = ConnectionStatus::Prompting;
    }

    /// Mirror what a live `active` goal snapshot leaves on the state.
    async fn mark_goal_active(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.expect("connection");
        state.write().await.goal_active = true;
    }

    /// Stand in for the connection loop's `GoalControl` arm: take the command,
    /// assert the action, answer `landed`, and hand the receiver back so the
    /// test can see what (if anything) the manager enqueues next. Returns a
    /// JoinHandle because `goal_control` blocks on that answer.
    fn answer_goal_control(
        mut rx: tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
        expected: GoalControlAction,
        landed: bool,
    ) -> tokio::task::JoinHandle<
        tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
    > {
        tokio::spawn(async move {
            match rx.recv().await.expect("goal control enqueued") {
                ConnectionCommand::GoalControl { action, reply } => {
                    assert_eq!(action, expected);
                    reply
                        .expect("an interrupting caller attaches a reply")
                        .send(landed)
                        .expect("manager is listening");
                }
                _ => panic!("expected a GoalControl command"),
            }
            rx
        })
    }

    #[tokio::test]
    async fn a_codex_goal_pause_mid_turn_interrupts_the_turn_after_the_goal_rpc() {
        // codex's pause is app-server metadata: it stops the goal from starting
        // ANOTHER turn but never touches the one that is running, so without
        // the interrupt the user clicks Pause and watches the agent keep
        // working. Order matters — the goal RPC has to LAND first, so the goal
        // is already non-active when the turn aborts and nothing auto-continues
        // on the way out.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let rx = insert_live_connection(&mgr, "c-goal-codex", AgentType::Codex, None).await;
        mark_prompting(&mgr, "c-goal-codex").await;
        mark_goal_active(&mgr, "c-goal-codex").await;
        let loop_stub = answer_goal_control(rx, GoalControlAction::Pause, true);

        mgr.goal_control(&db.conn, "c-goal-codex", GoalControlAction::Pause)
            .await
            .unwrap();

        let mut rx = loop_stub.await.unwrap();
        assert!(
            rx.try_recv().is_err(),
            "user cancel is sent on the control lane, not as ConnectionCommand"
        );
        assert!(rx.try_recv().is_err(), "nothing else is enqueued");
    }

    #[tokio::test]
    async fn a_rejected_goal_control_leaves_the_turn_alone() {
        // The agent refused the pause (bad params, unknown session, dead
        // process). The goal is still active, so killing the turn would destroy
        // in-flight work AND let codex resume the goal at the next idle point —
        // strictly worse than the error banner the loop already emitted.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let rx = insert_live_connection(&mgr, "c-goal-refused", AgentType::Codex, None).await;
        mark_prompting(&mgr, "c-goal-refused").await;
        mark_goal_active(&mgr, "c-goal-refused").await;
        let loop_stub = answer_goal_control(rx, GoalControlAction::Pause, false);

        mgr.goal_control(&db.conn, "c-goal-refused", GoalControlAction::Pause)
            .await
            .unwrap();

        let mut rx = loop_stub.await.unwrap();
        assert!(rx.try_recv().is_err(), "a failed control cancels nothing");
    }

    #[tokio::test]
    async fn a_codex_goal_clear_interrupts_even_when_codeg_thinks_it_is_idle() {
        // Codeg's `Prompting` only covers turns IT started. A goal loop's
        // continuations are started by codex — detached turns no host request
        // owns — so the session reads "connected" while the agent is very much
        // working, which is precisely when the user hits Clear. Gating on our
        // own turn bookkeeping would skip the interrupt exactly there, so the
        // interrupt is not gated on liveness: with an ACTIVE goal, whatever is
        // running is the goal's work, and with nothing running the interrupt is
        // the same harmless no-op the Stop button is.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let rx = insert_live_connection(&mgr, "c-goal-idle", AgentType::Codex, None).await;
        mark_goal_active(&mgr, "c-goal-idle").await;
        let loop_stub = answer_goal_control(rx, GoalControlAction::Clear, true);

        mgr.goal_control(&db.conn, "c-goal-idle", GoalControlAction::Clear)
            .await
            .unwrap();

        let mut rx = loop_stub.await.unwrap();
        assert!(
            rx.try_recv().is_err(),
            "user cancel is sent on the control lane, not as ConnectionCommand"
        );
        assert!(rx.try_recv().is_err(), "nothing else is enqueued");
    }

    #[tokio::test]
    async fn clearing_a_paused_goal_never_touches_the_users_own_turn() {
        // A paused goal drives nothing, so a turn running alongside it is
        // something the user started themselves. Dismissing the stale card is
        // housekeeping — killing that prompt (and draining its permissions, and
        // cascading to its delegations) would be destroying unrelated work.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let mut rx = insert_live_connection(&mgr, "c-goal-paused", AgentType::Codex, None).await;
        mark_prompting(&mgr, "c-goal-paused").await; // the user's own prompt
                                                     // `goal_active` stays false: the last snapshot was `paused`.

        mgr.goal_control(&db.conn, "c-goal-paused", GoalControlAction::Clear)
            .await
            .unwrap();

        match rx.try_recv() {
            Ok(ConnectionCommand::GoalControl { action, reply }) => {
                assert_eq!(action, GoalControlAction::Clear);
                assert!(reply.is_none(), "no interrupt is intended, so none waits");
            }
            _ => panic!("expected a GoalControl command"),
        }
        assert!(rx.try_recv().is_err(), "the user's turn survives");
    }

    #[tokio::test]
    async fn a_claude_goal_clear_never_interrupts_the_turn_carrying_it() {
        // claude-agent-acp implements `_session/goal` by STEERING the text
        // "/goal clear" into the running turn (or prompting when idle).
        // Cancelling would kill the message that carries the clear and leave
        // the goal armed — worse than doing nothing. Same for every adapter
        // whose control channel we haven't verified. No interrupt is intended,
        // so no reply is even asked for and the call doesn't wait.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let mut rx =
            insert_live_connection(&mgr, "c-goal-claude", AgentType::ClaudeCode, None).await;
        mark_prompting(&mgr, "c-goal-claude").await;
        mark_goal_active(&mgr, "c-goal-claude").await;

        mgr.goal_control(&db.conn, "c-goal-claude", GoalControlAction::Clear)
            .await
            .unwrap();

        match rx.try_recv() {
            Ok(ConnectionCommand::GoalControl { action, reply }) => {
                assert_eq!(action, GoalControlAction::Clear);
                assert!(reply.is_none(), "nobody waits on an in-band control");
            }
            _ => panic!("expected a GoalControl command"),
        }
        assert!(rx.try_recv().is_err(), "the carrying turn survives");
    }

    #[tokio::test]
    async fn send_prompt_linked_attaches_user_message_to_prompt_for_root() {
        // A root send attaches the projected user-message payload to the
        // enqueued Prompt command (the connection loop emits the UserMessage
        // event itself, ordered before the agent request). With a live receiver
        // the enqueue succeeds and the payload is observable on the command.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-root").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-root";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-root")),
        )
        .await;

        let result = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "hello viewers".into(),
                }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        assert!(
            result.is_ok(),
            "enqueue should succeed with a live receiver"
        );

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(prompts.len(), 1, "exactly one Prompt enqueued");
        let um = prompts[0]
            .as_ref()
            .expect("root Prompt carries a user_message");
        assert!(
            um.0.starts_with("user-"),
            "connection-scoped id fallback, got {:?}",
            um.0
        );
        assert!(
            um.1.iter().any(|t| t == "hello viewers"),
            "user_message must carry the prompt text, got {um:?}"
        );
        // Live UI / UserMessage broadcast uses original user content only.
        // Wire-only `<codeg_terminal_context>` is appended in the connection
        // loop after this payload is captured for broadcast.
        assert!(
            um.1.iter().all(|t| !t.contains("codeg_terminal_context")),
            "user_message must never leak terminal context block, got {um:?}"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_rejects_second_prompt_while_turn_in_flight() {
        // Two clients co-controlling one connection can send near-
        // simultaneously. The first accepted prompt marks the turn in flight;
        // the second must be REJECTED with TurnInProgress (not enqueued behind
        // the active turn and silently dropped by the loop) so the frontend can
        // re-queue it. Only one Prompt reaches the connection loop.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-gate").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-gate";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-gate")),
        )
        .await;

        let first = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "first".into(),
                }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        assert!(first.is_ok(), "first prompt accepted");

        let second = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "second".into(),
                }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        assert!(
            matches!(second, Err(AcpError::TurnInProgress)),
            "second concurrent prompt must be rejected with TurnInProgress, got {second:?}"
        );

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(
            prompts.len(),
            1,
            "only the first prompt reaches the loop; the second is rejected, not queued"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_rejects_empty_prompt_without_wedging_gate() {
        // An empty prompt is rejected BEFORE any side effects: it must NOT
        // create/link a conversation row, must NOT set the concurrency gate
        // (which — with no TurnComplete to clear it — would 409 every future
        // send), and the connection must stay usable for a real prompt.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-empty").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-empty";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-empty")),
        )
        .await;

        let rows_before = count_conversation_rows(&db).await;
        let empty = mgr
            .send_prompt_linked(&db, conn_id, vec![], Some(folder_id), None, None, None)
            .await;
        assert!(empty.is_err(), "an empty prompt must be rejected");
        assert_eq!(
            count_conversation_rows(&db).await,
            rows_before,
            "a rejected empty prompt must NOT create/link a conversation row"
        );
        assert!(
            !mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .turn_in_flight,
            "a rejected empty prompt must NOT set the concurrency gate"
        );

        // The connection is not wedged: a real prompt afterwards is accepted.
        let ok = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text { text: "hi".into() }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        assert!(
            ok.is_ok(),
            "a real prompt after an empty one must still be accepted"
        );
        assert_eq!(
            drain_prompt_user_messages(&mut cmd_rx).len(),
            1,
            "exactly the one real prompt reached the loop"
        );
    }

    #[tokio::test]
    async fn send_prompt_returns_turn_in_progress_when_busy() {
        // The non-linked `send_prompt` path (used by the chat channel) must
        // surface `TurnInProgress` — NOT a connection-loss error — when a turn
        // is already in flight, so the chat channel treats it as a transient
        // busy rejection instead of tearing down the session.
        let mgr = ConnectionManager::new();
        let conn_id = "conn-busy";
        let _rx = insert_live_connection(&mgr, conn_id, AgentType::ClaudeCode, None).await;
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .turn_in_flight = true;

        let busy_db = crate::db::test_helpers::fresh_in_memory_db().await;
        let res = mgr
            .send_prompt(
                &busy_db,
                conn_id,
                vec![PromptInputBlock::Text { text: "hi".into() }],
                None,
            )
            .await;
        assert!(
            matches!(res, Err(AcpError::TurnInProgress)),
            "send_prompt must return TurnInProgress when a turn is in flight, got {res:?}"
        );
    }

    #[tokio::test]
    async fn continuation_gate_linked_rejects_arming_before_turn_in_flight_without_side_effects() {
        use crate::acp::delegation::continuation::store::{
            ContinuationStore, InMemoryContinuationStore, NewContinuation,
        };
        use chrono::{Duration, Utc};

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, "/tmp/continuation-gate").await;
        let conversation =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("seed conversation");
        let manager = ConnectionManager::new();
        let conn_id = "continuation-gate-linked";
        let mut command_receiver =
            insert_live_connection(&manager, conn_id, AgentType::ClaudeCode, None).await;
        let mut events = subscribe_conn_stream(&manager, conn_id).await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let armed_at = Utc::now();
        store
            .insert_arming(NewContinuation {
                continuation_id: "continuation-gate".into(),
                parent_conversation_id: conversation.id,
                parent_session_id: "session".into(),
                parent_connection_id: conn_id.into(),
                parent_turn_generation: 1,
                task_ids: crate::acp::delegation::continuation::types::ContinuationTaskIds(vec![
                    "task".into(),
                ]),
                armed_at,
                wake_at: armed_at + Duration::minutes(4),
                internal_prompt_id: "internal-prompt".into(),
                internal_prompt_marker: "marker".into(),
            })
            .await
            .expect("arm continuation");
        manager.install_continuation_store(store);
        manager
            .get_state(conn_id)
            .await
            .expect("state")
            .write()
            .await
            .turn_in_flight = true;

        let rows_before = count_conversation_rows(&db).await;
        let result = manager
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(conversation.id),
                None,
                None,
            )
            .await;

        assert!(
            matches!(
                result,
                Err(AcpError::ContinuationInProgress {
                    conversation_id,
                    state: crate::acp::delegation::continuation::types::ContinuationState::Arming,
                }) if conversation_id == conversation.id
            ),
            "active continuation must win over turn_in_flight, got {result:?}"
        );
        assert_eq!(count_conversation_rows(&db).await, rows_before);
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        let row = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .expect("read conversation")
            .expect("conversation exists");
        assert_eq!(
            row.status,
            ConversationStatus::InProgress,
            "rejected prompt must not write a conversation status"
        );
        assert!(
            command_receiver.try_recv().is_err(),
            "rejected prompt must not enqueue ConnectionCommand::Prompt"
        );
        assert!(
            events.try_recv().is_err(),
            "rejected prompt must not emit ConversationLinked, status, or UserMessage events"
        );
        let state = manager.get_state(conn_id).await.expect("state");
        assert_eq!(
            state.read().await.conversation_id,
            None,
            "rejected prompt must not link the conversation"
        );
    }

    #[tokio::test]
    async fn continuation_gate_nonlinked_public_paths_reject_arming_before_turn_in_flight() {
        use crate::acp::delegation::continuation::store::{
            ContinuationStore, InMemoryContinuationStore, NewContinuation,
        };
        use chrono::{Duration, Utc};

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id =
            crate::db::test_helpers::seed_folder(&db, "/tmp/continuation-gate-direct").await;
        let conversation =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("seed conversation");
        let manager = ConnectionManager::new();
        let conn_id = "continuation-gate-direct";
        let mut command_receiver =
            insert_live_connection(&manager, conn_id, AgentType::ClaudeCode, None).await;
        manager
            .get_state(conn_id)
            .await
            .expect("state")
            .write()
            .await
            .conversation_id = Some(conversation.id);
        let store = Arc::new(InMemoryContinuationStore::default());
        let armed_at = Utc::now();
        store
            .insert_arming(NewContinuation {
                continuation_id: "continuation-gate-direct".into(),
                parent_conversation_id: conversation.id,
                parent_session_id: "session".into(),
                parent_connection_id: conn_id.into(),
                parent_turn_generation: 1,
                task_ids: crate::acp::delegation::continuation::types::ContinuationTaskIds(vec![
                    "task".into(),
                ]),
                armed_at,
                wake_at: armed_at + Duration::minutes(4),
                internal_prompt_id: "internal-prompt".into(),
                internal_prompt_marker: "marker".into(),
            })
            .await
            .expect("arm continuation");
        manager.install_continuation_store(store);
        manager
            .get_state(conn_id)
            .await
            .expect("state")
            .write()
            .await
            .turn_in_flight = true;

        let foreground = manager
            .send_prompt(&db, conn_id, one_text_block(), None)
            .await;
        let background = manager
            .send_prompt_background(&db, conn_id, one_text_block())
            .await;
        for result in [foreground, background] {
            assert!(
                matches!(
                    result,
                    Err(AcpError::ContinuationInProgress {
                        conversation_id,
                        state: crate::acp::delegation::continuation::types::ContinuationState::Arming,
                    }) if conversation_id == conversation.id
                ),
                "active continuation must win over turn_in_flight, got {result:?}"
            );
        }
        assert!(
            command_receiver.try_recv().is_err(),
            "rejected public prompt paths must not enqueue ConnectionCommand::Prompt"
        );
    }

    #[tokio::test]
    async fn fork_session_rejects_when_turn_in_flight() {
        // A fork re-points the live session; it must not run while a turn is in
        // flight (a racing send would route to the wrong session, and the Fork
        // command would be dropped by the in-turn loop). It rejects with
        // TurnInProgress so the caller re-queues, WITHOUT enqueuing a Fork.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-fork-busy";
        let mut cmd_rx = insert_live_connection(&mgr, conn_id, AgentType::ClaudeCode, None).await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = Some(7); // fork requires a linked row
            s.turn_in_flight = true; // a turn is already running
        }

        let res = mgr.fork_session(&db, conn_id, None, None).await;
        assert!(
            matches!(res, Err(AcpError::TurnInProgress)),
            "fork must reject with TurnInProgress while a turn is in flight, got {res:?}"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "a rejected fork must NOT enqueue a Fork command"
        );
    }

    #[tokio::test]
    async fn fork_session_failure_leaves_gate_clear_and_lock_free() {
        // A fork holds `prompt_lock` for its whole critical section and never
        // SETS `turn_in_flight`, so even when the fork FAILS (here: a dead
        // command receiver makes the `Fork` send error) the connection isn't
        // wedged — the gate stays clear and the prompt lock is released on the
        // error path. (A fork emits no TurnComplete, so a gate it had set would
        // have had nothing to clear it.)
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-fork-fail";
        // insert_fake_connection drops the cmd receiver → the Fork send fails.
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(9);

        let res = mgr.fork_session(&db, conn_id, None, None).await;
        assert!(res.is_err(), "fork with a dead receiver must fail");
        assert!(
            !mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .turn_in_flight,
            "a failed fork must leave the gate clear"
        );
        let lock = mgr.clone_prompt_lock(conn_id).await.unwrap();
        assert!(
            lock.try_lock().is_ok(),
            "a failed fork must release prompt_lock so the connection stays usable"
        );
    }

    #[tokio::test]
    async fn fork_persists_despite_caller_cancellation() {
        // Cancellation-shield regression. Once `fork_session` enqueues the `Fork`
        // command, the connection loop re-points the live session to S2 and emits
        // `SessionStarted{S2}` REGARDLESS of caller liveness (it ignores a dead
        // reply channel). So the DB persistence that records the two-row layout
        // must NOT be tied to the caller's future — a dropped caller (HTTP client
        // disconnect) must not strand the live session on S2 with the pre-fork S1
        // history orphaned. We drop the caller mid-fork (reply withheld), then
        // release the reply and assert the detached task STILL persists the
        // current row (→ S2, `[Fork]` title) and the sibling (→ S1).
        use crate::acp::connection::ConnectionCommand;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-shield").await;
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Topic".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::update_external_id(&db.conn, pre.id, "session-S1".into())
            .await
            .unwrap();

        // Connection with a GATED fake fork reply: withheld until `go_tx` fires,
        // so we can drop the caller before the reply (and thus the persistence).
        let (tx, mut rx, _liveness_rx) = connection_channel(4);
        let mut state = SessionState::new(
            "c-shield".to_string(),
            AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = Some(pre.id);
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: "c-shield".to_string(),
            agent_type: AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation: state.connection_incarnation.clone(),
            tool_lease_registry: state.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .0,
            observed_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .1,
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mgr = ConnectionManager::new();
        mgr.connections
            .lock()
            .await
            .insert("c-shield".to_string(), conn);

        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let fake_loop = tokio::spawn(async move {
            if let Some(ConnectionCommand::Fork { reply }) = rx.recv().await {
                go_rx.await.ok(); // withhold the reply until the test releases it
                let _ = reply.send(Ok(crate::acp::types::ForkProtocolResult {
                    forked_session_id: "session-S2".into(),
                    original_session_id: "session-S1".into(),
                }));
            }
            rx // keep the receiver alive
        });

        // Drive fork under a short timeout: it spawns the shielded task (which
        // enqueues `Fork` and blocks on the withheld reply), then the timeout
        // DROPS this caller future. The detached persistence task must survive.
        let timed = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mgr.fork_session(&db, "c-shield", None, None),
        )
        .await;
        assert!(
            timed.is_err(),
            "caller must be dropped before the withheld reply is delivered"
        );

        // Nothing persisted yet (reply still withheld) — the row is untouched.
        let mid = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            mid.external_id.as_deref(),
            Some("session-S1"),
            "fork must not persist before the protocol reply"
        );

        // Release the reply: the DETACHED task completes the persistence even
        // though the caller is long gone.
        go_tx.send(()).ok();
        let _ = fake_loop.await;

        // Poll (bounded) until the two-row layout appears.
        let mut persisted = false;
        for _ in 0..200 {
            let current = conversation_service::get_by_id(&db.conn, pre.id)
                .await
                .unwrap();
            let rows = conversation::Entity::find().all(&db.conn).await.unwrap();
            let has_sibling = rows
                .iter()
                .any(|r| r.id != pre.id && r.external_id.as_deref() == Some("session-S1"));
            if current.external_id.as_deref() == Some("session-S2")
                && current.title.as_deref() == Some("[Fork] Topic")
                && has_sibling
            {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            persisted,
            "fork persistence must complete despite caller cancellation"
        );
    }

    #[tokio::test]
    async fn send_prompt_inner_does_not_set_gate_while_blocked_on_capacity() {
        // Cancellation-safety: the gate is set only AFTER reserving channel
        // capacity, with no await between the set and the send. If the future is
        // dropped while awaiting capacity (channel full), `turn_in_flight` must
        // remain false — otherwise a cancelled send would wedge the connection.
        let mgr = ConnectionManager::new();
        let conn_id = "conn-cancel";
        let _rx = insert_live_connection(&mgr, conn_id, AgentType::ClaudeCode, None).await;
        // Fill the command channel to capacity (4, per insert_live_connection)
        // by sending DIRECTLY on the cloned sender — bypassing the gate — so the
        // next reserve() blocks.
        let tx = mgr
            .connections
            .lock()
            .await
            .get(conn_id)
            .unwrap()
            .cmd_tx
            .clone();
        for _ in 0..4 {
            tx.send(crate::acp::connection::ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "filler".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                bypass_autonomous_hold: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        }

        // send_prompt_inner now blocks on reserve(); drop it via a short timeout.
        let fut = mgr.send_prompt_inner(
            None,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "blocked".into(),
            }],
            None,
            false,
            true,
            true,
            None,
        );
        let res = tokio::time::timeout(std::time::Duration::from_millis(50), fut).await;
        assert!(
            res.is_err(),
            "send_prompt_inner should still be blocked on channel capacity"
        );
        assert!(
            !mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .turn_in_flight,
            "the gate must NOT be set while blocked on channel capacity (cancellation-safe)"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_uses_client_message_id_for_user_message() {
        // The UI threads its optimistic turn id as `client_message_id`; the
        // broadcast UserMessage must carry it verbatim so the sender dedups its
        // own echo by exact id (not a heuristic).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-cmid").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-cmid";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-cmid")),
        )
        .await;

        mgr.send_prompt_linked_with_message_id(
            &db,
            conn_id,
            vec![PromptInputBlock::Text { text: "hi".into() }],
            Some(folder_id),
            None,
            None,
            Some("optimistic-abc".to_string()),
            None,
        )
        .await
        .expect("send");

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(
            prompts
                .first()
                .and_then(|um| um.as_ref())
                .map(|(id, _)| id.as_str()),
            Some("optimistic-abc"),
            "Prompt's user_message must carry the client-supplied message_id verbatim"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_failed_reserve_leaves_gate_clear() {
        // A failed enqueue (dropped cmd receiver) fails at the channel
        // `reserve()` step — which is BEFORE the turn-in-flight gate is set — so
        // the gate is never set, not "rolled back". The connection must stay
        // usable (turn_in_flight false), and the row rolls back to Cancelled.
        // pending_user_message stays None (the loop, which never ran, owns it).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-fail").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-fail";
        // insert_fake_connection drops the cmd receiver → send_prompt_inner fails.
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-fail")),
            EventEmitter::Noop,
        )
        .await;

        let result = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "never enqueued".into(),
                }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        assert!(result.is_err(), "a dropped receiver must fail the enqueue");

        let state = mgr.get_state(conn_id).await.unwrap();
        let snap = state.read().await;
        assert!(
            !snap.turn_in_flight,
            "a failed enqueue must roll back turn_in_flight so the connection isn't wedged"
        );
        let pending = snap.pending_user_message.clone();
        assert!(
            pending.is_none(),
            "a failed enqueue must not strand pending_user_message"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_skips_user_message_for_delegation_child() {
        // Delegation children surface their kickoff prompt via a separate path;
        // send_prompt_linked must NOT broadcast a user_message (or capture
        // pending) for them, so the sub-agent viewer doesn't double-render.
        use crate::acp::delegation::spawner::DelegationLink;
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-deleg").await;
        let parent =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent");
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-deleg";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/um-deleg")),
        )
        .await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "child kickoff".into(),
            }],
            Some(folder_id),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            }),
            None,
        )
        .await
        .expect("delegation kickoff enqueues");

        let command = cmd_rx.try_recv().expect("the kickoff prompt is enqueued");
        let ConnectionCommand::Prompt {
            user_message,
            bypass_autonomous_hold,
            ..
        } = command
        else {
            panic!("delegation kickoff must enqueue a Prompt command");
        };
        assert!(
            user_message.is_none(),
            "delegation child Prompt must carry NO user_message (kickoff is surfaced separately)"
        );
        assert!(
            bypass_autonomous_hold,
            "delegation kickoff must bypass a pre-kickoff autonomous hold"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "delegation kickoff must enqueue exactly one command"
        );
        let pending = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .pending_user_message
            .clone();
        assert!(
            pending.is_none(),
            "delegation child must not capture pending_user_message"
        );
    }

    /// Gen-1 durable fence pre-creates the child row, then adopts it on send
    /// via `conversation_id` + `DelegationLink`. Adoption must keep child
    /// semantics: no root user-message, no awaiting-reply, no mandatory-route
    /// scan of task text (same contract as create-path delegation children).
    #[tokio::test]
    async fn send_prompt_linked_prebound_child_preserves_delegation_semantics() {
        use crate::acp::delegation::spawner::DelegationLink;
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-prebound").await;
        let parent =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent");
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("prebound seed".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-prebound".into(),
                delegation_call_id: "call-prebound".into(),
            }),
        )
        .await
        .expect("prebound child");

        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-prebound";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/um-prebound")),
        )
        .await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                // Mentions that would install mandatory routes on a root prompt.
                text: "do work @profile-mandatory-route".into(),
            }],
            Some(folder_id),
            Some(child.id),
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-prebound".into(),
                delegation_call_id: "call-prebound".into(),
            }),
            None,
        )
        .await
        .expect("prebound adoption must accept link + conversation_id");

        // Drain full Prompt commands so we can assert mark_awaiting_reply.
        let mut prompts = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let crate::acp::connection::ConnectionCommand::Prompt {
                user_message,
                mark_awaiting_reply,
                ..
            } = cmd
            {
                prompts.push((user_message, mark_awaiting_reply));
            }
        }
        assert_eq!(prompts.len(), 1, "exactly one Prompt enqueued");
        let (user_message, mark_awaiting_reply) = &prompts[0];
        assert!(
            user_message.is_none(),
            "prebound delegation child must not attach user_message"
        );
        assert!(
            !*mark_awaiting_reply,
            "prebound delegation child must not mark awaiting-reply"
        );
        let pending = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .pending_user_message
            .clone();
        assert!(
            pending.is_none(),
            "prebound delegation child must not capture pending_user_message"
        );
        // Bound to the pre-created row, not a second create.
        assert_eq!(
            mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .conversation_id,
            Some(child.id)
        );
    }

    #[test]
    fn user_prompt_text_preview_joins_and_trims_text_blocks() {
        let blocks = vec![
            PromptInputBlock::Text {
                text: "  hello  ".into(),
            },
            PromptInputBlock::Text {
                text: "world".into(),
            },
        ];
        assert_eq!(
            user_prompt_text_preview(&blocks).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn user_prompt_text_preview_is_none_for_empty_or_textless() {
        assert!(user_prompt_text_preview(&[]).is_none());
        assert!(
            user_prompt_text_preview(&[PromptInputBlock::Text { text: "   ".into() }]).is_none()
        );
        let img = vec![PromptInputBlock::Image {
            data: "x".into(),
            mime_type: "image/png".into(),
            uri: None,
        }];
        assert!(user_prompt_text_preview(&img).is_none());
    }

    #[test]
    fn user_prompt_text_preview_truncates_long_input() {
        let long = "a".repeat(USER_PROMPT_PREVIEW_MAX_CHARS + 50);
        let preview = user_prompt_text_preview(&[PromptInputBlock::Text { text: long }]).unwrap();
        // truncate_str keeps MAX chars then appends a 3-char "..." marker.
        assert_eq!(preview.chars().count(), USER_PROMPT_PREVIEW_MAX_CHARS + 3);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn delegation_child_title_seed_uses_parser_title_from_first_prompt() {
        // The delegating prompt is a single text block (the task) — the seed must
        // equal what the parser's `title_from_user_text` produces from it, so a
        // later `refresh_auto_title` over the same first turn is a no-op.
        let task = "Review the auth module for race conditions";
        let blocks = vec![PromptInputBlock::Text { text: task.into() }];
        assert_eq!(
            delegation_child_title_seed(&blocks),
            Some(crate::parsers::title_from_user_text(task))
        );
    }

    #[test]
    fn delegation_child_title_seed_is_none_for_textless_prompt() {
        // Empty / whitespace / image-only prompts seed no title (stays NULL,
        // backfilled on first detail load as before).
        assert!(delegation_child_title_seed(&[]).is_none());
        assert!(delegation_child_title_seed(&[PromptInputBlock::Text {
            text: "  \n ".into()
        }])
        .is_none());
        let img = vec![PromptInputBlock::Image {
            data: "x".into(),
            mime_type: "image/png".into(),
            uri: None,
        }];
        assert!(delegation_child_title_seed(&img).is_none());
    }

    #[test]
    fn delegation_child_title_seed_caps_long_task_text() {
        // Mirrors the parser cap (100 chars) so an over-long task doesn't store a
        // runaway title; `title_from_user_text` keeps 100 then appends "...".
        let long = "x".repeat(250);
        let seed = delegation_child_title_seed(&[PromptInputBlock::Text { text: long }]).unwrap();
        assert_eq!(seed.chars().count(), 103);
        assert!(seed.ends_with("..."));
    }

    /// A successful UI send (delegation = None, text present) emits
    /// `UserPromptSent` carrying the message preview, after the link + status
    /// events.
    #[tokio::test]
    async fn send_prompt_linked_emits_user_prompt_sent_on_success() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/ups").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-ups-1";
        let _rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/ups")),
        )
        .await;
        let mut stream = subscribe_conn_stream(&mgr, conn_id).await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "hello world".into(),
            }],
            Some(folder_id),
            None,
            None,
            None,
        )
        .await
        .expect("send should succeed with a live receiver");

        let mut found = None;
        for _ in 0..5 {
            let env = recv_first_acp_event(&mut stream).await;
            if let AcpEvent::UserPromptSent { text_preview } = env.payload {
                found = Some(text_preview);
                break;
            }
        }
        assert_eq!(found.as_deref(), Some("hello world"));
    }

    /// A textless prompt (image-only) succeeds but emits NO `UserPromptSent` —
    /// the notification fires for text messages only.
    #[tokio::test]
    async fn send_prompt_linked_skips_user_prompt_sent_for_textless_prompt() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/ups2").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-ups-2";
        let _rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/ups2")),
        )
        .await;
        let mut stream = subscribe_conn_stream(&mgr, conn_id).await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Image {
                data: "deadbeef".into(),
                mime_type: "image/png".into(),
                uri: None,
            }],
            Some(folder_id),
            None,
            None,
            None,
        )
        .await
        .expect("send should succeed with a live receiver");

        let mut saw_user_prompt = false;
        for _ in 0..4 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), stream.recv()).await {
                Ok(Ok(env)) => {
                    if matches!(env.payload, AcpEvent::UserPromptSent { .. }) {
                        saw_user_prompt = true;
                    }
                }
                _ => break,
            }
        }
        assert!(
            !saw_user_prompt,
            "a textless (image-only) prompt must not emit UserPromptSent"
        );
    }

    #[tokio::test]
    async fn get_state_returns_arc_for_known_connection() {
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), fake_connection("c1", None));
        }
        let state = mgr.get_state("c1").await.expect("state should be found");
        assert_eq!(state.read().await.connection_id, "c1");
    }

    #[tokio::test]
    async fn get_state_returns_none_for_unknown_connection() {
        let mgr = ConnectionManager::new();
        assert!(mgr.get_state("does-not-exist").await.is_none());
    }

    #[tokio::test]
    async fn find_connection_by_conversation_id_matches_when_bound() {
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), fake_connection("c1", Some(42)));
            map.insert("c2".to_string(), fake_connection("c2", None));
        }
        let found = mgr
            .find_connection_by_conversation_id(42)
            .await
            .expect("should find c1");
        assert_eq!(found, "c1");
        assert!(mgr.find_connection_by_conversation_id(999).await.is_none());
    }

    #[tokio::test]
    async fn find_all_connections_for_identity_includes_both_insert_orders() {
        use crate::web::event_bridge::EventEmitter;

        async fn collect(order: &[&str]) -> Vec<String> {
            let mgr = ConnectionManager::new();
            for id in order {
                mgr.insert_test_connection(id, AgentType::ClaudeCode, None, EventEmitter::Noop)
                    .await;
                let state = mgr.get_state(id).await.unwrap();
                let mut s = state.write().await;
                s.conversation_id = Some(7);
            }
            let mut ids = mgr
                .find_all_connections_for_conversation_identity(7, None, AgentType::ClaudeCode)
                .await;
            ids.sort();
            ids
        }

        assert_eq!(
            collect(&["parent-a", "parent-b"]).await,
            vec!["parent-a".to_string(), "parent-b".to_string()]
        );
        assert_eq!(
            collect(&["parent-b", "parent-a"]).await,
            vec!["parent-a".to_string(), "parent-b".to_string()]
        );
    }

    #[tokio::test]
    async fn find_all_connections_for_identity_matches_external_id_only() {
        use crate::web::event_bridge::EventEmitter;

        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("ext-only", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        {
            let state = mgr.get_state("ext-only").await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = None;
            s.external_id = Some("session-ext".into());
            s.agent_type = AgentType::Codex;
        }
        // Unrelated conversation id should still find via external binding.
        let ids = mgr
            .find_all_connections_for_conversation_identity(
                99,
                Some("session-ext"),
                AgentType::Codex,
            )
            .await;
        assert_eq!(ids, vec!["ext-only".to_string()]);

        // Wrong agent_type must not match external-only.
        let none = mgr
            .find_all_connections_for_conversation_identity(
                99,
                Some("session-ext"),
                AgentType::ClaudeCode,
            )
            .await;
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn send_prompt_linked_creates_conversation_on_first_call_only() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test").await;

        let mgr = ConnectionManager::new();
        let conn_id = "c1";
        {
            let mut map = mgr.connections.lock().await;
            // Note: cmd_tx receiver is dropped, so send_prompt's mpsc.send will fail
            // with ProcessExited. That's fine — we only verify the linkage side
            // effect, not the actual prompt forwarding.
            map.insert(conn_id.into(), fake_connection(conn_id, None));
        }

        // First call: creates conversation row, sets state.conversation_id.
        // The mpsc send error after linking is expected and ignored here.
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        let snap = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .to_snapshot();
        assert!(
            snap.conversation_id.is_some(),
            "conversation_id should be set"
        );
        assert_eq!(snap.folder_id, Some(folder_id));
        let first_id = snap.conversation_id.unwrap();

        // Second call: ignores folder_id, does NOT create another row.
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        let snap2 = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .to_snapshot();
        assert_eq!(snap2.conversation_id, Some(first_id));
    }

    #[tokio::test]
    async fn send_prompt_linked_errors_when_no_folder_id() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let conn_id = "c1";
        {
            let mut map = mgr.connections.lock().await;
            map.insert(conn_id.into(), fake_connection(conn_id, None));
        }
        let result = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), None, None, None, None)
            .await;
        assert!(
            result.is_err(),
            "should error when folder_id is not provided for a new conversation row"
        );
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("folder_id"),
            "error should mention missing folder_id, got: {err_str}"
        );
    }

    /// Count of `conversation` rows (ignoring soft-delete) — used by the
    /// caller-supplied conversation_id tests to assert no new row was created.
    async fn count_conversation_rows(db: &crate::db::AppDatabase) -> usize {
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        conversation::Entity::find()
            .all(&db.conn)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn send_prompt_linked_uses_caller_conversation_id_when_provided() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/caller-id").await;
        // Pre-create a conversation row the caller will reference.
        let pre_existing =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-caller-id";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/caller-id")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        // Count rows before
        let before = count_conversation_rows(&db).await;

        // Send with caller-supplied conversation_id + folder_id.
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(pre_existing.id),
                None,
                None,
            )
            .await;

        // No new conversation row was created.
        let after = count_conversation_rows(&db).await;
        assert_eq!(after, before, "no new row should be created");

        // State now has the caller-supplied conversation_id.
        let state = mgr.get_state(conn_id).await.unwrap();
        assert_eq!(state.read().await.conversation_id, Some(pre_existing.id));

        // ConversationLinked event was emitted with the caller's id.
        let env = recv_first_acp_event(&mut rx).await;
        match env.payload {
            AcpEvent::ConversationLinked {
                conversation_id,
                folder_id: emitted_folder,
                ..
            } => {
                assert_eq!(conversation_id, pre_existing.id);
                assert_eq!(emitted_folder, folder_id);
            }
            other => panic!("expected ConversationLinked, got {other:?}"),
        }
    }

    /// Drain the global broadcaster and report whether a `conversation://changed`
    /// upsert for `id` carrying `external_id` was emitted.
    fn drain_has_upsert_with_external_id(
        rx: &mut broadcast::Receiver<WebEvent>,
        id: i32,
        external_id: &str,
    ) -> bool {
        while let Ok(evt) = rx.try_recv() {
            if evt.channel != crate::web::event_bridge::CONVERSATION_CHANGED_EVENT {
                continue;
            }
            let p = &*evt.payload;
            if p["kind"] == "upsert"
                && p["summary"]["id"] == id
                && p["summary"]["external_id"] == external_id
            {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn send_prompt_linked_session_started_before_link_broadcasts_external_id_branch_b() {
        // SessionStarted-before-link: external_id is already on the live state
        // but no conversation_id yet, so the lifecycle subscriber skipped its
        // broadcast. The synchronous external_id persist inside
        // send_prompt_linked (backend-create Branch B) must itself emit a
        // corrective `conversation://changed` upsert so other clients converge.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/sess-pre-b").await;
        let mgr = ConnectionManager::new();
        let (broadcaster, mut rx) = make_test_broadcaster();
        let conn_id = "conn-sess-pre-b";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/sess-pre-b")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.external_id = Some("ext-pre".to_string());
        }

        // cmd_tx receiver is dropped → the prompt send fails after linking, but
        // the link + external_id persist + broadcast already happened.
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;

        let cid = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .conversation_id
            .expect("conversation should be linked");
        let row = conversation_service::get_by_id(&db.conn, cid)
            .await
            .unwrap();
        assert_eq!(row.external_id.as_deref(), Some("ext-pre"));
        assert!(
            drain_has_upsert_with_external_id(&mut rx, cid, "ext-pre"),
            "Branch B must broadcast a conversation://changed upsert carrying external_id"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_session_started_before_link_broadcasts_external_id_branch_a() {
        // Same precondition, caller-supplied conversation_id (adopt Branch A).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/sess-pre-a").await;
        let pre =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let mgr = ConnectionManager::new();
        let (broadcaster, mut rx) = make_test_broadcaster();
        let conn_id = "conn-sess-pre-a";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/sess-pre-a")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.external_id = Some("ext-pre-a".to_string());
        }

        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(pre.id),
                None,
                None,
            )
            .await;

        let row = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(row.external_id.as_deref(), Some("ext-pre-a"));
        assert!(
            drain_has_upsert_with_external_id(&mut rx, pre.id, "ext-pre-a"),
            "Branch A must broadcast a conversation://changed upsert carrying external_id"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_rejects_conversation_id_without_folder_id() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-bad-args";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/x")),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;

        let err = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), None, Some(42), None, None)
            .await
            .expect_err("should reject conversation_id without folder_id");
        assert!(matches!(err, AcpError::Protocol(_)));
    }

    #[tokio::test]
    async fn send_prompt_linked_caller_id_is_noop_when_already_linked() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/already").await;
        let pre =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-already";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/already")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        // Pre-link the connection state.
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.conversation_id = Some(pre.id);
        }
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        let before = count_conversation_rows(&db).await;
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(pre.id),
                None,
                None,
            )
            .await;
        let after = count_conversation_rows(&db).await;
        assert_eq!(after, before);

        // No ConversationLinked event was emitted (already linked). The
        // centralized status transition fires InProgress; then because the
        // dropped cmd_tx receiver makes `send_prompt_inner` return
        // ProcessExited, the rollback path fires Cancelled. Two events,
        // strictly ordered.
        let env_in_progress = recv_first_acp_event(&mut rx).await;
        match env_in_progress.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, pre.id);
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => {
                panic!("first event must be ConversationStatusChanged(InProgress), got {other:?}")
            }
        }
        let env_cancelled = recv_first_acp_event(&mut rx).await;
        match env_cancelled.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, pre.id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => panic!(
                "second event must be ConversationStatusChanged(Cancelled) after send failure, got {other:?}"
            ),
        }
    }

    // ---------- Phase: status centralization ----------

    #[tokio::test]
    async fn send_prompt_linked_writes_in_progress_and_emits_event() {
        use crate::db::entities::conversation;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/status").await;

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-status-1";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/status")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        // First call: backend creates the conversation row and links it.
        // The cmd_tx receiver in `insert_fake_connection` has been dropped,
        // so `send_prompt_inner` returns ProcessExited — exercising the new
        // Cancelled-rollback path. We expect THREE events in order:
        //   1. ConversationLinked
        //   2. ConversationStatusChanged(InProgress)  [pre-send write]
        //   3. ConversationStatusChanged(Cancelled)   [rollback after send failure]
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;

        let env1 = recv_first_acp_event(&mut rx).await;
        let conv_id = match env1.payload {
            AcpEvent::ConversationLinked {
                conversation_id,
                folder_id: emitted_folder,
                ..
            } => {
                assert_eq!(emitted_folder, folder_id);
                conversation_id
            }
            other => panic!("first event must be ConversationLinked, got {other:?}"),
        };
        let env2 = recv_first_acp_event(&mut rx).await;
        match env2.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => {
                panic!("second event must be ConversationStatusChanged(InProgress), got {other:?}")
            }
        }
        let env3 = recv_first_acp_event(&mut rx).await;
        match env3.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => panic!(
                "third event must be ConversationStatusChanged(Cancelled) on send failure, got {other:?}"
            ),
        }
        // Ordering invariant: ConversationLinked < InProgress < Cancelled.
        assert!(
            env2.seq > env1.seq && env3.seq > env2.seq,
            "event seqs must be strictly monotonic: linked={} in_progress={} cancelled={}",
            env1.seq,
            env2.seq,
            env3.seq
        );

        // DB row settles at Cancelled (the rollback after send failure). The
        // intermediate InProgress write is observable only via the event,
        // not by the time the test reads the row.
        let row = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists");
        assert_eq!(row.status, ConversationStatus::Cancelled);

        // Second send: already-linked path also writes + emits InProgress
        // and then Cancelled (same send-failure rollback). Pre-flip the row
        // to PendingReview to observe the transition flip forward — mirrors
        // the "follow-up turn after a TurnComplete" scenario.
        conversation_service::update_status(&db.conn, conv_id, ConversationStatus::PendingReview)
            .await
            .unwrap();

        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;

        let env4 = recv_first_acp_event(&mut rx).await;
        match env4.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => panic!(
                "second send must re-emit ConversationStatusChanged(InProgress) first, got {other:?}"
            ),
        }
        let env5 = recv_first_acp_event(&mut rx).await;
        match env5.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => {
                panic!("second send must rollback to Cancelled after send failure, got {other:?}")
            }
        }
        let row2 = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row2.status, ConversationStatus::Cancelled);
    }

    // ---------- Phase: connection dedup ----------

    #[tokio::test]
    async fn find_connection_for_reuse_returns_none_when_session_id_is_none() {
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        // Insert a connection that *would* match if session_id were Some.
        let id = "c1";
        insert_fake_connection(
            &mgr,
            id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/reuse")),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;
        {
            let state = mgr.get_state(id).await.unwrap();
            state.write().await.external_id = Some("ext-1".into());
        }
        let found = mgr
            .find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&PathBuf::from("/tmp/reuse")),
                None,
            )
            .await;
        assert!(
            found.is_none(),
            "no session_id means we never dedup speculative connects"
        );
    }

    #[tokio::test]
    async fn spawn_agent_reuses_existing_connection_when_session_id_matches() {
        // Direct unit test for the lookup helper that spawn_agent calls
        // before its (process-spawning) block. We test the helper directly so
        // the test never tries to launch an agent process.
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let existing_id = "preexisting-conn";
        let working_dir = PathBuf::from("/tmp/reuse-match");
        insert_fake_connection(
            &mgr,
            existing_id,
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state(existing_id).await.unwrap();
            let mut s = state.write().await;
            s.external_id = Some("ext-1".into());
            s.status = ConnectionStatus::Connected;
        }

        // Same session_id + same agent + same working_dir -> reuse.
        let found = mgr
            .find_connection_for_reuse(AgentType::ClaudeCode, Some(&working_dir), Some("ext-1"))
            .await;
        assert_eq!(found.as_deref(), Some(existing_id));

        // Different session_id -> no reuse.
        assert!(mgr
            .find_connection_for_reuse(AgentType::ClaudeCode, Some(&working_dir), Some("other-ext"))
            .await
            .is_none());

        // Different working_dir -> no reuse.
        assert!(mgr
            .find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&PathBuf::from("/tmp/different")),
                Some("ext-1")
            )
            .await
            .is_none());

        // Different agent_type -> no reuse.
        assert!(mgr
            .find_connection_for_reuse(AgentType::Codex, Some(&working_dir), Some("ext-1"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn reuse_bypasses_unavailable_new_shell() {
        use crate::acp::terminal_context::AcpLaunchInputs;
        use crate::models::SystemTerminalSettings;
        use crate::terminal::shell::test_support::{pwsh_spec as test_pwsh_spec, snapshot};

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let existing_id = "reuse-shell-conn";
        let working_dir = PathBuf::from("/tmp/reuse-shell");
        insert_fake_connection(
            &mgr,
            existing_id,
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;
        let original_snapshot = snapshot("pwsh.exe", test_pwsh_spec());
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut(existing_id).unwrap();
            conn.terminal_shell = original_snapshot.clone();
            let mut s = conn.state.write().await;
            s.external_id = Some("ext-shell".into());
            s.status = ConnectionStatus::Connected;
        }

        let inputs = AcpLaunchInputs::with_placeholder_route(
            BTreeMap::new(),
            SystemTerminalSettings {
                default_shell: Some("missing-shell".into()),
            },
        );
        let id = mgr
            .spawn_agent(
                AgentType::ClaudeCode,
                Some(working_dir.to_string_lossy().into_owned()),
                Some("ext-shell".into()),
                inputs,
                "test-window".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                None,
                None,
            )
            .await
            .expect("reuse must succeed even when new shell is unavailable");
        assert_eq!(id, existing_id);
        let stored = {
            let map = mgr.connections.lock().await;
            map.get(existing_id).unwrap().terminal_shell.clone()
        };
        assert_eq!(stored, original_snapshot);
    }

    #[tokio::test]
    async fn new_connection_rejects_unavailable_shell() {
        use crate::acp::terminal_context::AcpLaunchInputs;
        use crate::models::SystemTerminalSettings;

        let mgr = ConnectionManager::new();
        let inputs = AcpLaunchInputs::with_placeholder_route(
            BTreeMap::new(),
            SystemTerminalSettings {
                default_shell: Some("missing-shell".into()),
            },
        );
        let err = mgr
            .spawn_agent(
                AgentType::ClaudeCode,
                Some("/tmp/new-shell".into()),
                None,
                inputs,
                "test-window".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                None,
                None,
            )
            .await
            .expect_err("unavailable shell must fail before process spawn");
        assert!(
            matches!(err, AcpError::TerminalShellUnavailable { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn changing_settings_does_not_mutate_running_snapshot() {
        use crate::acp::terminal_context::{finalize_acp_launch_config, AcpLaunchInputs};
        use crate::models::SystemTerminalSettings;
        use crate::terminal::shell::ResolvedShellSnapshot;

        fn make_usable_shell(dir: &std::path::Path, basename: &str) -> PathBuf {
            let path = dir.join(basename);
            std::fs::write(&path, b"").expect("write temp shell");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&path, perms).unwrap();
            }
            path
        }

        let dir = tempfile::tempdir().unwrap();
        let (name_a, name_b) = if cfg!(windows) {
            ("pwsh.exe", "cmd.exe")
        } else {
            ("bash", "zsh")
        };
        let path_a = make_usable_shell(dir.path(), name_a);
        let path_b = make_usable_shell(dir.path(), name_b);

        let snap_a: ResolvedShellSnapshot = finalize_acp_launch_config(
            AcpLaunchInputs::with_placeholder_route(
                BTreeMap::new(),
                SystemTerminalSettings {
                    default_shell: Some(path_a.to_string_lossy().into_owned()),
                },
            ),
            AgentType::ClaudeCode,
        )
        .expect("shell a")
        .terminal_shell;
        let snap_b: ResolvedShellSnapshot = finalize_acp_launch_config(
            AcpLaunchInputs::with_placeholder_route(
                BTreeMap::new(),
                SystemTerminalSettings {
                    default_shell: Some(path_b.to_string_lossy().into_owned()),
                },
            ),
            AgentType::ClaudeCode,
        )
        .expect("shell b")
        .terminal_shell;
        assert_ne!(snap_a, snap_b);

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let existing_id = "snap-immutable";
        let working_dir = PathBuf::from("/tmp/snap-immutable");
        insert_fake_connection(
            &mgr,
            existing_id,
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;
        {
            let mut map = mgr.connections.lock().await;
            let conn = map.get_mut(existing_id).unwrap();
            conn.terminal_shell = snap_a.clone();
            let mut s = conn.state.write().await;
            s.external_id = Some("ext-snap".into());
            s.status = ConnectionStatus::Connected;
        }

        // Reuse with settings that would resolve to snap_b — must keep snap_a.
        let id = mgr
            .spawn_agent(
                AgentType::ClaudeCode,
                Some(working_dir.to_string_lossy().into_owned()),
                Some("ext-snap".into()),
                AcpLaunchInputs::with_placeholder_route(
                    BTreeMap::new(),
                    SystemTerminalSettings {
                        default_shell: Some(path_b.to_string_lossy().into_owned()),
                    },
                ),
                "test-window".into(),
                EventEmitter::Noop,
                None,
                BTreeMap::new(),
                ConnectionLaunchContext::default(),
                None,
                None,
            )
            .await
            .expect("reuse");
        assert_eq!(id, existing_id);
        let stored = {
            let map = mgr.connections.lock().await;
            map.get(existing_id).unwrap().terminal_shell.clone()
        };
        assert_eq!(stored, snap_a);
        assert_ne!(stored, snap_b);
    }

    #[tokio::test]
    async fn find_connection_for_reuse_skips_disconnected_or_errored() {
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let working_dir = PathBuf::from("/tmp/torn-down");
        insert_fake_connection(
            &mgr,
            "torn",
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state("torn").await.unwrap();
            let mut s = state.write().await;
            s.external_id = Some("ext-1".into());
            s.status = ConnectionStatus::Disconnected;
        }
        assert!(
            mgr.find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&working_dir),
                Some("ext-1"),
            )
            .await
            .is_none(),
            "Disconnected connection must not be reused"
        );

        // Flip to Error — also excluded.
        {
            let state = mgr.get_state("torn").await.unwrap();
            state.write().await.status = ConnectionStatus::Error;
        }
        assert!(
            mgr.find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&working_dir),
                Some("ext-1"),
            )
            .await
            .is_none(),
            "Errored connection must not be reused"
        );
    }

    /// Helper that backdates a connection's `last_activity_at` so the
    /// idle sweep sees it as having crossed its threshold.
    async fn backdate_last_activity(mgr: &ConnectionManager, conn_id: &str, secs_ago: i64) {
        let state = mgr.get_state(conn_id).await.expect("connection exists");
        let mut s = state.write().await;
        s.last_activity_at = chrono::Utc::now() - chrono::Duration::seconds(secs_ago);
    }

    #[tokio::test]
    async fn sweep_idle_disconnects_idle_connected_connections() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "stale",
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/stale")),
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "stale", 600).await;

        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 1);
        assert!(
            mgr.connections.lock().await.get("stale").is_none(),
            "Idle connection must be removed after sweep"
        );
    }

    #[tokio::test]
    async fn sweep_idle_skips_recently_active_connection() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "fresh",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // last_activity_at defaults to "now" inside SessionState::new — no
        // backdating, so it should NOT be swept.
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 0);
        assert!(mgr.connections.lock().await.contains_key("fresh"));
    }

    #[tokio::test]
    async fn sweep_idle_skips_prompting_connection() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "prompting",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "prompting", 600).await;
        // Override status to Prompting — a turn is in flight; never sweep.
        {
            let state = mgr.get_state("prompting").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 0);
        assert!(mgr.connections.lock().await.contains_key("prompting"));
    }

    #[tokio::test]
    async fn sweep_idle_skips_pending_permission() {
        use crate::acp::session_state::PendingPermissionState;
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "permission",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "permission", 600).await;
        {
            let state = mgr.get_state("permission").await.unwrap();
            state.write().await.pending_permission = Some(PendingPermissionState {
                request_id: "req-1".into(),
                tool_call_id: "tc-1".into(),
                tool_call: serde_json::json!({ "toolCallId": "tc-1", "title": "test" }),
                options: vec![],
                created_at: chrono::Utc::now(),
                queued: 0,
            });
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(
            n, 0,
            "Connection with pending permission must not be swept (user is mid-decision)"
        );
        assert!(mgr.connections.lock().await.contains_key("permission"));
    }

    #[tokio::test]
    async fn sweep_idle_skips_active_background_work() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "background",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "background", 600).await;
        {
            let state = mgr.get_state("background").await.unwrap();
            let mut state = state.write().await;
            // Mirror what apply_event(BackgroundActivity) records: pending
            // work plus a recent watcher heartbeat.
            state.background_outstanding = 1;
            state.background_activity_at = Some(chrono::Utc::now());
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(
            n, 0,
            "Connection with unresolved background work must not be swept \
             (disconnecting kills the agent CLI and the background task with it)"
        );
        assert!(mgr.connections.lock().await.contains_key("background"));

        // Once the watcher settles the work (outstanding back to 0), the same
        // connection becomes sweepable again.
        {
            let state = mgr.get_state("background").await.unwrap();
            let mut state = state.write().await;
            state.background_outstanding = 0;
            state.last_activity_at = chrono::Utc::now() - chrono::Duration::seconds(600);
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 1, "settled background work no longer exempts the sweep");
    }

    #[tokio::test]
    async fn sweep_idle_picks_only_qualifying_subset() {
        let mgr = ConnectionManager::new();
        for id in ["a", "b", "c"] {
            insert_fake_connection(&mgr, id, AgentType::ClaudeCode, None, EventEmitter::Noop).await;
        }
        // a: idle (sweep target), b: fresh (not idle), c: idle but Prompting (skipped).
        backdate_last_activity(&mgr, "a", 600).await;
        backdate_last_activity(&mgr, "c", 600).await;
        {
            let state = mgr.get_state("c").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 1);
        let map = mgr.connections.lock().await;
        assert!(!map.contains_key("a"));
        assert!(map.contains_key("b"));
        assert!(map.contains_key("c"));
    }

    /// When two `spawn_agent` calls race for the same logical session id,
    /// the per-key dedup mutex makes the second one observe the first's
    /// freshly-spawned connection and reuse it. Without the mutex, both
    /// would have missed dedup during the connecting window.
    ///
    /// Simulates the race by pre-inserting a "first call's connection" with
    /// `external_id` set; what's tested is that two concurrent
    /// `find_connection_for_reuse` calls under the same lock see consistent
    /// state. The `spawn_locks` map being shared via `clone_ref` is the
    /// invariant we need.
    #[tokio::test]
    async fn spawn_locks_are_shared_across_clone_ref() {
        let mgr = ConnectionManager::new();
        let cloned = mgr.clone_ref();
        // Both clones must reference the same map. Insert via one,
        // observe via the other.
        let key = SpawnDedupKey {
            agent_type: AgentType::ClaudeCode,
            working_dir: Some(PathBuf::from("/tmp/dedup-test")),
            session_id: "ext-shared".into(),
        };
        {
            let mut locks = mgr.spawn_locks.lock().await;
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
        }
        let cloned_locks = cloned.spawn_locks.lock().await;
        assert!(
            cloned_locks.contains_key(&key),
            "spawn_locks must be shared between original and clone_ref"
        );
    }

    #[tokio::test]
    async fn connection_manager_clones_share_continuation_store() {
        use crate::acp::delegation::continuation::store::{
            ContinuationStore, InMemoryContinuationStore,
        };

        let manager = ConnectionManager::new();
        let store = Arc::new(InMemoryContinuationStore::default()) as Arc<dyn ContinuationStore>;
        manager.install_continuation_store(store);
        let cloned = manager.clone_ref();

        assert!(Arc::ptr_eq(
            &manager.continuation_store().expect("store installed"),
            &cloned
                .continuation_store()
                .expect("shared store visible to clone"),
        ));
    }

    /// Two concurrent `send_prompt_linked` calls on the SAME connection
    /// must serialize through the per-connection `prompt_lock` so the
    /// backend-creates branch can't fire twice and produce duplicate
    /// conversation rows. The second call observes `already_linked == true`
    /// (set by the first under the lock) and skips creation.
    #[tokio::test]
    async fn send_prompt_linked_serializes_concurrent_callers() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/race").await;

        let mgr = Arc::new(ConnectionManager::new());
        let conn_id = "race-conn";
        {
            let mut map = mgr.connections.lock().await;
            map.insert(conn_id.into(), fake_connection(conn_id, None));
        }

        let before = count_conversation_rows(&db).await;
        // tokio::join! polls the two futures concurrently in the SAME
        // task — they can borrow `&db` and `mgr` without the 'static
        // requirement that `tokio::spawn` would impose.
        let mgr_ref = mgr.as_ref();
        tokio::join!(
            async {
                let _ = mgr_ref
                    .send_prompt_linked(
                        &db,
                        conn_id,
                        one_text_block(),
                        Some(folder_id),
                        None,
                        None,
                        None,
                    )
                    .await;
            },
            async {
                let _ = mgr_ref
                    .send_prompt_linked(
                        &db,
                        conn_id,
                        one_text_block(),
                        Some(folder_id),
                        None,
                        None,
                        None,
                    )
                    .await;
            },
        );

        let after = count_conversation_rows(&db).await;
        assert_eq!(
            after - before,
            1,
            "exactly one new conversation row across two concurrent send_prompt_linked"
        );
    }

    // ---------- Phase: spawn handshake wait helper ----------

    #[tokio::test]
    async fn wait_for_session_started_returns_ready_when_sender_fires() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Fire immediately on a separate task so the wait future actually
        // gets to register.
        tokio::spawn(async move {
            let _ = tx.send(());
        });
        let (outcome, elapsed) = wait_for_session_started(rx, Duration::from_millis(500)).await;
        assert_eq!(outcome, HandshakeWaitOutcome::Ready);
        assert!(
            elapsed < Duration::from_millis(500),
            "Ready outcome must resolve well before timeout, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn wait_for_session_started_returns_aborted_when_sender_drops() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Drop the sender — emulates "connection died before SessionStarted",
        // i.e. SessionState's tx was dropped during cleanup.
        drop(tx);
        let (outcome, elapsed) = wait_for_session_started(rx, Duration::from_millis(500)).await;
        assert_eq!(outcome, HandshakeWaitOutcome::Aborted);
        assert!(
            elapsed < Duration::from_millis(500),
            "Aborted outcome must resolve well before timeout, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn wait_for_session_started_returns_timed_out_when_neither_happens() {
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Hold the sender alive but never fire and never drop. Tight
        // timeout so the test stays fast; production timeout is 60s.
        let (outcome, elapsed) = wait_for_session_started(rx, Duration::from_millis(40)).await;
        assert_eq!(outcome, HandshakeWaitOutcome::TimedOut);
        assert!(
            elapsed >= Duration::from_millis(40),
            "TimedOut must wait at least the full timeout, got {elapsed:?}"
        );
    }

    #[test]
    fn spawn_handshake_timeout_from_env_uses_default_when_unset() {
        // Snapshot env, mutate, restore. Single test owns this var to avoid
        // cross-test contention.
        let prev = std::env::var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS").ok();
        std::env::remove_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS");
        let default = spawn_handshake_timeout_from_env();
        assert_eq!(default, Duration::from_secs(SPAWN_HANDSHAKE_TIMEOUT_SECS));

        std::env::set_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS", "5");
        assert_eq!(spawn_handshake_timeout_from_env(), Duration::from_secs(5));

        std::env::set_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS", "garbage");
        assert_eq!(
            spawn_handshake_timeout_from_env(),
            Duration::from_secs(SPAWN_HANDSHAKE_TIMEOUT_SECS),
            "invalid value falls back to default"
        );

        // Restore.
        match prev {
            Some(v) => std::env::set_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS", v),
            None => std::env::remove_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS"),
        }
    }

    /// Successful status owners emit exactly one authoritative `state` patch
    /// on conversation://changed (no legacy `status` bridge, no duplicates).
    #[tokio::test]
    async fn cancel_emits_exactly_one_state_event_with_backend_patch() {
        use crate::db::entities::conversation;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/cancel-state-event").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        assert_eq!(conv.status, ConversationStatus::InProgress);

        let mgr = ConnectionManager::new();
        let (broadcaster, mut global_rx) = make_test_broadcaster();
        let conn_id = "conn-cancel-state";
        // Keep cmd_rx alive so Cancel enqueues; a dropped receiver fails before
        // the status CAS.
        let _cmd_rx = mgr
            .insert_test_connection_live(
                conn_id,
                AgentType::ClaudeCode,
                Some(PathBuf::from("/tmp/cancel-state-event")),
                EventEmitter::test_web_only(broadcaster.clone()),
            )
            .await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.conversation_id = Some(conv.id);
        }
        let mut acp_rx = subscribe_conn_stream(&mgr, conn_id).await;

        mgr.cancel(&db.conn, conn_id).await.expect("cancel");

        // Per-connection status event first.
        let env = recv_first_acp_event(&mut acp_rx).await;
        match env.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv.id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => panic!("expected ConversationStatusChanged(Cancelled), got {other:?}"),
        }

        // Exactly one global state patch, values match the backend row.
        let row = conversation::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut state_events = Vec::new();
        while let Ok(evt) = global_rx.try_recv() {
            if evt.channel == crate::web::event_bridge::CONVERSATION_CHANGED_EVENT {
                state_events.push(evt);
            }
        }
        assert_eq!(
            state_events.len(),
            1,
            "cancel must emit exactly one conversation://changed event"
        );
        let p = &*state_events[0].payload;
        assert_eq!(p["kind"], "state");
        assert_eq!(p["patch"]["id"], conv.id);
        assert_eq!(p["patch"]["status"], "cancelled");
        assert!(p["patch"]["awaiting_reply_token"].is_null());
        assert_eq!(
            p["patch"]["updated_at"],
            serde_json::to_value(row.updated_at).unwrap()
        );
    }

    #[tokio::test]
    async fn send_prompt_status_owners_emit_one_state_patch_each() {
        use crate::db::entities::conversation;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/prompt-state-event").await;
        let mgr = ConnectionManager::new();
        let (broadcaster, mut global_rx) = make_test_broadcaster();
        let conn_id = "conn-prompt-state";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/prompt-state-event")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;

        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "trigger send failure".into(),
                }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;

        let mut state_events = Vec::new();
        while let Ok(evt) = global_rx.try_recv() {
            if evt.channel != crate::web::event_bridge::CONVERSATION_CHANGED_EVENT {
                continue;
            }
            let p = &*evt.payload;
            if p["kind"] == "state" {
                state_events.push(p.clone());
            }
        }
        // InProgress (prompt start) + Cancelled (send rollback) — no legacy status,
        // and no duplicate of either.
        assert_eq!(
            state_events.len(),
            2,
            "expected one state patch per successful status write, got {state_events:?}"
        );
        assert_eq!(state_events[0]["patch"]["status"], "in_progress");
        assert_eq!(state_events[1]["patch"]["status"], "cancelled");
        let conv_id = state_events[1]["patch"]["id"].as_i64().unwrap() as i32;
        let row = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, ConversationStatus::Cancelled);
        assert_eq!(
            state_events[1]["patch"]["updated_at"],
            serde_json::to_value(row.updated_at).unwrap()
        );
    }

    /// When `send_prompt_inner` fails (process gone, channel closed) the row
    /// must end up `Cancelled`, NOT stuck on `in_progress`. Without this
    /// rollback the lifecycle subscriber's TurnComplete write never fires
    /// (no turn ever started), so the only thing that could later un-stick
    /// the row is a follow-up prompt happening to succeed — fragile, and on
    /// the server-side / chat-channel paths there may be no follow-up at all.
    #[tokio::test]
    async fn send_prompt_linked_rolls_back_to_cancelled_on_send_failure() {
        use crate::db::entities::conversation;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/cancel-rollback").await;

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-cancel";
        // insert_fake_connection drops the cmd_tx receiver, so send_prompt_inner
        // returns ProcessExited — exactly the failure mode this test targets.
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/cancel-rollback")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        // Non-empty blocks so the send reaches `reserve()` (which fails on the
        // dropped receiver → ProcessExited); an empty prompt would be rejected
        // earlier, before the gate, and never exercise this rollback path.
        let result = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "trigger send failure".into(),
                }],
                Some(folder_id),
                None,
                None,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(AcpError::ProcessExited)),
            "send_prompt_inner must propagate ProcessExited up to the caller; got {result:?}"
        );

        // Drain events: ConversationLinked → InProgress → Cancelled, in order.
        let env_linked = recv_first_acp_event(&mut rx).await;
        let conv_id = match env_linked.payload {
            AcpEvent::ConversationLinked {
                conversation_id, ..
            } => conversation_id,
            other => panic!("expected ConversationLinked first, got {other:?}"),
        };
        let env_in_progress = recv_first_acp_event(&mut rx).await;
        match env_in_progress.payload {
            AcpEvent::ConversationStatusChanged { status, .. } => {
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => {
                panic!("expected ConversationStatusChanged(InProgress) before send, got {other:?}")
            }
        }
        let env_cancelled = recv_first_acp_event(&mut rx).await;
        match env_cancelled.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(
                    status,
                    ConversationStatus::Cancelled,
                    "send_prompt failure must roll the row forward to Cancelled, not leave InProgress"
                );
            }
            other => panic!(
                "expected ConversationStatusChanged(Cancelled) on send failure, got {other:?}"
            ),
        }

        // Strict ordering: linked < in_progress < cancelled. The lifecycle
        // contract says the Cancelled emit cannot precede the InProgress one
        // — UIs that animate based on "previous → current" depend on this.
        assert!(
            env_in_progress.seq > env_linked.seq && env_cancelled.seq > env_in_progress.seq,
            "event seq must be strictly monotonic: linked={} in_progress={} cancelled={}",
            env_linked.seq,
            env_in_progress.seq,
            env_cancelled.seq,
        );

        // DB row settles at Cancelled — final ground truth read.
        let row = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists");
        assert_eq!(row.status, ConversationStatus::Cancelled);
    }

    // ---------- fork_session ----------

    /// Build a connection whose cmd_rx is drained by a spawned task that
    /// fakes the protocol-level fork reply. Returns the manager so the test
    /// can call `fork_session`. The fake reply task lives until it processes
    /// one Fork command, then exits.
    async fn manager_with_fake_fork(
        conn_id: &str,
        conversation_id: i32,
        forked_session_id: &str,
        original_session_id: &str,
    ) -> (Arc<ConnectionManager>, tokio::task::JoinHandle<()>) {
        use crate::acp::connection::ConnectionCommand;
        let (tx, mut rx, _liveness_rx) = connection_channel(4);
        let mut state = SessionState::new(
            conn_id.to_string(),
            crate::models::agent::AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = Some(conversation_id);
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: conn_id.to_string(),
            agent_type: crate::models::agent::AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation: state.connection_incarnation.clone(),
            tool_lease_registry: state.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .0,
            observed_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .1,
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mgr = Arc::new(ConnectionManager::new());
        {
            let mut map = mgr.connections.lock().await;
            map.insert(conn_id.to_string(), conn);
        }

        let forked = forked_session_id.to_string();
        let original = original_session_id.to_string();
        let join = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if let ConnectionCommand::Fork { reply } = cmd {
                    let _ = reply.send(Ok(crate::acp::types::ForkProtocolResult {
                        forked_session_id: forked.clone(),
                        original_session_id: original.clone(),
                    }));
                    return;
                }
            }
        });
        (mgr, join)
    }

    #[tokio::test]
    async fn fork_session_writes_atomic_two_row_layout() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-happy").await;

        // Pre-existing row: stands in for the conversation about to be forked.
        // Title gets a `[Fork] ` prefix; sibling row inherits the clean title.
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Original Topic".into()),
            Some("feature/x".into()),
        )
        .await
        .unwrap();
        // External_id starts as S1 — manager.fork_session will swap to S2.
        conversation_service::update_external_id(&db.conn, pre.id, "session-S1".into())
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-fork", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork", None, None)
            .await
            .expect("fork_session should succeed");
        let _ = join.await;

        assert_eq!(result.forked_session_id, "session-S2");
        assert_eq!(result.original_session_id, "session-S1");
        let sibling_id = result.sibling_conversation_id;
        assert_ne!(sibling_id, pre.id, "sibling row must be a fresh row");

        // Current row: external_id=S2, title prefixed.
        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.external_id.as_deref(), Some("session-S2"));
        assert_eq!(current.title.as_deref(), Some("[Fork] Original Topic"));

        // Sibling row: external_id=S1, clean title, PendingReview, same folder/git_branch.
        let sibling = conversation_service::get_by_id(&db.conn, sibling_id)
            .await
            .unwrap();
        assert_eq!(sibling.external_id.as_deref(), Some("session-S1"));
        assert_eq!(sibling.title.as_deref(), Some("Original Topic"));
        assert_eq!(sibling.status, "pending_review");
        assert_eq!(sibling.folder_id, folder_id);
        assert_eq!(sibling.git_branch.as_deref(), Some("feature/x"));
    }

    #[tokio::test]
    async fn fork_preserves_generated_title_guard_without_enrolling_sibling() {
        use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
        use crate::db::test_helpers;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-title-guard").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Generated Topic".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::update_external_id(&db.conn, pre.id, "session-S1".into())
            .await
            .unwrap();

        // Live row already has a finalized generated title and a residual job.
        let mut active: conversation::ActiveModel = conversation::Entity::find_by_id(pre.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .into();
        active.auto_title_finalized = Set(true);
        active.update(&db.conn).await.unwrap();

        let now = chrono::Utc::now();
        auto_title_job::ActiveModel {
            conversation_id: Set(pre.id),
            state: Set(AutoTitleJobState::AwaitingTurn),
            attempts: Set(0),
            first_user_text: Set(None),
            first_assistant_text: Set(None),
            first_prompt_at: Set(None),
            locale: Set(None),
            usable_turn_seq: Set(0),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(None),
            config_gen: Set(0),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("seed job on live row");

        let (mgr, join) =
            manager_with_fake_fork("c-fork-guard", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork-guard", None, None)
            .await
            .expect("fork");
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();

        assert!(
            current.auto_title_finalized,
            "live row must retain auto_title_finalized"
        );
        assert!(
            sibling.auto_title_finalized,
            "sibling must copy auto_title_finalized"
        );
        assert!(
            auto_title_job::Entity::find_by_id(pre.id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_some(),
            "existing job stays on the live row"
        );
        assert!(
            auto_title_job::Entity::find_by_id(result.sibling_conversation_id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "sibling must not receive a new auto-title job"
        );
    }

    #[tokio::test]
    async fn fork_session_strips_existing_fork_prefix_without_stacking() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-restack").await;

        // Title already has `[Fork] ` — re-fork must not produce `[Fork] [Fork] ...`.
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("[Fork] Topic".into()),
            None,
        )
        .await
        .unwrap();
        let (mgr, join) =
            manager_with_fake_fork("c-restack", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-restack", None, None)
            .await
            .unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            current.title.as_deref(),
            Some("[Fork] Topic"),
            "should re-stack as single [Fork] prefix, not [Fork] [Fork] ..."
        );
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(sibling.title.as_deref(), Some("Topic"));
    }

    #[tokio::test]
    async fn fork_session_strips_no_space_fork_prefix() {
        // Defensive: a title produced outside the normal flow could lack the
        // space (e.g. external import). The frontend regex `/^\[Fork]\s*/g`
        // tolerated this; the backend strip must too, otherwise re-fork would
        // produce `[Fork] [Fork]xxx`.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-no-space").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("[Fork]NoSpaceTitle".into()),
            None,
        )
        .await
        .unwrap();
        let (mgr, join) =
            manager_with_fake_fork("c-nosp", pre.id, "session-S2", "session-S1").await;
        mgr.fork_session(&db, "c-nosp", None, None).await.unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            current.title.as_deref(),
            Some("[Fork] NoSpaceTitle"),
            "no-space prefix must be tolerantly stripped before re-stacking"
        );
    }

    #[tokio::test]
    async fn fork_session_reads_latest_committed_row_not_a_cached_snapshot() {
        // Regression guard for the write-first ordering in `persist_fork_outcome`.
        // The fork must derive its `[Fork] …` title and the sibling's preserved
        // title from the row's LATEST committed state, read under the write lock
        // the transaction takes with its opening statement — not from a value
        // captured earlier. If a future change reintroduces an early/cached read
        // (e.g. reading before the transaction, or threading a stale title in as
        // a param), a rename committed just before the fork would be clobbered.
        // Here we commit the rename first, then fork, and assert the fork
        // reflects the renamed title on both rows.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-latest").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Stale Original".into()),
            None,
        )
        .await
        .unwrap();
        // Commit a manual rename AFTER creation but BEFORE the fork runs. A
        // correct fork observes this; a stale-snapshot fork would emit
        // "[Fork] Stale Original" / "Stale Original" instead.
        conversation_service::update_title(&db.conn, pre.id, "Renamed By User".into())
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-latest", pre.id, "session-S2", "session-S1").await;
        let result = mgr.fork_session(&db, "c-latest", None, None).await.unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            current.title.as_deref(),
            Some("[Fork] Renamed By User"),
            "fork must prefix the LATEST committed title, not a stale snapshot"
        );
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(
            sibling.title.as_deref(),
            Some("Renamed By User"),
            "sibling must preserve the LATEST committed title, not a stale snapshot"
        );
    }

    #[tokio::test]
    async fn fork_session_errors_without_orphan_when_row_missing() {
        // The current-row write is the transaction's first statement and its
        // `rows_affected == 0` is the not-found signal. If the linked row has
        // vanished (hard-deleted out from under a live connection), the fork
        // must error and, because the sibling INSERT shares the transaction,
        // leave NO orphan sibling behind.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        // Seed a folder but NO conversation row; the connection points at an id
        // that does not exist in the DB.
        let _folder_id = test_helpers::seed_folder(&db, "/tmp/fork-missing").await;
        let missing_conversation_id = 99_999;

        let (mgr, join) = manager_with_fake_fork(
            "c-missing",
            missing_conversation_id,
            "session-S2",
            "session-S1",
        )
        .await;
        let err = mgr
            .fork_session(&db, "c-missing", None, None)
            .await
            .expect_err("fork against a missing row must error");
        let _ = join.await;
        assert!(
            err.to_string().contains("not found"),
            "error should mention the missing row, got: {err}"
        );

        // No orphan: the failed transaction rolled back, so the DB holds zero
        // conversation rows (the sibling INSERT must not have committed).
        let all = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert!(
            all.is_empty(),
            "a failed fork must not leave an orphan sibling row, found: {}",
            all.len()
        );
    }

    #[tokio::test]
    async fn fork_session_errors_without_orphan_when_row_soft_deleted() {
        // Forking a soft-deleted conversation must NOT resurrect it: the sibling
        // insert would set `deleted_at = None`, creating a fresh visible row from
        // deleted data. The write-first claim filters `deleted_at IS NULL`, so a
        // deleted row matches nothing → the fork aborts with a not-found error,
        // writes nothing, and leaves the original row soft-deleted and unchanged.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-deleted").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Doomed Topic".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::update_external_id(&db.conn, pre.id, "session-S1".into())
            .await
            .unwrap();
        conversation_service::soft_delete(&db.conn, pre.id)
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-deleted", pre.id, "session-S2", "session-S1").await;
        let err = mgr
            .fork_session(&db, "c-deleted", None, None)
            .await
            .expect_err("fork against a soft-deleted row must error");
        let _ = join.await;
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("deleted"),
            "error should mention the missing/deleted row, got: {err}"
        );

        // No resurrection: exactly the original row remains, still soft-deleted,
        // still bound to S1 — no visible sibling was inserted, and the current
        // row was neither re-pointed at S2 nor `[Fork]`-prefixed.
        let all = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert_eq!(all.len(), 1, "no sibling row should have been inserted");
        let only = &all[0];
        assert_eq!(only.id, pre.id);
        assert!(
            only.deleted_at.is_some(),
            "the original row must stay soft-deleted"
        );
        assert_eq!(
            only.external_id.as_deref(),
            Some("session-S1"),
            "the deleted row must not be re-pointed at the forked session"
        );
        assert_eq!(
            only.title.as_deref(),
            Some("Doomed Topic"),
            "the deleted row must not gain a [Fork] prefix"
        );
    }

    #[tokio::test]
    async fn fork_session_rejects_unbound_connection() {
        // Without a linked conversation_id the sibling row would orphan S1
        // history (no row to point at it). fork_session must refuse early —
        // BEFORE sending the Fork command to the agent, so we don't burn an
        // ACP round-trip on a request we can't persist.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c-unbound".into(), fake_connection("c-unbound", None));
        }
        let err = mgr
            .fork_session(&db, "c-unbound", None, None)
            .await
            .expect_err("unbound fork must error");
        assert!(
            err.to_string().contains("linked conversation row"),
            "error should mention missing linkage, got: {err}"
        );
    }

    #[tokio::test]
    async fn fork_session_links_unbound_row_from_caller_ids() {
        // Bug #2: a conversation opened from history resumes via `session_id`
        // but its row isn't bound to the connection until the first prompt
        // fires `ConversationLinked`. A fork-send forks BEFORE that prompt, so
        // fork_session must adopt the caller-supplied (conversation_id,
        // folder_id) and succeed — instead of rejecting as unlinked (which is
        // exactly what the user hit forking a conversation opened from history).
        use crate::acp::connection::ConnectionCommand;
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-relink").await;
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("History".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::update_external_id(&db.conn, pre.id, "session-S1".into())
            .await
            .unwrap();

        // A connection with NO linked conversation_id — mirrors a fresh resume
        // of a historical conversation that hasn't sent a prompt yet.
        let (tx, mut rx, _liveness_rx) = connection_channel(4);
        let mut state = SessionState::new(
            "c-relink".to_string(),
            AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = None;
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: "c-relink".to_string(),
            agent_type: AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            owner_operation_id: None,
            ownership_generation: 0,
            connection_incarnation: state.connection_incarnation.clone(),
            tool_lease_registry: state.tool_lease_registry.clone(),
            parent_connection_id: None,
            cmd_tx: tx,
            control_tx: test_control_sender(),
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .0,
            observed_config: matching_config_pair(
                String::new(),
                "system",
                crate::acp::delegation::route::test_empty_route_plan().fingerprint,
            )
            .1,
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c-relink".to_string(), conn);
        }
        let join = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if let ConnectionCommand::Fork { reply } = cmd {
                    let _ = reply.send(Ok(crate::acp::types::ForkProtocolResult {
                        forked_session_id: "session-S2".to_string(),
                        original_session_id: "session-S1".to_string(),
                    }));
                    return;
                }
            }
        });

        let result = mgr
            .fork_session(&db, "c-relink", Some(pre.id), Some(folder_id))
            .await
            .expect("fork must link the unbound row from caller ids and succeed");
        let _ = join.await;

        assert_eq!(result.forked_session_id, "session-S2");
        // The connection is now linked to the row...
        let linked = mgr.get_state("c-relink").await.expect("connection exists");
        assert_eq!(linked.read().await.conversation_id, Some(pre.id));
        // ...the current row is re-pointed to S2 with a `[Fork]` title...
        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.external_id.as_deref(), Some("session-S2"));
        assert_eq!(current.title.as_deref(), Some("[Fork] History"));
        // ...and a sibling preserves the pre-fork S1 history.
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(sibling.external_id.as_deref(), Some("session-S1"));
    }

    // --- wait_for_session_options polling ----------------------------------
    //
    // These tests exercise the probe's wait loop directly by hand-seeding
    // `SessionState` on an injected connection. They avoid spawning a real
    // agent (which is what `probe_agent_options` itself would do) — the goal
    // is to lock in the three behaviors the public API depends on:
    //   1. ready+grace → Ok(snapshot) reflecting current state
    //   2. never-ready within timeout → Err(ProbeTimedOut), not Ok(empty)
    //   3. selectors_ready=true with empty options → Ok(empty snapshot)

    use crate::acp::types::{
        SessionConfigKindInfo, SessionConfigOptionInfo, SessionConfigSelectInfo, SessionModeInfo,
        SessionModeStateInfo,
    };

    fn sample_modes() -> SessionModeStateInfo {
        SessionModeStateInfo {
            current_mode_id: "default".into(),
            available_modes: vec![
                SessionModeInfo {
                    id: "default".into(),
                    name: "Default".into(),
                    description: None,
                },
                SessionModeInfo {
                    id: "yolo".into(),
                    name: "YOLO".into(),
                    description: None,
                },
            ],
        }
    }

    fn sample_config_options() -> Vec<SessionConfigOptionInfo> {
        vec![SessionConfigOptionInfo {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: None,
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: "sonnet".into(),
                options: vec![],
                groups: vec![],
            }),
        }]
    }

    #[tokio::test]
    async fn wait_for_session_options_returns_snapshot_after_ready_plus_grace() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection(
            "probe-1",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // Seed the state the probe is waiting on. Done BEFORE the wait
        // starts so the very first poll already sees ready=true and only
        // the 500 ms grace period gates the return.
        {
            let state = mgr.get_state("probe-1").await.expect("state");
            let mut s = state.write().await;
            s.modes = Some(sample_modes());
            s.config_options = Some(sample_config_options());
            s.selectors_ready = true;
        }

        let start = std::time::Instant::now();
        let snapshot = mgr
            .wait_for_session_options("probe-1", Duration::from_secs(2))
            .await
            .expect("ready+grace path must return Ok");
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(450),
            "expected ~500ms grace, observed {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "should NOT wait the full 2s timeout, observed {elapsed:?}"
        );
        assert_eq!(snapshot.config_options.len(), 1);
        assert!(snapshot.modes.is_some());
    }

    #[tokio::test]
    async fn wait_for_session_options_times_out_when_selectors_never_ready() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection(
            "probe-2",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // Critical guarantee: even though `config_options` is `Some(...)`,
        // because `selectors_ready` is still false, the wait MUST timeout
        // and return Err — never Ok(empty) which would mislead the UI.
        {
            let state = mgr.get_state("probe-2").await.expect("state");
            let mut s = state.write().await;
            s.config_options = Some(vec![]);
            s.selectors_ready = false;
        }

        let err = mgr
            .wait_for_session_options("probe-2", Duration::from_millis(300))
            .await
            .expect_err("timeout path must return Err");
        assert!(
            matches!(err, AcpError::ProbeTimedOut),
            "expected ProbeTimedOut, got {err:?}"
        );
        assert_eq!(err.code(), Some("probe_timed_out"));
    }

    #[tokio::test]
    async fn wait_for_session_options_returns_empty_when_ready_with_no_options() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection(
            "probe-3",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // Real outcome the UI renders as "agent has nothing to configure":
        // selectors_ready=true, modes=None, config_options=None. Must
        // succeed, not error — this is the path that distinguishes a
        // legitimately empty agent from an unresponsive one.
        {
            let state = mgr.get_state("probe-3").await.expect("state");
            let mut s = state.write().await;
            s.modes = None;
            s.config_options = None;
            s.selectors_ready = true;
        }

        let snapshot = mgr
            .wait_for_session_options("probe-3", Duration::from_secs(2))
            .await
            .expect("ready-empty path must return Ok, not Err");
        assert!(snapshot.modes.is_none());
        assert!(snapshot.config_options.is_empty());
    }

    #[tokio::test]
    async fn wait_for_session_options_unknown_connection_errors_immediately() {
        let mgr = ConnectionManager::new();
        let err = mgr
            .wait_for_session_options("does-not-exist", Duration::from_secs(5))
            .await
            .expect_err("missing connection must error");
        assert!(
            matches!(err, AcpError::ConnectionNotFound(_)),
            "expected ConnectionNotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn apply_event_error_populates_last_error_snapshot() {
        // Directly drives SessionState::apply_event to assert the Error
        // arm now writes `last_error` (rather than being a no-op as it
        // was before). The probe path reads this to surface the
        // agent's own error message after cleanup runs.
        use crate::acp::session_state::SessionState;
        let mut s = SessionState::new(
            "c1".into(),
            crate::models::agent::AgentType::ClaudeCode,
            None,
            "test-window".into(),
            None,
        );
        assert!(s.last_error.is_none(), "fresh state has no error");

        s.apply_event(&AcpEvent::Error {
            message: "agent exploded".into(),
            agent_type: "claude_code".into(),
            code: Some("sdk_not_installed".into()),
            details: None,
            terminal: true,
        });
        let captured = s.last_error.as_ref().expect("error must be captured");
        assert_eq!(captured.message, "agent exploded");
        assert_eq!(captured.code.as_deref(), Some("sdk_not_installed"));

        // A second Error event overwrites — `last_error` is "latest",
        // not "first". Keeps post-mortem reads aligned with what the
        // user most recently observed on the event channel.
        s.apply_event(&AcpEvent::Error {
            message: "second failure".into(),
            agent_type: "claude_code".into(),
            code: None,
            details: None,
            terminal: true,
        });
        let captured = s.last_error.as_ref().unwrap();
        assert_eq!(captured.message, "second failure");
        assert!(captured.code.is_none());
    }

    // --- live feedback: submit gate + consume drain --------------------

    /// Make a test connection feedback-capable AND mid-turn (the happy state).
    async fn mark_feedback_ready(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.unwrap();
        let mut s = state.write().await;
        s.feedback_tool_available = true;
        s.turn_in_flight = true;
    }

    async fn set_feedback_tool_available(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.unwrap();
        state.write().await.feedback_tool_available = true;
    }

    #[tokio::test]
    async fn submit_feedback_rejected_when_tool_unavailable() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        // feedback_tool_available defaults false: the agent never got the tool
        // (e.g. its session started before the feature was enabled), even mid-turn.
        let state = mgr.get_state("c1").await.unwrap();
        state.write().await.turn_in_flight = true;
        let err = mgr.submit_feedback("c1", "note".into()).await.unwrap_err();
        assert!(matches!(err, AcpError::FeedbackDisabled));
        assert!(state.read().await.feedback.is_empty());
    }

    #[tokio::test]
    async fn submit_feedback_rejected_when_no_turn_in_flight() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        // Tool available but no turn in flight → nothing to steer.
        set_feedback_tool_available(&mgr, "c1").await;
        let err = mgr.submit_feedback("c1", "note".into()).await.unwrap_err();
        assert!(matches!(err, AcpError::NoActiveTurn));
        // And nothing was appended.
        let state = mgr.get_state("c1").await.unwrap();
        assert!(state.read().await.feedback.is_empty());
    }

    #[tokio::test]
    async fn submit_feedback_missing_connection_errors() {
        let mgr = ConnectionManager::new();
        let err = mgr
            .submit_feedback("nope", "note".into())
            .await
            .unwrap_err();
        assert!(matches!(err, AcpError::ConnectionNotFound(_)));
    }

    #[tokio::test]
    async fn submit_feedback_appends_when_turn_in_flight() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        let item = mgr
            .submit_feedback("c1", "  use UserService  ".into())
            .await
            .unwrap();
        assert_eq!(item.status, FeedbackStatus::Pending);
        // Stored text is trimmed.
        assert_eq!(item.text, "use UserService");
        let state = mgr.get_state("c1").await.unwrap();
        let s = state.read().await;
        assert_eq!(s.feedback.len(), 1);
        assert_eq!(s.feedback[0].text, "use UserService");
    }

    #[tokio::test]
    async fn submit_feedback_rejects_empty_and_oversized() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        // Empty / whitespace-only → rejected, nothing appended.
        for empty in ["", "   ", "\n\t "] {
            let err = mgr.submit_feedback("c1", empty.into()).await.unwrap_err();
            assert!(matches!(err, AcpError::InvalidFeedback(_)));
        }
        // Oversized → rejected.
        let huge = "x".repeat(MAX_FEEDBACK_CHARS + 1);
        let err = mgr.submit_feedback("c1", huge).await.unwrap_err();
        assert!(matches!(err, AcpError::InvalidFeedback(_)));
        // Exactly at the bound is accepted.
        let at_bound = "y".repeat(MAX_FEEDBACK_CHARS);
        assert!(mgr.submit_feedback("c1", at_bound).await.is_ok());
        let state = mgr.get_state("c1").await.unwrap();
        assert_eq!(
            state.read().await.feedback.len(),
            1,
            "only the valid note stuck"
        );
    }

    // --- ask_user_question: register / answer / cancel -------------------

    fn q_spec() -> Vec<QuestionSpec> {
        vec![crate::acp::question::QuestionSpec {
            id: "qa".into(),
            question: "Which approach?".into(),
            header: "Approach".into(),
            multi_select: false,
            options: vec![
                crate::acp::question::QuestionOption {
                    label: "A".into(),
                    description: String::new(),
                },
                crate::acp::question::QuestionOption {
                    label: "B".into(),
                    description: String::new(),
                },
            ],
            is_secret: false,
            recovery: None,
        }]
    }

    fn shared_control_reserve_request(
        connection_id: &str,
        conversation_id: i32,
    ) -> SharedReserveRequest {
        SharedReserveRequest {
            key: SharedSessionKey::Conversation(conversation_id),
            connection_id: connection_id.to_string(),
            launch_identity: SharedLaunchIdentity {
                agent_type: AgentType::Codex,
                working_dir_fingerprint: "shared-control-cwd".into(),
                external_session_id: None,
                attach_mode: SessionAttachMode::Default,
                route_fingerprint: "shared-control-route".into(),
                route_capability: crate::acp::shared_session::SharedRouteCapability::Standard,
                terminal_shell_fingerprint: "shared-control-shell".into(),
                purpose: ConnectionPurpose::User,
            },
            client_instance_id: "shared-control-client".into(),
            device_id: "shared-control-device".into(),
            request_id: "shared-control-connect".into(),
            retry_failed_generation: None,
            now: tokio::time::Instant::now(),
            now_utc: chrono::Utc::now(),
        }
    }

    async fn ready_shared_control_manager(
        connection_id: &str,
        conversation_id: i32,
    ) -> (
        Arc<ConnectionManager>,
        SharedSessionAttachment,
        SharedMutationGuard,
    ) {
        let manager = Arc::new(ConnectionManager::new());
        manager
            .insert_test_connection(connection_id, AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let broker = manager.shared_session_broker();
        let attachment = broker
            .reserve_or_attach(shared_control_reserve_request(
                connection_id,
                conversation_id,
            ))
            .await
            .unwrap()
            .attachment;
        let (state, emitter) = manager
            .get_state_and_emitter(connection_id)
            .await
            .expect("test connection state");
        broker
            .install_registered(
                connection_id,
                attachment.generation,
                "shared-control-driver".into(),
                state,
                emitter,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        broker
            .mark_ready(
                connection_id,
                attachment.generation,
                "shared-control-driver",
            )
            .await
            .unwrap();
        let guard = SharedMutationGuard {
            connection_id: connection_id.to_string(),
            generation: attachment.generation,
            lease_id: attachment.lease_id.clone(),
        };
        (manager, attachment, guard)
    }

    fn assert_shared_interaction_already_resolved(result: Result<(), AcpError>) {
        assert!(matches!(
            result,
            Err(AcpError::Shared(
                SharedSessionError::InteractionAlreadyResolved
            ))
        ));
    }

    #[tokio::test]
    async fn shared_missing_question_and_plan_responders_return_stable_loser() {
        let (manager, attachment, guard) =
            ready_shared_control_manager("shared-missing", 1901).await;
        let broker = manager.shared_session_broker();

        broker
            .observe_interaction(
                &attachment.connection_id,
                attachment.generation,
                "shared-control-driver",
                SharedInteractionKind::Question,
                "missing-question",
            )
            .await
            .unwrap();
        assert_shared_interaction_already_resolved(
            manager
                .answer_shared_question(SharedInteractionRequest {
                    guard: guard.clone(),
                    interaction_id: "missing-question".into(),
                    answer: QuestionAnswer::default(),
                })
                .await,
        );
        assert_shared_interaction_already_resolved(
            manager
                .answer_shared_question(SharedInteractionRequest {
                    guard: guard.clone(),
                    interaction_id: "missing-question".into(),
                    answer: QuestionAnswer::default(),
                })
                .await,
        );

        broker
            .observe_interaction(
                &attachment.connection_id,
                attachment.generation,
                "shared-control-driver",
                SharedInteractionKind::PlanApproval,
                "missing-plan",
            )
            .await
            .unwrap();
        let plan_answer = PlanApprovalAnswer {
            decision: PlanApprovalDecision::Approve,
            feedback: None,
        };
        assert_shared_interaction_already_resolved(
            manager
                .answer_shared_plan_approval(SharedInteractionRequest {
                    guard: guard.clone(),
                    interaction_id: "missing-plan".into(),
                    answer: plan_answer.clone(),
                })
                .await,
        );
        assert_shared_interaction_already_resolved(
            manager
                .answer_shared_plan_approval(SharedInteractionRequest {
                    guard,
                    interaction_id: "missing-plan".into(),
                    answer: plan_answer,
                })
                .await,
        );
    }

    #[tokio::test]
    async fn shared_in_flight_question_loser_is_stable_and_never_reopens() {
        let (manager, attachment, guard) =
            ready_shared_control_manager("shared-in-flight", 1902).await;
        let registered = manager
            .register_question(&attachment.connection_id, q_spec())
            .await
            .expect("question registered");
        manager
            .claim_question_settlement(&registered.question_id)
            .await;
        manager
            .shared_session_broker()
            .observe_interaction(
                &attachment.connection_id,
                attachment.generation,
                "shared-control-driver",
                SharedInteractionKind::Question,
                &registered.question_id,
            )
            .await
            .unwrap();

        assert_shared_interaction_already_resolved(
            manager
                .answer_shared_question(SharedInteractionRequest {
                    guard: guard.clone(),
                    interaction_id: registered.question_id.clone(),
                    answer: QuestionAnswer::default(),
                })
                .await,
        );
        manager
            .release_question_settlement(&registered.question_id)
            .await;
        assert_shared_interaction_already_resolved(
            manager
                .answer_shared_question(SharedInteractionRequest {
                    guard,
                    interaction_id: registered.question_id.clone(),
                    answer: QuestionAnswer::default(),
                })
                .await,
        );
        assert!(manager
            .pending_questions
            .lock()
            .await
            .contains_key(&registered.question_id));
        manager
            .finish_question_settlement(&registered.question_id, None)
            .await;
    }

    #[tokio::test]
    async fn pending_question_parent_connection_id_peeks_without_consuming() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cq-owner", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_question("cq-owner", q_spec())
            .await
            .expect("registered");
        assert_eq!(
            mgr.pending_question_parent_connection_id(&reg.question_id)
                .await
                .as_deref(),
            Some("cq-owner")
        );
        // Peek leaves the entry answerable.
        assert!(mgr
            .get_state("cq-owner")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some());
        assert!(mgr
            .pending_question_parent_connection_id("missing-q")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn register_then_answer_question_resolves_and_clears() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cq", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_question("cq", q_spec())
            .await
            .expect("registered");
        // SessionState reflects the pending question for snapshot recovery.
        assert!(mgr
            .get_state("cq")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some());

        let answer = crate::acp::question::QuestionAnswer {
            answers: vec![crate::acp::question::QuestionAnswerItem {
                question_id: "qa".into(),
                labels: vec!["A".into()],
            }],
            declined: false,
        };
        // Stale/wrong caller connection_id still routes by question_id.
        mgr.answer_question("stale-caller", &reg.question_id, answer)
            .await
            .unwrap();

        // The blocked listener's receiver resolves with the self-describing
        // outcome (question text joined in).
        let outcome = reg.answer_rx.await.expect("answer delivered");
        assert!(!outcome.declined);
        assert_eq!(outcome.answers.len(), 1);
        assert_eq!(outcome.answers[0].question, "Which approach?");
        assert_eq!(outcome.answers[0].selected, vec!["A".to_string()]);
        // pending_question cleared after resolve.
        assert!(mgr
            .get_state("cq")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_none());

        // Idempotent: answering an already-resolved id is a no-op success.
        mgr.answer_question("cq", &reg.question_id, Default::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancel_question_clears_and_drops_sender() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cqx", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg = mgr.register_question("cqx", q_spec()).await.unwrap();
        mgr.cancel_question("cqx", &reg.question_id).await;
        // Dropping the sender surfaces to the parked listener as a recv error
        // (which it renders as a declined outcome).
        assert!(reg.answer_rx.await.is_err());
        assert!(mgr
            .get_state("cqx")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_none());
    }

    #[tokio::test]
    async fn cancel_questions_by_parent_drops_only_matching_connection() {
        // The run_connection teardown guard sweeps a tearing-down connection's
        // parked ask without touching other connections' pending questions.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("ca", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mgr.insert_test_connection("cb", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg_a = mgr.register_question("ca", q_spec()).await.unwrap();
        let reg_b = mgr.register_question("cb", q_spec()).await.unwrap();

        // Tear down only connection "ca".
        mgr.cancel_questions_by_parent("ca").await;

        // ca's parked listener is unblocked (sender dropped → recv error) and its
        // card cleared; cb is untouched.
        assert!(reg_a.answer_rx.await.is_err());
        assert!(mgr
            .get_state("ca")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_none());
        assert!(mgr
            .get_state("cb")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some());

        // cb still resolves normally afterwards.
        mgr.answer_question("cb", &reg_b.question_id, Default::default())
            .await
            .unwrap();
        assert!(reg_b.answer_rx.await.is_ok());
    }

    #[tokio::test]
    async fn register_then_answer_plan_approval_resolves_and_clears() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("pa", AgentType::Grok, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_plan_approval("pa", "call-1".into(), "# Plan\n- step".into())
            .await
            .expect("registered");
        // SessionState reflects the pending approval for snapshot recovery.
        {
            let state = mgr.get_state("pa").await.unwrap();
            let guard = state.read().await;
            let pending = guard.pending_plan_approval.as_ref().expect("pending set");
            assert_eq!(pending.plan_markdown, "# Plan\n- step");
            assert_eq!(pending.tool_call_id, "call-1");
        }

        mgr.answer_plan_approval(
            "pa",
            &reg.approval_id,
            crate::acp::plan_approval::PlanApprovalAnswer {
                decision: crate::acp::plan_approval::PlanApprovalDecision::RequestChanges,
                feedback: Some("use SSE".into()),
            },
        )
        .await
        .unwrap();

        // The blocked ext handler's receiver resolves with the user's decision.
        let got = reg.answer_rx.await.expect("answer delivered");
        assert_eq!(
            got.decision,
            crate::acp::plan_approval::PlanApprovalDecision::RequestChanges
        );
        assert_eq!(got.feedback.as_deref(), Some("use SSE"));
        // pending_plan_approval cleared after resolve.
        assert!(mgr
            .get_state("pa")
            .await
            .unwrap()
            .read()
            .await
            .pending_plan_approval
            .is_none());

        // Idempotent: answering an already-resolved id is a no-op success.
        mgr.answer_plan_approval(
            "pa",
            &reg.approval_id,
            crate::acp::plan_approval::PlanApprovalAnswer {
                decision: crate::acp::plan_approval::PlanApprovalDecision::Approve,
                feedback: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn register_plan_approval_refuses_second_pending_on_same_connection() {
        // At most one approval per connection (the agent is blocked in exit_plan_mode).
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("pa2", AgentType::Grok, None, EventEmitter::Noop)
            .await;
        let _reg = mgr
            .register_plan_approval("pa2", "c1".into(), "plan".into())
            .await
            .expect("first registers");
        assert!(mgr
            .register_plan_approval("pa2", "c2".into(), "plan2".into())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn cancel_plan_approvals_by_parent_drops_sender_and_clears() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("pax", AgentType::Grok, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_plan_approval("pax", "c".into(), "plan".into())
            .await
            .unwrap();
        mgr.cancel_plan_approvals_by_parent("pax").await;
        // Dropping the sender surfaces to the parked handler as a recv error
        // (which it renders as a disconnect reply — plan mode stays active).
        assert!(reg.answer_rx.await.is_err());
        assert!(mgr
            .get_state("pax")
            .await
            .unwrap()
            .read()
            .await
            .pending_plan_approval
            .is_none());
    }

    #[tokio::test]
    async fn compensate_clears_card_when_entry_drained_before_request_emit() {
        // Regression for the teardown event-ordering race: register inserts, the
        // sweep drains the entry, THEN register's QuestionRequest emit lands. The
        // post-emit presence check must emit a compensating QuestionResolved so no
        // client keeps a card with no live backend waiter, and signal decline.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cc", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let (state, emitter) = mgr.get_state_and_emitter("cc").await.unwrap();

        // Simulate register's QuestionRequest emit for an entry that has already
        // been drained (never inserted here): the card shows, nothing is parked.
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::QuestionRequest {
                question_id: "q1".into(),
                questions: q_spec(),
            },
        )
        .await;
        assert!(state.read().await.pending_question.is_some(), "card shown");

        // Missing entry → compensate clears the card and reports decline.
        assert!(
            mgr.compensate_if_question_drained("q1", &state, &emitter)
                .await,
            "missing entry is compensated"
        );
        assert!(
            state.read().await.pending_question.is_none(),
            "compensating QuestionResolved cleared the card"
        );

        // A genuinely-parked entry is left alone (no false compensation).
        let reg = mgr.register_question("cc", q_spec()).await.unwrap();
        assert!(
            !mgr.compensate_if_question_drained(&reg.question_id, &state, &emitter)
                .await,
            "present entry is not compensated"
        );
        assert!(state.read().await.pending_question.is_some());
    }

    #[tokio::test]
    async fn register_question_unknown_connection_is_none() {
        let mgr = ConnectionManager::new();
        assert!(mgr.register_question("nope", q_spec()).await.is_none());
    }

    #[tokio::test]
    async fn second_concurrent_ask_is_refused_and_first_stays_answerable() {
        // A parallel/misbehaving client could fire two asks on one connection
        // before the first resolves. The single-slot card/snapshot can't hold
        // two, so the second is refused (None → declined) and the FIRST stays
        // intact and answerable — never orphaned.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cc2", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let first = mgr
            .register_question("cc2", q_spec())
            .await
            .expect("first registers");
        // Second concurrent ask on the same connection is refused.
        assert!(
            mgr.register_question("cc2", q_spec()).await.is_none(),
            "second concurrent ask must be refused"
        );
        // The first is still the pending one and still answerable.
        let state = mgr.get_state("cc2").await.unwrap();
        assert_eq!(
            state
                .read()
                .await
                .pending_question
                .as_ref()
                .map(|p| p.question_id.clone()),
            Some(first.question_id.clone())
        );
        mgr.answer_question(
            "cc2",
            &first.question_id,
            crate::acp::question::QuestionAnswer {
                answers: vec![crate::acp::question::QuestionAnswerItem {
                    question_id: "qa".into(),
                    labels: vec!["A".into()],
                }],
                declined: false,
            },
        )
        .await
        .unwrap();
        assert!(first.answer_rx.await.is_ok(), "first ask resolves");
        // After resolve, a new ask is accepted again.
        assert!(mgr.register_question("cc2", q_spec()).await.is_some());
    }

    #[tokio::test]
    async fn read_pending_is_readonly_commit_marks_delivered() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        let a = mgr.submit_feedback("c1", "a".into()).await.unwrap();
        let b = mgr.submit_feedback("c1", "b".into()).await.unwrap();

        // READ returns both pending notes (insert order) WITHOUT mutating state.
        let pending = mgr.read_pending_feedback("c1").await;
        let texts: Vec<&str> = pending.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b"]);
        // A second read still returns them — read is non-destructive, so an
        // abandoned (peer-closed) call leaves the notes retryable.
        assert_eq!(mgr.read_pending_feedback("c1").await.len(), 2);
        {
            let state = mgr.get_state("c1").await.unwrap();
            assert!(state
                .read()
                .await
                .feedback
                .iter()
                .all(|f| f.status == FeedbackStatus::Pending));
        }

        // COMMIT marks the named notes delivered.
        mgr.commit_feedback_delivered("c1", vec![a.id.clone(), b.id.clone()])
            .await;
        // Now READ returns nothing (delivered notes are filtered out).
        assert!(mgr.read_pending_feedback("c1").await.is_empty());
        let state = mgr.get_state("c1").await.unwrap();
        assert!(state
            .read()
            .await
            .feedback
            .iter()
            .all(|f| f.status == FeedbackStatus::Delivered));

        // COMMIT is idempotent — re-committing already-delivered ids is a no-op.
        mgr.commit_feedback_delivered("c1", vec![a.id, b.id]).await;
    }

    #[tokio::test]
    async fn read_pending_missing_connection_returns_empty() {
        let mgr = ConnectionManager::new();
        assert!(mgr.read_pending_feedback("nope").await.is_empty());
        // Commit on a missing connection is a safe no-op.
        mgr.commit_feedback_delivered("nope", vec!["x".into()])
            .await;
    }

    // ─── Task 7: root safe fallback + child never fallback + late close ──

    fn root_codeg_request() -> SpawnAttemptRequest {
        use crate::acp::delegation::route::{
            DelegationRoutePolicy, DelegationRouteSource, NativeSuppressionPlan,
            ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        SpawnAttemptRequest {
            origin: DelegationConnectionOrigin::Root,
            plan: DelegationRoutePlan {
                managed: true,
                requested: DelegationRoutePolicy::Codeg,
                effective: DelegationRoutePolicy::Codeg,
                source: DelegationRouteSource::GlobalDefault,
                native_suppression: NativeSuppressionPlan::CodexMultiAgentFalse,
                expose_codeg_delegation: true,
                degraded_reason: None,
                adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
                fingerprint: "test-root-codeg".into(),
            },
        }
    }

    fn codeg_child_request() -> SpawnAttemptRequest {
        let mut req = root_codeg_request();
        req.origin = DelegationConnectionOrigin::CodegChild;
        req.plan.source = crate::acp::delegation::route::DelegationRouteSource::ForcedChild;
        req
    }

    #[tokio::test]
    async fn root_safe_fallback_retries_once_only_for_typed_route_bootstrap_failure() {
        let harness = SpawnAttemptHarness::new([
            Err(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            )),
            Ok("native-connection".into()),
        ]);
        let result = spawn_with_safe_fallback(root_codeg_request(), &harness)
            .await
            .unwrap();
        assert_eq!(result.connection_id, "native-connection");
        assert_eq!(
            result.plan.source,
            crate::acp::delegation::route::DelegationRouteSource::SafeFallback
        );
        assert_eq!(harness.attempt_count(), 2);

        let fatal = SpawnAttemptHarness::new([Err(RouteBootstrapOutcome::Fatal(
            AcpError::SdkNotInstalled("missing SDK".into()),
        ))]);
        assert!(matches!(
            spawn_with_safe_fallback(root_codeg_request(), &fatal).await,
            Err(AcpError::SdkNotInstalled(_))
        ));
        assert_eq!(fatal.attempt_count(), 1);
    }

    #[tokio::test]
    async fn forced_child_never_falls_back_and_late_close_never_switches_route() {
        let harness = SpawnAttemptHarness::new([Err(RouteBootstrapOutcome::RouteSpecific(
            RouteDegradedReason::CompanionInitializationFailed,
        ))]);
        assert_eq!(
            spawn_with_safe_fallback(codeg_child_request(), &harness)
                .await
                .unwrap_err()
                .code(),
            Some("route_unavailable")
        );
        assert_eq!(harness.attempt_count(), 1);

        let state = state_with_route(codeg_plan_for_late_close());
        apply_companion_closed(&state).await;
        let snapshot = state.read().await.to_snapshot();
        assert_eq!(
            snapshot.delegation_route.effective,
            crate::acp::delegation::route::DelegationRoutePolicy::Codeg
        );
        assert!(!snapshot.delegation_route.delegation_available);
    }

    fn codeg_plan_for_late_close() -> DelegationRoutePlan {
        use crate::acp::delegation::route::{
            DelegationRoutePolicy, DelegationRouteSource, NativeSuppressionPlan,
            ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression: NativeSuppressionPlan::CodexMultiAgentFalse,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: "late-close".into(),
        }
    }

    fn state_with_route(
        plan: DelegationRoutePlan,
    ) -> Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>> {
        let mut s = crate::acp::session_state::SessionState::new(
            "late-close".into(),
            AgentType::Codex,
            None,
            "test".into(),
            None,
        );
        s.set_route_plan_snapshot(&plan);
        s.set_delegation_available(true);
        Arc::new(tokio::sync::RwLock::new(s))
    }

    /// Production-shaped teardown: abort task, awaited revoke, observe map
    /// absence before return — including delayed cleanup after abort.
    #[tokio::test]
    async fn teardown_unexposed_revokes_and_observes_map_absence_before_return() {
        use crate::acp::connection::AgentConnection;
        use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker};
        use crate::acp::delegation::lease::CompanionLeaseRegistry;
        use crate::acp::delegation::listener::{TokenEntry, TokenRegistry};
        use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
        use crate::acp::delegation::types::DelegationError;
        use crate::acp::types::ConnectionStatus;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct EmptyLookup;
        #[async_trait::async_trait]
        impl ConversationDepthLookup for EmptyLookup {
            async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
                Ok(None)
            }
        }
        struct NoQuestions;
        #[async_trait::async_trait]
        impl SessionQuestionAccess for NoQuestions {
            async fn register_question(
                &self,
                _parent_connection_id: &str,
                _questions: Vec<QuestionSpec>,
            ) -> Option<RegisteredQuestion> {
                None
            }
            async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}
            async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
        }

        let mgr = ConnectionManager::new();
        let leases = Arc::new(CompanionLeaseRegistry::default());
        let tokens = Arc::new(TokenRegistry::default());
        let token = "teardown-tok".to_string();
        tokens
            .register(
                token.clone(),
                TokenEntry::legacy("unexposed-1", PathBuf::from("/tmp")),
            )
            .await;
        let mut waiter = leases.register(&token).await;
        leases.mark_ready(&token).await.unwrap();
        waiter.wait_ready(Duration::from_millis(50)).await.unwrap();

        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        mgr.install_delegation(crate::acp::connection::DelegationInjection {
            broker,
            continuation_coordinator: std::sync::Weak::new(),
            parent_connection_exit_causes: Arc::new(
                crate::acp::connection::ParentConnectionExitCauses::default(),
            ),
            tokens: Arc::clone(&tokens),
            leases: Arc::clone(&leases),
            socket_path: PathBuf::from("/tmp/codeg-test.sock"),
            agent_availability: Arc::new(AllAgentsAvailable),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
            questions: Arc::new(NoQuestions)
                as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            plan_approvals: no_plan_approvals(),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        });

        let conn_id = "unexposed-1".to_string();
        let mut state =
            SessionState::new(conn_id.clone(), AgentType::Codex, None, "test".into(), None);
        state.delegation_token = Some(token.clone());
        state.status = ConnectionStatus::Connecting;
        let connection_incarnation = state.connection_incarnation.clone();
        let tool_lease_registry = state.tool_lease_registry.clone();
        let state = Arc::new(RwLock::new(state));
        let (tx, _rx, _liveness_rx) = connection_channel(4);
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = codeg_plan_for_late_close();
        let (spawn_config, observed_config) = matching_config_pair(
            "agent",
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );

        let removed = Arc::new(AtomicBool::new(false));
        let child_pid = Arc::new(std::sync::atomic::AtomicU32::new(4242));
        let event_order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));

        // Connection task parks on pending() — Disconnect cannot wake it; only
        // abort terminates (proves teardown does not rely on a drained Disconnect).
        let join = tokio::spawn(async move {
            std::future::pending::<()>().await;
        });
        // Ensure the task is scheduled before we store its abort handle.
        tokio::task::yield_now().await;
        let abort = join.abort_handle();

        // Delayed map removal after revoke (production cleanup-guard race).
        // Teardown must await absence — not force-remove.
        let connections = mgr.connections.clone();
        let conn_id_task = conn_id.clone();
        let tokens_watch = Arc::clone(&tokens);
        let token_watch = token.clone();
        let removed_flag = Arc::clone(&removed);
        let child_pid_cleanup = Arc::clone(&child_pid);
        let order_cleanup = Arc::clone(&event_order);
        let cleanup_task = tokio::spawn(async move {
            loop {
                if tokens_watch.lookup(&token_watch).await.is_none() {
                    order_cleanup.lock().unwrap().push("revoked");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
            connections.lock().await.remove(&conn_id_task);
            removed_flag.store(true, Ordering::SeqCst);
            order_cleanup.lock().unwrap().push("map_removed");
            tokio::time::sleep(Duration::from_millis(80)).await;
            child_pid_cleanup.store(0, Ordering::SeqCst);
            order_cleanup.lock().unwrap().push("process_reaped");
        });

        mgr.connections.lock().await.insert(
            conn_id.clone(),
            AgentConnection {
                id: conn_id.clone(),
                agent_type: AgentType::Codex,
                status: ConnectionStatus::Connecting,
                owner_window_label: "test".into(),
                owner_operation_id: None,
                ownership_generation: 0,
                connection_incarnation,
                tool_lease_registry,
                parent_connection_id: None,
                cmd_tx: tx,
                control_tx: test_control_sender(),
                task_abort: Some(abort),
                state: Arc::clone(&state),
                emitter: EventEmitter::Noop,
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell,
                route_plan,
                origin: DelegationConnectionOrigin::Root,
                route_preference: None,
                route_capability:
                    crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
                child_pid: child_pid.clone(),
            },
        );

        assert!(mgr.connections.lock().await.contains_key(&conn_id));
        assert!(
            mgr.connections
                .lock()
                .await
                .get(&conn_id)
                .and_then(|c| c.task_abort.clone())
                .is_some(),
            "task_abort must be installed for unexposed teardown"
        );
        assert!(!join.is_finished(), "precondition: parking task is live");

        let t0 = std::time::Instant::now();
        mgr.teardown_unexposed_for_test(&conn_id)
            .await
            .expect("delayed cleanup must yield Ok after map absence");
        let elapsed = t0.elapsed();
        assert!(
            !mgr.connections.lock().await.contains_key(&conn_id),
            "map entry must be absent before teardown returns"
        );
        assert!(
            removed.load(Ordering::SeqCst),
            "delayed cleanup must have removed the entry (teardown awaited it)"
        );
        assert!(
            elapsed >= Duration::from_millis(180),
            "teardown must wait for map removal and process reap; elapsed={elapsed:?}"
        );
        assert!(tokens.lookup(&token).await.is_none());
        assert!(!*waiter.availability().borrow());
        for _ in 0..200 {
            if join.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            join.is_finished(),
            "connection task must be aborted (Disconnect cannot wake pending())"
        );
        let _ = join.await;
        let _ = cleanup_task.await;
        let events = event_order.lock().unwrap().clone();
        assert_eq!(
            events,
            vec!["revoked", "map_removed", "process_reaped"],
            "expected revoke, map removal, and process reap before attempt 2"
        );
    }

    /// Stuck cleanup: short teardown timeout fails closed; attempt 2 never starts.
    /// Does not force-remove the map entry.
    #[tokio::test]
    async fn teardown_unexposed_stuck_cleanup_fails_closed_no_attempt_two() {
        use crate::acp::connection::AgentConnection;
        use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker};
        use crate::acp::delegation::lease::CompanionLeaseRegistry;
        use crate::acp::delegation::listener::{TokenEntry, TokenRegistry};
        use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
        use crate::acp::delegation::types::DelegationError;
        use crate::acp::types::ConnectionStatus;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct EmptyLookup;
        #[async_trait::async_trait]
        impl ConversationDepthLookup for EmptyLookup {
            async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
                Ok(None)
            }
        }
        struct NoQuestions;
        #[async_trait::async_trait]
        impl SessionQuestionAccess for NoQuestions {
            async fn register_question(
                &self,
                _parent_connection_id: &str,
                _questions: Vec<QuestionSpec>,
            ) -> Option<RegisteredQuestion> {
                None
            }
            async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}
            async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
        }

        let mgr = ConnectionManager::new();
        let leases = Arc::new(CompanionLeaseRegistry::default());
        let tokens = Arc::new(TokenRegistry::default());
        let token = "stuck-teardown-tok".to_string();
        tokens
            .register(
                token.clone(),
                TokenEntry::legacy("stuck-1", PathBuf::from("/tmp")),
            )
            .await;
        let _waiter = leases.register(&token).await;

        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        mgr.install_delegation(crate::acp::connection::DelegationInjection {
            broker,
            continuation_coordinator: std::sync::Weak::new(),
            parent_connection_exit_causes: Arc::new(
                crate::acp::connection::ParentConnectionExitCauses::default(),
            ),
            tokens: Arc::clone(&tokens),
            leases: Arc::clone(&leases),
            socket_path: PathBuf::from("/tmp/codeg-test.sock"),
            agent_availability: Arc::new(AllAgentsAvailable),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
            questions: Arc::new(NoQuestions)
                as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            plan_approvals: no_plan_approvals(),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        });

        let conn_id = "stuck-1".to_string();
        let mut state =
            SessionState::new(conn_id.clone(), AgentType::Codex, None, "test".into(), None);
        state.delegation_token = Some(token.clone());
        state.status = ConnectionStatus::Connecting;
        let connection_incarnation = state.connection_incarnation.clone();
        let tool_lease_registry = state.tool_lease_registry.clone();
        let state = Arc::new(RwLock::new(state));
        let (tx, _rx, _liveness_rx) = connection_channel(4);
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = codeg_plan_for_late_close();
        let (spawn_config, observed_config) = matching_config_pair(
            "agent",
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );

        let join = tokio::spawn(async move {
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        let abort = join.abort_handle();

        // No cleanup task: map entry stays forever (stuck cleanup).
        mgr.connections.lock().await.insert(
            conn_id.clone(),
            AgentConnection {
                id: conn_id.clone(),
                agent_type: AgentType::Codex,
                status: ConnectionStatus::Connecting,
                owner_window_label: "test".into(),
                owner_operation_id: None,
                ownership_generation: 0,
                connection_incarnation,
                tool_lease_registry,
                parent_connection_id: None,
                cmd_tx: tx,
                control_tx: test_control_sender(),
                task_abort: Some(abort),
                state: Arc::clone(&state),
                emitter: EventEmitter::Noop,
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell,
                route_plan,
                origin: DelegationConnectionOrigin::Root,
                route_preference: None,
                route_capability:
                    crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
                child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            },
        );

        // Production root RouteSpecific branch: teardown must succeed before attempt 2.
        // Short per-call waits keep the test deterministic without global overrides.
        let attempt_two_starts = Arc::new(AtomicUsize::new(0));
        let attempt_n = Arc::clone(&attempt_two_starts);
        let mut attempt = 1u8;
        let bootstrap = RouteBootstrapOutcome::RouteSpecific(
            RouteDegradedReason::CompanionInitializationFailed,
        );
        let outcome = match bootstrap {
            RouteBootstrapOutcome::RouteSpecific(reason) if attempt == 1 => {
                match mgr
                    .teardown_unexposed_for_test_with_waits(
                        &conn_id,
                        Duration::from_millis(40),
                        Duration::from_millis(20),
                    )
                    .await
                {
                    Ok(()) => {
                        // Would start attempt 2 only after success.
                        attempt = 2;
                        attempt_n.fetch_add(1, Ordering::SeqCst);
                        let _ = reason;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            _ => panic!("expected RouteSpecific"),
        };

        assert!(
            matches!(outcome, Err(AcpError::ProcessExited)),
            "stuck cleanup must fail closed with ProcessExited; got {outcome:?}"
        );
        assert_eq!(
            attempt_two_starts.load(Ordering::SeqCst),
            0,
            "attempt 2 must never start when teardown fails"
        );
        assert_eq!(attempt, 1, "attempt counter must stay at 1");
        assert!(
            mgr.connections.lock().await.contains_key(&conn_id),
            "must not force-remove a stuck map entry"
        );
        // Token/lease revoke and task abort still ran.
        assert!(tokens.lookup(&token).await.is_none());
        for _ in 0..200 {
            if join.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(join.is_finished(), "task abort must still be requested");

        let _ = join.await;
        mgr.connections.lock().await.remove(&conn_id);
    }

    /// Fallback policy still at most two attempts; teardown completes before attempt 2.
    #[tokio::test]
    async fn safe_fallback_records_teardown_before_attempt_two() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let sequence = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seq = Arc::clone(&sequence);
        let attempt_n = Arc::new(AtomicUsize::new(0));

        // Stand-in for production: attempt1 RouteSpecific → teardown log → attempt2.
        let outcomes: [Result<String, RouteBootstrapOutcome>; 2] = [
            Err(RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed,
            )),
            Ok("native-connection".into()),
        ];
        let mut outcomes = outcomes.into_iter();
        let mut plans = Vec::new();
        let request = root_codeg_request();
        let mut plan = request.plan.clone();
        let origin = request.origin;

        for attempt in 1u8..=2 {
            attempt_n.fetch_add(1, Ordering::SeqCst);
            seq.lock().unwrap().push(format!("attempt_{attempt}_start"));
            plans.push(plan.clone());
            match outcomes.next().unwrap() {
                Ok(id) => {
                    seq.lock().unwrap().push(format!("attempt_{attempt}_ready"));
                    assert_eq!(id, "native-connection");
                    assert_eq!(attempt, 2);
                    break;
                }
                Err(RouteBootstrapOutcome::RouteSpecific(reason))
                    if origin == DelegationConnectionOrigin::Root && attempt == 1 =>
                {
                    seq.lock().unwrap().push("teardown_start".into());
                    // Production would await map absence here; record the gate.
                    seq.lock().unwrap().push("teardown_map_absent".into());
                    seq.lock().unwrap().push("teardown_done".into());
                    plan = safe_native_fallback(&plan, reason);
                    continue;
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
        }

        let events = sequence.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "attempt_1_start".to_string(),
                "teardown_start".to_string(),
                "teardown_map_absent".to_string(),
                "teardown_done".to_string(),
                "attempt_2_start".to_string(),
                "attempt_2_ready".to_string(),
            ]
        );
        assert_eq!(attempt_n.load(Ordering::SeqCst), 2);
        assert_eq!(
            plans[1].source,
            crate::acp::delegation::route::DelegationRouteSource::SafeFallback
        );
    }

    struct CleanupGateStore {
        inner: Arc<crate::acp::delegation::continuation::store::InMemoryContinuationStore>,
        gate: tokio::sync::Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                tokio::sync::oneshot::Receiver<()>,
            )>,
        >,
        fail_active_load: bool,
    }

    #[async_trait::async_trait]
    impl crate::acp::delegation::continuation::store::ContinuationStore for CleanupGateStore {
        async fn insert_arming(
            &self,
            new: crate::acp::delegation::continuation::store::NewContinuation,
        ) -> Result<
            crate::acp::delegation::continuation::store::ContinuationRecord,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner.insert_arming(new).await
        }

        async fn load(
            &self,
            continuation_id: &str,
        ) -> Result<
            Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner.load(continuation_id).await
        }

        async fn load_active_for_conversation(
            &self,
            conversation_id: i32,
        ) -> Result<
            Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            if let Some((entered, release)) = self.gate.lock().await.take() {
                let _ = entered.send(());
                let _ = release.await;
            }
            if self.fail_active_load {
                return Err(
                    crate::acp::delegation::continuation::store::ContStoreError::InvalidRecord(
                        "injected stop read failure".to_string(),
                    ),
                );
            }
            self.inner
                .load_active_for_conversation(conversation_id)
                .await
        }

        async fn list_non_terminal(
            &self,
        ) -> Result<
            Vec<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner.list_non_terminal().await
        }

        async fn cas_transition(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: crate::acp::delegation::continuation::types::ContinuationState,
            patch: crate::acp::delegation::continuation::store::ContinuationPatch,
        ) -> Result<
            Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner
                .cas_transition(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    patch,
                )
                .await
        }

        async fn cas_claim_cleanup(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: crate::acp::delegation::continuation::types::ContinuationState,
        ) -> Result<
            Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner
                .cas_claim_cleanup(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                )
                .await
        }

        async fn cas_fail_and_cancel_parent(
            &self,
            continuation_id: &str,
            generation: u64,
            expected_version: u64,
            expected_state: crate::acp::delegation::continuation::types::ContinuationState,
            failure_code: crate::acp::delegation::continuation::types::ContinuationFailureCode,
            finished_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<
            Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner
                .cas_fail_and_cancel_parent(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    failure_code,
                    finished_at,
                )
                .await
        }

        async fn matches_admitted_marker(
            &self,
            conversation_id: i32,
            marker: &str,
        ) -> Result<bool, crate::acp::delegation::continuation::store::ContStoreError> {
            self.inner
                .matches_admitted_marker(conversation_id, marker)
                .await
        }

        async fn load_latest_failure_for_conversation(
            &self,
            conversation_id: i32,
        ) -> Result<
            Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
            crate::acp::delegation::continuation::store::ContStoreError,
        > {
            self.inner
                .load_latest_failure_for_conversation(conversation_id)
                .await
        }
    }

    struct CleanupEmptyDepth;

    #[async_trait::async_trait]
    impl crate::acp::delegation::broker::ConversationDepthLookup for CleanupEmptyDepth {
        async fn parent_of(
            &self,
            _id: i32,
        ) -> Result<Option<i32>, crate::acp::delegation::types::DelegationError> {
            Ok(None)
        }
    }

    fn cleanup_new_continuation(
        id: &str,
    ) -> crate::acp::delegation::continuation::store::NewContinuation {
        let now = chrono::Utc::now();
        crate::acp::delegation::continuation::store::NewContinuation {
            continuation_id: id.to_string(),
            parent_conversation_id: 1,
            parent_session_id: "session".to_string(),
            parent_connection_id: "cleanup-parent".to_string(),
            parent_turn_generation: 1,
            task_ids: crate::acp::delegation::continuation::types::ContinuationTaskIds(vec![
                "task-1".to_string(),
            ]),
            armed_at: now,
            wake_at: now,
            internal_prompt_id: format!("prompt-{id}"),
            internal_prompt_marker: format!("marker-{id}"),
        }
    }

    async fn install_cleanup_connection(
        manager: &Arc<ConnectionManager>,
        store: Arc<dyn crate::acp::delegation::continuation::store::ContinuationStore>,
    ) -> (
        tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
        tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionControl>,
        Arc<crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator>,
    ) {
        let (cmd_tx, cmd_rx, _) = connection_channel(4);
        let (control_tx, control_rx, _) = connection_channel(4);
        let mut connection = fake_connection("cleanup-parent", Some(1));
        connection.cmd_tx = cmd_tx;
        connection.control_tx = control_tx;
        manager
            .connections
            .lock()
            .await
            .insert("cleanup-parent".to_string(), connection);
        manager.install_continuation_store(store.clone());

        let broker = Arc::new(crate::acp::delegation::broker::DelegationBroker::new(
            Arc::new(crate::acp::delegation::spawner::mock::MockSpawner::default())
                as Arc<dyn crate::acp::delegation::spawner::ConnectionSpawner>,
            Arc::new(CleanupEmptyDepth)
                as Arc<dyn crate::acp::delegation::broker::ConversationDepthLookup>,
        ));
        let metrics = Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default());
        let coordinator = Arc::new(
            crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator::new(
                store,
                broker.clone(),
                metrics.clone(),
                Arc::new(crate::acp::delegation::continuation::coordinator::ManagerContinuationPort::new(manager.clone())),
                Arc::new(crate::acp::delegation::continuation::coordinator::SystemContinuationClock::new()),
            ),
        );
        let tokens = Arc::new(
            crate::acp::delegation::listener::TokenRegistry::with_continuation_coordinator(
                coordinator.clone(),
            ),
        );
        manager.install_delegation(crate::acp::connection::DelegationInjection {
            broker,
            tokens,
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: std::path::PathBuf::from("cleanup.sock"),
            agent_availability: Arc::new(AllAgentsAvailable),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
            questions: Arc::new(ConnectionManagerQuestionLookup {
                manager: manager.clone(),
            }),
            plan_approvals: no_plan_approvals(),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics,
            continuation_coordinator: Arc::downgrade(&coordinator),
            parent_connection_exit_causes: Arc::new(
                crate::acp::connection::ParentConnectionExitCauses::default(),
            ),
        });
        (cmd_rx, control_rx, coordinator)
    }

    async fn install_shared_cleanup_turn(
        manager: &Arc<ConnectionManager>,
        conversation_id: i32,
        folder_id: i32,
        turn_id: &str,
    ) -> (SharedSessionAttachment, SharedMutationGuard) {
        let broker = manager.shared_session_broker();
        let attachment = broker
            .reserve_or_attach(shared_control_reserve_request(
                "cleanup-parent",
                conversation_id,
            ))
            .await
            .unwrap()
            .attachment;
        let (state, emitter) = manager
            .get_state_and_emitter("cleanup-parent")
            .await
            .unwrap();
        broker
            .install_registered(
                "cleanup-parent",
                attachment.generation,
                "shared-cleanup-driver".into(),
                state,
                emitter,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .unwrap();
        broker
            .mark_ready(
                "cleanup-parent",
                attachment.generation,
                "shared-cleanup-driver",
            )
            .await
            .unwrap();
        let guard = SharedMutationGuard {
            connection_id: "cleanup-parent".into(),
            generation: attachment.generation,
            lease_id: attachment.lease_id.clone(),
        };
        let admission = broker
            .enqueue_prompt(SharedPromptRequest {
                guard: guard.clone(),
                client_instance_id: "shared-control-client".into(),
                client_request_id: format!("shared-stop-prompt-{turn_id}"),
                blocks: vec![PromptInputBlock::Text {
                    text: "shared stop".into(),
                }],
                folder_id: Some(folder_id),
                conversation_id: Some(conversation_id),
                client_message_id: format!("shared-stop-message-{turn_id}"),
                capture: None,
                submitted_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        broker
            .mark_prompt_admission_published(
                "cleanup-parent",
                attachment.generation,
                &admission.queue_item_id,
            )
            .await
            .unwrap();
        assert!(matches!(
            broker
                .claim_dispatchable_head(
                    "cleanup-parent",
                    attachment.generation,
                    turn_id,
                    &SharedRuntimeWorkSnapshot {
                        event_seq: 0,
                        status: ConnectionStatus::Connected,
                        turn_in_flight: false,
                        pending_permission_id: None,
                        pending_question_id: None,
                        pending_plan_approval_id: None,
                        continuation_wait: false,
                        active_delegations: 0,
                        background_outstanding: 0,
                        conversation_write_error: None,
                    },
                )
                .await
                .unwrap(),
            DispatchHeadDecision::Claimed(_)
        ));
        (attachment, guard)
    }

    #[tokio::test]
    async fn continuation_cleanup_stop_holds_prompt_lock_until_durable_cleanup_finishes() {
        use crate::acp::delegation::continuation::store::ContinuationStore;

        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/cleanup-stop").await;
        crate::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;
        let inner = Arc::new(
            crate::acp::delegation::continuation::store::InMemoryContinuationStore::default(),
        );
        inner
            .insert_arming(cleanup_new_continuation("gated"))
            .await
            .unwrap();
        let (entered_tx, mut entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let store = Arc::new(CleanupGateStore {
            inner: inner.clone(),
            gate: tokio::sync::Mutex::new(Some((entered_tx, release_rx))),
            fail_active_load: false,
        });
        let manager = Arc::new(ConnectionManager::new());
        let (mut cmd_rx, mut control_rx, _coordinator) =
            install_cleanup_connection(&manager, store).await;
        let stop_db = db.conn.clone();
        let stop_manager = manager.clone();
        let mut stop =
            tokio::spawn(async move { stop_manager.cancel(&stop_db, "cleanup-parent").await });
        tokio::select! {
            entered = &mut entered_rx => entered.expect("stop entered continuation cleanup"),
            result = &mut stop => panic!("stop completed before durable cleanup: {result:?}"),
        }

        let prompt_db = Arc::new(crate::db::AppDatabase {
            conn: db.conn.clone(),
        });
        let prompt_manager = manager.clone();
        let prompt = tokio::spawn(async move {
            prompt_manager
                .send_prompt(
                    &prompt_db,
                    "cleanup-parent",
                    vec![PromptInputBlock::Text {
                        text: "racing prompt".to_string(),
                    }],
                    None,
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !prompt.is_finished(),
            "external prompt passed prompt_lock early"
        );

        release_tx.send(()).unwrap();
        stop.await.unwrap().unwrap();
        prompt.await.unwrap().unwrap();
        assert!(matches!(
            control_rx.try_recv(),
            Ok(ConnectionControl::Cancel)
        ));
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(ConnectionCommand::Prompt { .. })
        ));
        assert_eq!(
            inner.load("gated").await.unwrap().unwrap().state,
            crate::acp::delegation::continuation::types::ContinuationState::Cancelled
        );
    }

    #[tokio::test]
    async fn shared_stop_settled_during_continuation_cleanup_never_sends_cancel() {
        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/shared-cleanup-stop").await;
        crate::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;
        let inner = Arc::new(
            crate::acp::delegation::continuation::store::InMemoryContinuationStore::default(),
        );
        let (entered_tx, mut entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let store = Arc::new(CleanupGateStore {
            inner,
            gate: tokio::sync::Mutex::new(Some((entered_tx, release_rx))),
            fail_active_load: false,
        });
        let manager = Arc::new(ConnectionManager::new());
        let (_cmd_rx, mut control_rx, _coordinator) =
            install_cleanup_connection(&manager, store).await;
        let (attachment, guard) =
            install_shared_cleanup_turn(&manager, 1, folder_id, "shared-cleanup-turn").await;
        let broker = manager.shared_session_broker();

        let stop_db = db.conn.clone();
        let stop_manager = manager.clone();
        let mut stop = tokio::spawn(async move {
            stop_manager
                .stop_shared_turn(
                    &stop_db,
                    SharedStopRequest {
                        guard,
                        turn_id: "shared-cleanup-turn".into(),
                    },
                )
                .await
        });
        tokio::select! {
            entered = &mut entered_rx => entered.expect("stop entered continuation cleanup"),
            result = &mut stop => panic!("stop completed before cleanup gate: {result:?}"),
        }
        broker
            .settle_active_turn(
                "cleanup-parent",
                attachment.generation,
                "shared-cleanup-driver",
                "end_turn",
            )
            .await
            .unwrap();
        release_tx.send(()).unwrap();

        assert!(matches!(
            stop.await.unwrap(),
            Err(AcpError::Shared(SharedSessionError::StaleTurn))
        ));
        assert!(matches!(
            control_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn shared_stop_receiver_closes_during_cleanup_releases_claim_for_retry() {
        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/shared-closed-stop").await;
        crate::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;
        let inner = Arc::new(
            crate::acp::delegation::continuation::store::InMemoryContinuationStore::default(),
        );
        let (entered_tx, mut entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let store = Arc::new(CleanupGateStore {
            inner,
            gate: tokio::sync::Mutex::new(Some((entered_tx, release_rx))),
            fail_active_load: false,
        });
        let manager = Arc::new(ConnectionManager::new());
        let (_cmd_rx, control_rx, _coordinator) = install_cleanup_connection(&manager, store).await;
        let (attachment, guard) =
            install_shared_cleanup_turn(&manager, 1, folder_id, "shared-closed-turn").await;
        let broker = manager.shared_session_broker();

        let stop_db = db.conn.clone();
        let stop_manager = manager.clone();
        let stop_guard = guard.clone();
        let mut stop = tokio::spawn(async move {
            stop_manager
                .stop_shared_turn(
                    &stop_db,
                    SharedStopRequest {
                        guard: stop_guard,
                        turn_id: "shared-closed-turn".into(),
                    },
                )
                .await
        });
        tokio::select! {
            entered = &mut entered_rx => entered.expect("stop entered continuation cleanup"),
            result = &mut stop => panic!("stop completed before cleanup gate: {result:?}"),
        }
        drop(control_rx);
        release_tx.send(()).unwrap();

        assert!(matches!(stop.await.unwrap(), Err(AcpError::ProcessExited)));
        let active = broker
            .diagnostic_for_connection(&attachment.connection_id)
            .await
            .unwrap()
            .active_turn
            .unwrap();
        assert_eq!(active.turn_id, "shared-closed-turn");
        assert!(!active.stop_requested, "definite failure must reopen stop");

        let (retry_tx, mut retry_rx, _) = connection_channel(4);
        manager
            .connections
            .lock()
            .await
            .get_mut("cleanup-parent")
            .unwrap()
            .control_tx = retry_tx;
        manager
            .stop_shared_turn(
                &db.conn,
                SharedStopRequest {
                    guard,
                    turn_id: "shared-closed-turn".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(retry_rx.try_recv(), Ok(ConnectionControl::Cancel)));
        assert!(matches!(
            retry_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(
            broker
                .diagnostic_for_connection(&attachment.connection_id)
                .await
                .unwrap()
                .active_turn
                .unwrap()
                .stop_requested
        );
    }

    #[tokio::test]
    async fn continuation_cleanup_stop_store_failure_still_dispatches_cancel_and_leaves_gate() {
        use crate::acp::delegation::continuation::store::ContinuationStore;

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/cleanup-failure").await;
        crate::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;
        let inner = Arc::new(
            crate::acp::delegation::continuation::store::InMemoryContinuationStore::default(),
        );
        inner
            .insert_arming(cleanup_new_continuation("unresolved"))
            .await
            .unwrap();
        let store = Arc::new(CleanupGateStore {
            inner: inner.clone(),
            gate: tokio::sync::Mutex::new(None),
            fail_active_load: true,
        });
        let manager = Arc::new(ConnectionManager::new());
        let (_cmd_rx, mut control_rx, _coordinator) =
            install_cleanup_connection(&manager, store).await;

        let error = manager
            .cancel(&db.conn, "cleanup-parent")
            .await
            .expect_err("persistence error must be surfaced after explicit Cancel");
        assert!(error.to_string().contains("injected stop read failure"));
        assert!(matches!(
            control_rx.try_recv(),
            Ok(ConnectionControl::Cancel)
        ));
        assert_eq!(
            inner.load("unresolved").await.unwrap().unwrap().state,
            crate::acp::delegation::continuation::types::ContinuationState::Arming
        );
    }

    #[tokio::test]
    async fn continuation_cleanup_stop_persistence_failure_fences_queued_attempt_zero_admission() {
        use crate::acp::connection::SuspensionAck;
        use crate::acp::delegation::continuation::coordinator::{
            ContinuationPromptRequest, JoinArmOutcome, JoinArmRequest, ParentContinuationPort,
            ParentTurnSnapshot, PromptAdmissionResult, SuspendRequest, SystemContinuationClock,
        };
        use crate::acp::delegation::continuation::store::ContinuationStore;
        use crate::acp::delegation::continuation::types::{
            ContinuationState, ContinuationWaitingProjection,
        };
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        struct QueuedAdmitPort {
            manager: Arc<ConnectionManager>,
            entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ParentContinuationPort for QueuedAdmitPort {
            async fn snapshot_parent(
                &self,
                connection_id: &str,
            ) -> Result<
                ParentTurnSnapshot,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                Ok(ParentTurnSnapshot {
                    connection_id: connection_id.into(),
                    conversation_id: 1,
                    session_id: "session".into(),
                    turn_generation: 1,
                    turn_in_flight: true,
                })
            }

            async fn suspend_parent(
                &self,
                request: SuspendRequest,
            ) -> Result<
                SuspensionAck,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                Ok(SuspensionAck {
                    continuation_id: request.continuation_id,
                    parent_turn_generation: request.parent_turn_generation,
                })
            }

            async fn admit_continuation(
                &self,
                request: ContinuationPromptRequest,
            ) -> Result<
                PromptAdmissionResult,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                self.calls.fetch_add(1, Ordering::SeqCst);
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
                // Contends for the same prompt_lock Stop holds through cleanup.
                self.manager.admit_delegation_continuation(request).await
            }

            async fn publish_waiting(
                &self,
                _connection_id: &str,
                _waiting: Option<ContinuationWaitingProjection>,
            ) -> Result<(), crate::acp::delegation::continuation::coordinator::ContinuationError>
            {
                Ok(())
            }

            async fn publish_failure(
                &self,
                _connection_id: &str,
                _code: crate::acp::delegation::continuation::types::ContinuationFailureCode,
            ) -> Result<(), crate::acp::delegation::continuation::coordinator::ContinuationError>
            {
                Ok(())
            }
        }

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/cleanup-queued").await;
        crate::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;
        let inner = Arc::new(
            crate::acp::delegation::continuation::store::InMemoryContinuationStore::default(),
        );
        let (stop_entered_tx, stop_entered_rx) = tokio::sync::oneshot::channel();
        let (stop_release_tx, stop_release_rx) = tokio::sync::oneshot::channel();
        let store = Arc::new(CleanupGateStore {
            inner: inner.clone(),
            gate: tokio::sync::Mutex::new(Some((stop_entered_tx, stop_release_rx))),
            fail_active_load: true,
        });
        let manager = Arc::new(ConnectionManager::new());
        let (cmd_tx, mut cmd_rx, _) = connection_channel(4);
        let (control_tx, mut control_rx, _) = connection_channel(4);
        let mut connection = fake_connection("cleanup-parent", Some(1));
        connection.cmd_tx = cmd_tx;
        connection.control_tx = control_tx;
        {
            let mut state = connection.state.write().await;
            state.external_id = Some("session".into());
            state.parent_turn_generation = 1;
            state.last_suspended_turn_generation = Some(1);
            state.turn_in_flight = false;
            state.active_turn_generation = None;
        }
        manager
            .connections
            .lock()
            .await
            .insert("cleanup-parent".to_string(), connection);
        manager.install_continuation_store(store.clone());

        let broker = Arc::new(crate::acp::delegation::broker::DelegationBroker::new(
            Arc::new(crate::acp::delegation::spawner::mock::MockSpawner::default())
                as Arc<dyn crate::acp::delegation::spawner::ConnectionSpawner>,
            Arc::new(CleanupEmptyDepth)
                as Arc<dyn crate::acp::delegation::broker::ConversationDepthLookup>,
        ));
        broker
            .seed_live_task_for_test("cleanup-parent", "task-1")
            .await;
        let metrics = Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (admit_release_tx, admit_release_rx) = tokio::sync::oneshot::channel();
        let port = Arc::new(QueuedAdmitPort {
            manager: manager.clone(),
            entered: Mutex::new(Some(entered_tx)),
            release: tokio::sync::Mutex::new(Some(admit_release_rx)),
            calls: AtomicUsize::new(0),
        });
        let coordinator = Arc::new(
            crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator::new(
                store,
                broker.clone(),
                metrics.clone(),
                port.clone() as Arc<dyn ParentContinuationPort>,
                Arc::new(SystemContinuationClock::new()),
            ),
        );
        let tokens = Arc::new(
            crate::acp::delegation::listener::TokenRegistry::with_continuation_coordinator(
                coordinator.clone(),
            ),
        );
        manager.install_delegation(crate::acp::connection::DelegationInjection {
            broker: broker.clone(),
            tokens,
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: std::path::PathBuf::from("cleanup-queued.sock"),
            agent_availability: Arc::new(AllAgentsAvailable),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
            questions: Arc::new(ConnectionManagerQuestionLookup {
                manager: manager.clone(),
            }),
            plan_approvals: no_plan_approvals(),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics,
            continuation_coordinator: Arc::downgrade(&coordinator),
            parent_connection_exit_causes: Arc::new(
                crate::acp::connection::ParentConnectionExitCauses::default(),
            ),
        });

        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "cleanup-parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
                transferred_wait_rx: None,
                foreground_release: {
                    let (owner, waiter) =
                        crate::acp::delegation::continuation::foreground_mcp_release_fence();
                    owner.frame_flushed();
                    waiter
                },
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        completion.await.unwrap().unwrap();
        broker
            .complete_call(
                "task-1",
                crate::acp::delegation::types::DelegationOutcome::Ok(
                    crate::acp::delegation::types::DelegationSuccess {
                        text: "done".into(),
                        child_conversation_id: 99,
                        child_agent_type: AgentType::Codex,
                        turn_count: 1,
                        duration_ms: 1,
                        token_usage: None,
                    },
                ),
            )
            .await;
        entered_rx
            .await
            .expect("live worker must reach attempt-zero admission gate");

        let stop_db = db.conn.clone();
        let stop_manager = manager.clone();
        let stop =
            tokio::spawn(async move { stop_manager.cancel(&stop_db, "cleanup-parent").await });
        stop_entered_rx
            .await
            .expect("Stop must own prompt_lock and enter store cleanup");
        // Worker was gated before manager.admit; release it so it queues on the
        // lock Stop currently holds.
        admit_release_tx.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(
            !stop.is_finished(),
            "Stop must still hold prompt_lock during cleanup"
        );
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "admission must not complete while Stop holds prompt_lock"
        );

        stop_release_tx.send(()).unwrap();
        let stop_result = stop.await.unwrap();
        assert!(
            stop_result.is_err(),
            "stop persistence failure must surface: {stop_result:?}"
        );
        assert!(stop_result
            .unwrap_err()
            .to_string()
            .contains("injected stop read failure"));
        assert!(matches!(
            control_rx.try_recv(),
            Ok(ConnectionControl::Cancel)
        ));
        for _ in 0..30 {
            if coordinator.worker_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(coordinator.worker_count(), 0);
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "queued admission must not send a hidden prompt after Stop releases prompt_lock"
        );
        let durable = inner.load(&continuation_id).await.unwrap().unwrap();
        assert_ne!(durable.state, ContinuationState::Completed);
        assert!(durable.prompt_admitted_at.is_none());
        assert!(port.calls.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn continuation_cleanup_manager_stop_after_admission_before_completed_uses_ordinary_cancel(
    ) {
        use crate::acp::connection::SuspensionAck;
        use crate::acp::delegation::continuation::coordinator::{
            ContinuationPromptRequest, JoinArmOutcome, JoinArmRequest, ParentContinuationPort,
            ParentTurnSnapshot, PromptAdmissionResult, SuspendRequest, SystemContinuationClock,
        };
        use crate::acp::delegation::continuation::store::{
            ContinuationStore, InMemoryContinuationStore,
        };
        use crate::acp::delegation::continuation::types::{
            ContinuationState, ContinuationWaitingProjection,
        };
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        /// Stamps durable admission through the real manager path, then blocks
        /// before returning so Stop races post-admission / pre-Completed.
        struct ManagerPostAdmissionPort {
            manager: Arc<ConnectionManager>,
            entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ParentContinuationPort for ManagerPostAdmissionPort {
            async fn snapshot_parent(
                &self,
                connection_id: &str,
            ) -> Result<
                ParentTurnSnapshot,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                Ok(ParentTurnSnapshot {
                    connection_id: connection_id.into(),
                    conversation_id: 1,
                    session_id: "session".into(),
                    turn_generation: 1,
                    turn_in_flight: true,
                })
            }

            async fn suspend_parent(
                &self,
                request: SuspendRequest,
            ) -> Result<
                SuspensionAck,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                Ok(SuspensionAck {
                    continuation_id: request.continuation_id,
                    parent_turn_generation: request.parent_turn_generation,
                })
            }

            async fn admit_continuation(
                &self,
                request: ContinuationPromptRequest,
            ) -> Result<
                PromptAdmissionResult,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                self.calls.fetch_add(1, Ordering::SeqCst);
                // Real durable admission + Prompt enqueue under prompt_lock.
                let result = self.manager.admit_delegation_continuation(request).await?;
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
                Ok(result)
            }

            async fn publish_waiting(
                &self,
                _connection_id: &str,
                _waiting: Option<ContinuationWaitingProjection>,
            ) -> Result<(), crate::acp::delegation::continuation::coordinator::ContinuationError>
            {
                Ok(())
            }

            async fn publish_failure(
                &self,
                _connection_id: &str,
                _code: crate::acp::delegation::continuation::types::ContinuationFailureCode,
            ) -> Result<(), crate::acp::delegation::continuation::coordinator::ContinuationError>
            {
                Ok(())
            }
        }

        /// Notifies when the worker CASes Completed so the race test avoids
        /// yield polling.
        struct CompletedNotifyStore {
            inner: Arc<InMemoryContinuationStore>,
            completed: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            cancelled: AtomicUsize,
        }

        impl CompletedNotifyStore {
            fn new(
                inner: Arc<InMemoryContinuationStore>,
            ) -> (Arc<Self>, tokio::sync::oneshot::Receiver<()>) {
                let (tx, rx) = tokio::sync::oneshot::channel();
                (
                    Arc::new(Self {
                        inner,
                        completed: Mutex::new(Some(tx)),
                        cancelled: AtomicUsize::new(0),
                    }),
                    rx,
                )
            }
        }

        #[async_trait]
        impl ContinuationStore for CompletedNotifyStore {
            async fn insert_arming(
                &self,
                new: crate::acp::delegation::continuation::store::NewContinuation,
            ) -> Result<
                crate::acp::delegation::continuation::store::ContinuationRecord,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner.insert_arming(new).await
            }

            async fn load(
                &self,
                continuation_id: &str,
            ) -> Result<
                Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner.load(continuation_id).await
            }

            async fn load_active_for_conversation(
                &self,
                conversation_id: i32,
            ) -> Result<
                Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner
                    .load_active_for_conversation(conversation_id)
                    .await
            }

            async fn list_non_terminal(
                &self,
            ) -> Result<
                Vec<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner.list_non_terminal().await
            }

            async fn cas_transition(
                &self,
                continuation_id: &str,
                generation: u64,
                expected_version: u64,
                expected_state: ContinuationState,
                patch: crate::acp::delegation::continuation::store::ContinuationPatch,
            ) -> Result<
                Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                if patch.state == ContinuationState::Cancelled {
                    self.cancelled.fetch_add(1, Ordering::SeqCst);
                }
                let result = self
                    .inner
                    .cas_transition(
                        continuation_id,
                        generation,
                        expected_version,
                        expected_state,
                        patch.clone(),
                    )
                    .await?;
                if result.is_some() && patch.state == ContinuationState::Completed {
                    if let Some(tx) = self
                        .completed
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take()
                    {
                        let _ = tx.send(());
                    }
                }
                Ok(result)
            }

            async fn cas_claim_cleanup(
                &self,
                continuation_id: &str,
                generation: u64,
                expected_version: u64,
                expected_state: ContinuationState,
            ) -> Result<
                Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner
                    .cas_claim_cleanup(
                        continuation_id,
                        generation,
                        expected_version,
                        expected_state,
                    )
                    .await
            }

            async fn cas_fail_and_cancel_parent(
                &self,
                continuation_id: &str,
                generation: u64,
                expected_version: u64,
                expected_state: ContinuationState,
                failure_code: crate::acp::delegation::continuation::types::ContinuationFailureCode,
                finished_at: chrono::DateTime<chrono::Utc>,
            ) -> Result<
                Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner
                    .cas_fail_and_cancel_parent(
                        continuation_id,
                        generation,
                        expected_version,
                        expected_state,
                        failure_code,
                        finished_at,
                    )
                    .await
            }

            async fn matches_admitted_marker(
                &self,
                conversation_id: i32,
                marker: &str,
            ) -> Result<bool, crate::acp::delegation::continuation::store::ContStoreError>
            {
                self.inner
                    .matches_admitted_marker(conversation_id, marker)
                    .await
            }

            async fn load_latest_failure_for_conversation(
                &self,
                conversation_id: i32,
            ) -> Result<
                Option<crate::acp::delegation::continuation::store::ContinuationRecord>,
                crate::acp::delegation::continuation::store::ContStoreError,
            > {
                self.inner
                    .load_latest_failure_for_conversation(conversation_id)
                    .await
            }
        }

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/cleanup-admitted").await;
        crate::db::test_helpers::seed_conversation(&db, folder_id, AgentType::Codex).await;
        let inner = Arc::new(InMemoryContinuationStore::default());
        let (store, completed_rx) = CompletedNotifyStore::new(inner.clone());
        let manager = Arc::new(ConnectionManager::new());
        let (cmd_tx, mut cmd_rx, _) = connection_channel(4);
        let (control_tx, mut control_rx, _) = connection_channel(4);
        let mut connection = fake_connection("cleanup-parent", Some(1));
        connection.cmd_tx = cmd_tx;
        connection.control_tx = control_tx;
        {
            let mut state = connection.state.write().await;
            state.external_id = Some("session".into());
            state.parent_turn_generation = 1;
            state.last_suspended_turn_generation = Some(1);
            state.turn_in_flight = false;
            state.active_turn_generation = None;
        }
        manager
            .connections
            .lock()
            .await
            .insert("cleanup-parent".to_string(), connection);
        manager.install_continuation_store(store.clone() as Arc<dyn ContinuationStore>);

        let broker = Arc::new(crate::acp::delegation::broker::DelegationBroker::new(
            Arc::new(crate::acp::delegation::spawner::mock::MockSpawner::default())
                as Arc<dyn crate::acp::delegation::spawner::ConnectionSpawner>,
            Arc::new(CleanupEmptyDepth)
                as Arc<dyn crate::acp::delegation::broker::ConversationDepthLookup>,
        ));
        broker
            .seed_live_task_for_test("cleanup-parent", "task-1")
            .await;
        let metrics = Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (admit_release_tx, admit_release_rx) = tokio::sync::oneshot::channel();
        let port = Arc::new(ManagerPostAdmissionPort {
            manager: manager.clone(),
            entered: Mutex::new(Some(entered_tx)),
            release: tokio::sync::Mutex::new(Some(admit_release_rx)),
            calls: AtomicUsize::new(0),
        });
        let coordinator = Arc::new(
            crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator::new(
                store.clone() as Arc<dyn ContinuationStore>,
                broker.clone(),
                metrics.clone(),
                port.clone() as Arc<dyn ParentContinuationPort>,
                Arc::new(SystemContinuationClock::new()),
            ),
        );
        let tokens = Arc::new(
            crate::acp::delegation::listener::TokenRegistry::with_continuation_coordinator(
                coordinator.clone(),
            ),
        );
        manager.install_delegation(crate::acp::connection::DelegationInjection {
            broker: broker.clone(),
            tokens,
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: std::path::PathBuf::from("cleanup-admitted.sock"),
            agent_availability: Arc::new(AllAgentsAvailable),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
            questions: Arc::new(ConnectionManagerQuestionLookup {
                manager: manager.clone(),
            }),
            plan_approvals: no_plan_approvals(),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics,
            continuation_coordinator: Arc::downgrade(&coordinator),
            parent_connection_exit_causes: Arc::new(
                crate::acp::connection::ParentConnectionExitCauses::default(),
            ),
        });

        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "cleanup-parent".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
                transferred_wait_rx: None,
                foreground_release: {
                    let (owner, waiter) =
                        crate::acp::delegation::continuation::foreground_mcp_release_fence();
                    owner.frame_flushed();
                    waiter
                },
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        completion.await.unwrap().unwrap();
        broker
            .complete_call(
                "task-1",
                crate::acp::delegation::types::DelegationOutcome::Ok(
                    crate::acp::delegation::types::DelegationSuccess {
                        text: "done".into(),
                        child_conversation_id: 99,
                        child_agent_type: AgentType::Codex,
                        turn_count: 1,
                        duration_ms: 1,
                        token_usage: None,
                    },
                ),
            )
            .await;
        entered_rx
            .await
            .expect("live worker must durable-admit before Completed CAS");

        let mid = inner.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(mid.state, ContinuationState::Resuming);
        assert!(mid.prompt_admitted_at.is_some());
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(ConnectionCommand::Prompt { .. })
        ));

        manager
            .cancel(&db.conn, "cleanup-parent")
            .await
            .expect("post-admission Stop must succeed via ordinary Cancel path");
        assert!(
            matches!(control_rx.try_recv(), Ok(ConnectionControl::Cancel)),
            "manager Stop must still dispatch ordinary ConnectionControl::Cancel"
        );
        assert_eq!(
            store.cancelled.load(Ordering::SeqCst),
            0,
            "post-admission Stop must not CAS Cancelled / win pre-admission cleanup"
        );
        let still = inner.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(still.state, ContinuationState::Resuming);
        assert!(still.prompt_admitted_at.is_some());

        admit_release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
            .await
            .expect("worker must finish Completed after post-admission Stop within bound")
            .expect("Completed CAS notification channel closed unexpectedly");
        let done = inner.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(done.state, ContinuationState::Completed);
        assert!(done.prompt_admitted_at.is_some());
        assert_eq!(store.cancelled.load(Ordering::SeqCst), 0);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        assert_eq!(coordinator.worker_count(), 0);
    }

    async fn apply_companion_closed(
        state: &Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
    ) {
        // Mirror post-ready lease close: only availability flips.
        state.write().await.set_delegation_available(false);
        // Apply the event path too so snapshot consumers see the same bit.
        state
            .write()
            .await
            .apply_event(&AcpEvent::DelegationAvailabilityChanged { available: false });
    }

    mod shared_dispatch {
        use super::*;
        use crate::acp::shared_session::{
            SharedMutationGuard, SharedPromptRequest, SharedQueuedPromptState,
        };

        fn queued_prompt(
            attachment: &SharedSessionAttachment,
            request_id: &str,
            text: &str,
        ) -> SharedPromptRequest {
            SharedPromptRequest {
                guard: SharedMutationGuard {
                    connection_id: attachment.connection_id.clone(),
                    generation: attachment.generation,
                    lease_id: attachment.lease_id.clone(),
                },
                client_instance_id: "dispatch-client".into(),
                client_request_id: request_id.into(),
                blocks: vec![PromptInputBlock::Text { text: text.into() }],
                folder_id: Some(9),
                conversation_id: Some(771),
                client_message_id: format!("message-{request_id}"),
                capture: None,
                submitted_at: chrono::Utc::now(),
            }
        }

        async fn ready_manager() -> (ConnectionManager, SharedSessionAttachment) {
            let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(
                FakeSharedSpawnDriver::immediate_ready(),
            ));
            let attachment = manager
                .connect_or_attach_shared(shared_launch(771, "dispatch-client").await)
                .await
                .unwrap();
            manager
                .wait_for_shared_phase(
                    &attachment.connection_id,
                    attachment.generation,
                    SharedSessionPhase::Ready,
                )
                .await
                .unwrap();
            (manager, attachment)
        }

        #[test]
        fn conversation_write_guard_preserves_recognized_codes_and_falls_back() {
            use crate::db::entities::delegation_workflow::CompletionProtocolMode;

            let recognized = [
                (
                    WorkflowStoreError::workflow_v2_retired(),
                    "workflow_v2_retired",
                ),
                (
                    WorkflowStoreError::WorkflowIdentityCorrupt {
                        source_conversation_id: 771,
                    },
                    "workflow_identity_corrupt",
                ),
                (
                    WorkflowStoreError::LegacyCompletionProtocolReadOnly,
                    "legacy_completion_protocol_read_only",
                ),
                (
                    WorkflowStoreError::UnsupportedCompletionProtocol {
                        version: 3,
                        mode: CompletionProtocolMode::V2Enforce,
                    },
                    "unsupported_completion_protocol",
                ),
            ];
            for (error, expected) in recognized {
                assert_eq!(stable_conversation_write_error(&error), expected);
            }

            for error in [
                WorkflowStoreError::Persistence("database unavailable".into()),
                WorkflowStoreError::NotFound("unknown workflow".into()),
            ] {
                assert_eq!(
                    stable_conversation_write_error(&error),
                    "session_unavailable"
                );
            }
        }

        #[tokio::test]
        async fn cancelled_enqueue_publication_is_recovered_once_before_dispatch() {
            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            let mut events = state.read().await.event_stream().subscribe();
            let publication_reached = Arc::new(tokio::sync::Barrier::new(2));
            let publication_resume = Arc::new(tokio::sync::Barrier::new(2));
            *manager
                .shared_enqueue_publication_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(SharedEnqueuePublicationHook {
                reached: publication_reached.clone(),
                resume: publication_resume.clone(),
            });

            let request = queued_prompt(&attachment, "cancelled-publication", "alpha");
            let original_manager = manager.clone_ref();
            let original_request = request.clone();
            let original = tokio::spawn(async move {
                original_manager
                    .enqueue_shared_prompt(original_request)
                    .await
            });
            publication_reached.wait().await;

            let snapshot = state.read().await.shared_runtime_work_snapshot(None);
            assert!(matches!(
                manager
                    .shared_session_broker()
                    .claim_dispatchable_head(
                        &attachment.connection_id,
                        attachment.generation,
                        "claim-before-admission-events",
                        &snapshot,
                    )
                    .await
                    .unwrap(),
                DispatchHeadDecision::Blocked
            ));
            while let Ok(envelope) = events.try_recv() {
                assert!(!matches!(
                    envelope.payload,
                    AcpEvent::PromptQueued { .. }
                        | AcpEvent::PromptQueueDepthChanged { .. }
                        | AcpEvent::PromptDispatchStarted { .. }
                ));
            }

            original.abort();
            assert!(original.await.unwrap_err().is_cancelled());
            let retry_manager = manager.clone_ref();
            let retry =
                tokio::spawn(async move { retry_manager.enqueue_shared_prompt(request).await });
            tokio::task::yield_now().await;
            assert!(!retry.is_finished());

            publication_resume.wait().await;
            retry.await.unwrap().unwrap();

            let mut admission_kinds = Vec::new();
            tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    match events.recv().await.unwrap().payload {
                        AcpEvent::PromptQueued { .. } => admission_kinds.push("queued"),
                        AcpEvent::PromptQueueDepthChanged { .. } => admission_kinds.push("depth"),
                        AcpEvent::PromptDispatchStarted { .. } => {
                            admission_kinds.push("dispatch");
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("published admission must wake the dispatcher");
            assert_eq!(admission_kinds, ["queued", "depth", "dispatch"]);

            let state = state.read().await;
            let projection = state.shared_session.as_ref().unwrap();
            assert!(projection
                .queue
                .iter()
                .all(|item| { item.client_message_id != "message-cancelled-publication" }));
        }

        #[tokio::test]
        async fn terminal_failure_invalidates_paused_admission_publication() {
            use crate::acp::shared_session::InternalPromptState;

            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            let mut events = state.read().await.event_stream().subscribe();
            let publication_reached = Arc::new(tokio::sync::Barrier::new(2));
            let publication_resume = Arc::new(tokio::sync::Barrier::new(2));
            *manager
                .shared_enqueue_publication_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(SharedEnqueuePublicationHook {
                reached: publication_reached.clone(),
                resume: publication_resume.clone(),
            });

            let request = queued_prompt(&attachment, "terminal-publication", "alpha");
            let retry_request = request.clone();
            let enqueue_manager = manager.clone_ref();
            let enqueue =
                tokio::spawn(async move { enqueue_manager.enqueue_shared_prompt(request).await });
            publication_reached.wait().await;

            let broker = manager.shared_session_broker();
            let before_failure = broker
                .diagnostic_for_connection(&attachment.connection_id)
                .await
                .unwrap();
            let queue_item_id = before_failure.queue[0].queue_item_id.clone();
            let driver_incarnation = broker
                .driver_incarnation_for_generation(&attachment.connection_id, attachment.generation)
                .await
                .unwrap()
                .unwrap();
            let failure_events = broker
                .fail_live_session(
                    &attachment.connection_id,
                    attachment.generation,
                    &driver_incarnation,
                    "session_unavailable",
                )
                .await
                .unwrap();
            manager
                .publish_shared_events(&attachment.connection_id, failure_events)
                .await
                .unwrap();
            while events.try_recv().is_ok() {}

            assert_eq!(
                broker
                    .prompt_state_for_test(&attachment.connection_id, &queue_item_id)
                    .await,
                Some(InternalPromptState::Failed)
            );
            publication_resume.wait().await;
            let _ = enqueue.await.unwrap();

            let mut late_admission_event = false;
            let mut dispatch_started = false;
            while let Ok(envelope) = events.try_recv() {
                late_admission_event |= matches!(
                    envelope.payload,
                    AcpEvent::PromptQueued { .. } | AcpEvent::PromptQueueDepthChanged { .. }
                );
                dispatch_started |=
                    matches!(envelope.payload, AcpEvent::PromptDispatchStarted { .. });
            }
            assert!(!late_admission_event);
            assert!(!dispatch_started);

            let projection = state.read().await.shared_session.clone().unwrap();
            assert!(projection.queue.is_empty());
            assert!(projection.active_turn.is_none());
            assert!(matches!(
                projection.phase,
                SharedSessionPhase::Failed { .. }
            ));
            assert_eq!(
                broker
                    .prompt_state_for_test(&attachment.connection_id, &queue_item_id)
                    .await,
                Some(InternalPromptState::Failed)
            );

            assert!(matches!(
                manager.enqueue_shared_prompt(retry_request).await,
                Err(AcpError::Shared(SharedSessionError::SessionUnavailable))
            ));
            assert!(events.try_recv().is_err());
            let snapshot = state.read().await.shared_runtime_work_snapshot(None);
            assert!(matches!(
                broker
                    .claim_dispatchable_head(
                        &attachment.connection_id,
                        attachment.generation,
                        "invalid-terminal-claim",
                        &snapshot,
                    )
                    .await
                    .unwrap(),
                DispatchHeadDecision::Blocked
            ));
        }

        #[tokio::test]
        async fn unexpected_driver_exit_fails_all_work_and_completes_cleanup_once() {
            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            state.write().await.turn_in_flight = true;
            manager
                .enqueue_shared_prompt(queued_prompt(&attachment, "exit-first", "alpha"))
                .await
                .unwrap();
            manager
                .enqueue_shared_prompt(queued_prompt(&attachment, "exit-second", "beta"))
                .await
                .unwrap();

            emit_with_state(
                &state,
                &EventEmitter::Noop,
                AcpEvent::StatusChanged {
                    status: ConnectionStatus::Disconnected,
                },
            )
            .await;

            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let phase = manager
                        .shared_session_broker()
                        .diagnostic_for_connection(&attachment.connection_id)
                        .await
                        .map(|diagnostic| diagnostic.phase);
                    if matches!(
                        phase,
                        Some(SharedSessionPhase::Failed {
                            cleanup_complete: true,
                            ..
                        })
                    ) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("unexpected exit cleanup must settle within the bound");

            let projection = manager
                .shared_session_broker()
                .diagnostic_for_connection(&attachment.connection_id)
                .await
                .unwrap();
            assert!(projection.queue.is_empty());
            assert!(projection.active_turn.is_none());
            assert!(
                !manager
                    .has_connection_map_entry_for_test(&attachment.connection_id)
                    .await
            );
            assert_eq!(manager.shared_teardown_count_for_test(), 1);
        }

        #[tokio::test]
        async fn sender_lease_release_preserves_waiting_fifo() {
            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            state.write().await.turn_in_flight = true;
            // Let any dispatcher iteration that sampled the old snapshot
            // finish against the still-empty queue before we publish work.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }

            let first = manager
                .enqueue_shared_prompt(queued_prompt(&attachment, "first", "alpha"))
                .await
                .unwrap();
            let second = manager
                .enqueue_shared_prompt(queued_prompt(&attachment, "second", "beta"))
                .await
                .unwrap();
            assert_eq!(first.state, SharedQueuedPromptState::Queued);
            assert_eq!(second.state, SharedQueuedPromptState::Queued);

            manager
                .shared_session_broker()
                .release_lease(&SharedMutationGuard {
                    connection_id: attachment.connection_id.clone(),
                    generation: attachment.generation,
                    lease_id: attachment.lease_id.clone(),
                })
                .await
                .unwrap();
            let snapshot = manager
                .shared_session_broker()
                .diagnostic_for_connection(&attachment.connection_id)
                .await
                .unwrap();
            assert_eq!(
                snapshot
                    .queue
                    .iter()
                    .map(|item| item.enqueue_seq)
                    .collect::<Vec<_>>(),
                [first.enqueue_seq, second.enqueue_seq]
            );
        }

        #[tokio::test]
        async fn dispatch_failure_emits_terminal_settlement_and_preserves_tail() {
            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            let mut events = state.read().await.event_stream().subscribe();

            manager
                .enqueue_shared_prompt(queued_prompt(&attachment, "head", "alpha"))
                .await
                .unwrap();
            manager
                .enqueue_shared_prompt(queued_prompt(&attachment, "tail", "beta"))
                .await
                .unwrap();

            let settled = tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    let envelope = events.recv().await.unwrap();
                    if matches!(
                        envelope.payload,
                        AcpEvent::SharedTurnSettled {
                            outcome: crate::acp::shared_session::SharedTurnOutcome::Failed,
                            ..
                        }
                    ) {
                        break;
                    }
                }
            })
            .await;
            assert!(
                settled.is_ok(),
                "claimed send failure must settle terminally"
            );
            let snapshot = manager
                .shared_session_broker()
                .diagnostic_for_connection(&attachment.connection_id)
                .await
                .unwrap();
            assert!(snapshot.active_turn.is_none());
            assert!(snapshot.queue.len() <= 1);
        }

        #[tokio::test]
        async fn enqueue_response_reflects_claim_that_wins_before_finalization() {
            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            state.write().await.turn_in_flight = true;
            let mut events = state.read().await.event_stream().subscribe();
            let reached = Arc::new(tokio::sync::Barrier::new(2));
            let resume = Arc::new(tokio::sync::Barrier::new(2));
            *manager
                .shared_enqueue_finalize_hook
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(SharedEnqueueFinalizeHook {
                reached: reached.clone(),
                resume: resume.clone(),
            });

            let enqueue_manager = manager.clone_ref();
            let enqueue_attachment = attachment.clone();
            let enqueue = tokio::spawn(async move {
                enqueue_manager
                    .enqueue_shared_prompt(queued_prompt(
                        &enqueue_attachment,
                        "claim-before-response",
                        "alpha",
                    ))
                    .await
            });
            reached.wait().await;

            state.write().await.turn_in_flight = false;
            manager
                .shared_session_broker()
                .notify_dispatcher(&attachment.connection_id, attachment.generation)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    if matches!(
                        events.recv().await.unwrap().payload,
                        AcpEvent::PromptDispatchStarted { .. }
                    ) {
                        break;
                    }
                }
            })
            .await
            .expect("dispatcher must claim while the enqueue response is paused");
            resume.wait().await;

            let result = enqueue.await.unwrap().unwrap();
            assert_eq!(result.state, SharedQueuedPromptState::Dispatching);
        }

        #[tokio::test]
        async fn ephemeral_record_rekeys_before_conversation_linked_is_observable() {
            let manager = ConnectionManager::new_with_shared_spawn_driver(Arc::new(
                FakeSharedSpawnDriver::immediate_ready(),
            ));
            let mut launch = shared_launch(772, "ephemeral-client").await;
            launch.key = SharedSessionKey::Ephemeral("ephemeral-772".into());
            let attachment = manager.connect_or_attach_shared(launch).await.unwrap();
            manager
                .wait_for_shared_phase(
                    &attachment.connection_id,
                    attachment.generation,
                    SharedSessionPhase::Ready,
                )
                .await
                .unwrap();
            manager
                .insert_test_connection(
                    &attachment.connection_id,
                    AgentType::Codex,
                    None,
                    EventEmitter::Noop,
                )
                .await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            let mut events = state.read().await.event_stream().subscribe();

            let mut request = queued_prompt(&attachment, "ephemeral", "link me");
            request.client_instance_id = "ephemeral-client".into();
            request.conversation_id = Some(772);
            manager.enqueue_shared_prompt(request).await.unwrap();

            let linked = tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    let envelope = events.recv().await.unwrap();
                    if matches!(
                        envelope.payload,
                        AcpEvent::ConversationLinked {
                            conversation_id: 772,
                            ..
                        }
                    ) {
                        break;
                    }
                }
            })
            .await;
            assert!(linked.is_ok(), "conversation link must be observable");
            assert!(matches!(
                manager
                    .shared_session_broker()
                    .key_for_connection_for_test(&attachment.connection_id)
                    .await,
                Some(SharedSessionKey::Conversation(772))
            ));
        }

        #[tokio::test]
        async fn shared_monitor_lock_order_does_not_deadlock() {
            let (manager, attachment) = ready_manager().await;
            let state = manager.get_state(&attachment.connection_id).await.unwrap();
            let driver_incarnation = manager
                .shared_session_broker()
                .driver_incarnation_for_generation(&attachment.connection_id, attachment.generation)
                .await
                .unwrap()
                .unwrap();
            let broker_snapshot = state.read().await.shared_runtime_work_snapshot(None);
            let map_guard = manager.connections.lock().await;
            let state_guard = state.write().await;
            let callback = tokio::time::timeout(
                Duration::from_millis(500),
                manager.shared_session_broker().reconcile_runtime_snapshot(
                    &attachment.connection_id,
                    attachment.generation,
                    &driver_incarnation,
                    &broker_snapshot,
                ),
            )
            .await;
            assert!(
                callback.is_ok(),
                "broker callbacks must not acquire SessionState or the manager map"
            );
            drop(state_guard);
            drop(map_guard);
            state.write().await.turn_in_flight = true;
            let mut attach_launch = shared_launch(771, "lock-order-client").await;
            attach_launch.request_id = "lock-order-attach".into();

            let raced = tokio::time::timeout(Duration::from_millis(500), async {
                let start = Arc::new(tokio::sync::Barrier::new(5));
                let enqueue_start = start.clone();
                let enqueue = async {
                    enqueue_start.wait().await;
                    manager
                        .enqueue_shared_prompt(queued_prompt(
                            &attachment,
                            "lock-order-prompt",
                            "alpha",
                        ))
                        .await
                };
                let attach_start = start.clone();
                let attach = async {
                    attach_start.wait().await;
                    manager.connect_or_attach_shared(attach_launch).await
                };
                let snapshot_start = start.clone();
                let snapshot = async {
                    snapshot_start.wait().await;
                    manager.get_state(&attachment.connection_id).await
                };
                let reconcile_start = start.clone();
                let reconcile = async {
                    reconcile_start.wait().await;
                    emit_with_state(
                        &state,
                        &EventEmitter::Noop,
                        AcpEvent::BackgroundActivity {
                            session_id: "lock-order-session".into(),
                            outstanding: 0,
                            turns: Vec::new(),
                            settled: Vec::new(),
                            watermark: 0,
                            detail_refetch: false,
                        },
                    )
                    .await
                };
                let release = async {
                    start.wait().await;
                };
                let (enqueue, attach, snapshot, (), ()) =
                    tokio::join!(enqueue, attach, snapshot, reconcile, release);
                enqueue.unwrap();
                attach.unwrap();
                assert!(snapshot.is_some());
            })
            .await;
            assert!(
                raced.is_ok(),
                "shared callback adapters must not reverse locks"
            );
        }
    }
}
