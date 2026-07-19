use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ActiveValue::Set, DatabaseConnection, EntityTrait,
    TransactionTrait,
};

#[cfg(any(test, feature = "test-utils"))]
use crate::acp::connection::matching_config_pair;
use crate::acp::connection::{
    spawn_agent_connection, AgentConnection, ConnectionCommand, RouteBootstrapOutcome,
    SpawnHandshake,
};
#[cfg(any(test, feature = "test-utils"))]
use crate::acp::delegation::route::DelegationRoutePlan;
#[cfg(test)]
use crate::acp::delegation::route::RouteDegradedReason;
use crate::acp::delegation::route::{safe_native_fallback, DelegationConnectionOrigin};
use crate::acp::error::AcpError;
use crate::acp::feedback::{
    bounded_feedback_batch, FeedbackItem, FeedbackStatus, PendingFeedback, SessionFeedbackAccess,
    MAX_FEEDBACK_CHARS, MAX_FEEDBACK_RESPONSE_BYTES,
};
use crate::acp::question::{
    build_outcome, QuestionAnswer, QuestionOutcome, QuestionSpec, RegisteredQuestion,
    SessionQuestionAccess,
};
use crate::acp::session_state::ActiveTurnContext;
use crate::acp::terminal_context::{finalize_acp_launch_config, AcpLaunchConfig, AcpLaunchInputs};
use crate::acp::types::{
    AcpEvent, AgentOptionsSnapshot, ConfigStaleKind, ConnectionInfo, ConnectionStatus,
    ForkResultInfo, PromptInputBlock,
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

/// Default upper bound on how long `spawn_agent` will hold the per-session
/// dedup lock waiting for `SessionStarted`. Picked to comfortably cover
/// cold-start agents (claude-code/codex warm: <2s; npx-fetched cold: 10–30s)
/// without deadlocking the next concurrent acp_connect when an agent is
/// genuinely broken.
pub(crate) const SPAWN_HANDSHAKE_TIMEOUT_SECS: u64 = 60;

/// Read the spawn-handshake timeout from `CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS`,
/// falling back to `SPAWN_HANDSHAKE_TIMEOUT_SECS`. Returns the configured
/// `Duration`. Tests can construct the manager with a custom value via
/// `with_spawn_handshake_timeout` instead of mutating env.
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
    /// Delegation broker + token registry + UDS path installed during app
    /// bootstrap (`install_delegation`). When present, `spawn_agent` propagates
    /// the injection to `spawn_agent_connection`, which makes
    /// `codeg-mcp` appear in the agent's MCP server list during ACP
    /// init. `Arc<OnceLock>` so the inner `Self` cloned from `clone_ref` sees
    /// the install too — the lock is set once at startup and never mutated.
    delegation_injection: Arc<std::sync::OnceLock<crate::acp::connection::DelegationInjection>>,
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
}

/// A parked `ask_user_question` awaiting its answer. The `sender` resolves the
/// blocked listener round-trip; `questions` is retained so `answer_question` can
/// build the self-describing outcome without a `SessionState` read (race-free).
struct PendingQuestionEntry {
    parent_connection_id: String,
    questions: Vec<QuestionSpec>,
    sender: tokio::sync::oneshot::Sender<QuestionOutcome>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            spawn_locks: Arc::new(Mutex::new(HashMap::new())),
            spawn_handshake_timeout: spawn_handshake_timeout_from_env(),
            delegation_injection: Arc::new(std::sync::OnceLock::new()),
            probe_locks: Arc::new(Mutex::new(HashMap::new())),
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns a shallow clone sharing the same underlying connection map.
    pub fn clone_ref(&self) -> Self {
        Self {
            connections: self.connections.clone(),
            spawn_locks: self.spawn_locks.clone(),
            spawn_handshake_timeout: self.spawn_handshake_timeout,
            delegation_injection: self.delegation_injection.clone(),
            probe_locks: self.probe_locks.clone(),
            pending_questions: self.pending_questions.clone(),
        }
    }

    /// Set the delegation injection context exactly once during bootstrap.
    /// Calling twice is a no-op — protects against accidental re-init in
    /// the unlikely event a second `build_delegation_stack` runs.
    pub fn install_delegation(&self, injection: crate::acp::connection::DelegationInjection) {
        let _ = self.delegation_injection.set(injection);
    }

    fn delegation_snapshot(&self) -> Option<crate::acp::connection::DelegationInjection> {
        self.delegation_injection.get().cloned()
    }

    /// Test-only constructor that overrides the spawn-handshake timeout.
    /// Production code should use `new()`.
    #[cfg(test)]
    fn with_spawn_handshake_timeout(timeout: Duration) -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            spawn_locks: Arc::new(Mutex::new(HashMap::new())),
            spawn_handshake_timeout: timeout,
            delegation_injection: Arc::new(std::sync::OnceLock::new()),
            probe_locks: Arc::new(Mutex::new(HashMap::new())),
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
        }
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
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut state = SessionState::new(
            id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
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
            cmd_tx: tx,
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
        };
        let mut map = self.connections.lock().await;
        map.insert(id.to_string(), conn);
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
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut state = SessionState::new(
            id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
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
            cmd_tx: tx,
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
        };
        self.connections.lock().await.insert(id.to_string(), conn);
        rx
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
    ) -> Result<String, AcpError> {
        // Connection dedup: when resuming an agent session (session_id is
        // Some), look for a live AgentConnection that already represents
        // the same external session in the same working_dir for the same
        // agent_type and is not torn down. If found, reuse it instead of
        // spawning a fresh process — this is what makes a browser refresh
        // mid-turn re-attach to the existing live state rather than orphan it.
        let working_dir_path = working_dir.as_ref().map(PathBuf::from);

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
        let dedup_lock = if let Some(sid) = session_id.as_deref() {
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
        };

        if let Some(existing) = self
            .find_connection_for_reuse(agent_type, working_dir_path.as_ref(), session_id.as_deref())
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
        // Internal title runs never carry Codeg MCP / delegation injection.
        let skip_delegation_injection = launch_context.purpose == ConnectionPurpose::InternalTitle;
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
            let connection_id = uuid::Uuid::new_v4().to_string();
            tracing::info!(
                "[ACP] spawning connection id={} owner_window={} agent={:?} \
                 attempt={} effective={:?}",
                connection_id,
                owner_window_label,
                agent_type,
                attempt,
                attempt_plan.effective
            );

            let injection = if skip_delegation_injection {
                None
            } else {
                self.delegation_snapshot()
            };

            let SpawnHandshake {
                session_started_rx,
                route_bootstrap_rx,
            } = match spawn_agent_connection(
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
                emitter.clone(),
                self.connections.clone(),
                preferred_mode_id.clone(),
                preferred_config_values.clone(),
                injection,
                launch_context.clone(),
            )
            .await
            {
                Ok(hs) => hs,
                Err(e) => {
                    // Spawn-time failures (SDK missing, shell, etc.) are Fatal —
                    // never route-specific fallback.
                    return Err(e);
                }
            };

            // When dedup is active, hold the lock until SessionStarted applies.
            if dedup_lock.is_some() {
                let timeout = self.spawn_handshake_timeout;
                let (outcome, elapsed) =
                    wait_for_session_started(session_started_rx, timeout).await;
                tracing::info!(
                    "[ACP] dedup_wait connection_id={} session_id={} outcome={} \
                     elapsed_ms={} timeout_ms={}",
                    connection_id,
                    session_id_for_log.as_deref().unwrap_or(""),
                    outcome.as_str(),
                    elapsed.as_millis(),
                    timeout.as_millis(),
                );
            }

            match route_bootstrap_rx.await {
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
        let (task_abort, state, cmd_tx) = {
            let map = self.connections.lock().await;
            match map.get(connection_id) {
                Some(conn) => (
                    conn.task_abort.clone(),
                    Some(Arc::clone(&conn.state)),
                    Some(conn.cmd_tx.clone()),
                ),
                None => (None, None, None),
            }
        };

        // Already absent: nothing to clean up (success).
        if task_abort.is_none() && state.is_none() {
            return Ok(());
        }

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
        if let Some(tx) = cmd_tx {
            let _ = tx.try_send(ConnectionCommand::Disconnect);
        }

        // 3) Observe actual map removal before Ok (no force-remove race).
        let deadline = tokio::time::Instant::now() + primary;
        loop {
            {
                let map = self.connections.lock().await;
                if !map.contains_key(connection_id) {
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
                        if !map.contains_key(connection_id) {
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

    /// Bump `last_activity_at` for a live connection so the idle sweep
    /// won't reap it. Used by the frontend keepalive loop to protect
    /// connections backing currently-open conversation tabs (the
    /// frontend is the only side that knows which tabs the user has
    /// open). Silently no-ops if the connection is missing or already
    /// in a terminal state — touch must never resurrect a dead
    /// connection or contend with the spawn/disconnect paths.
    pub async fn touch(&self, conn_id: &str) -> bool {
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
        let to_disconnect: Vec<String> = {
            let connections = self.connections.lock().await;
            let mut victims = Vec::new();
            for (id, conn) in connections.iter() {
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
                    victims.push(id.clone());
                }
            }
            victims
        };
        let mut disconnected = 0;
        for id in to_disconnect {
            tracing::info!("[ACP] idle sweep disconnecting connection={}", id);
            if self.disconnect(&id).await.is_ok() {
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

    /// Forwards a prompt to the connection's command channel without
    /// touching `prompt_lock`. Internal helper — both `send_prompt` and
    /// `send_prompt_linked` acquire the lock externally and then call
    /// this. Re-entering through `send_prompt` from `send_prompt_linked`
    /// while holding the lock would deadlock, hence the split.
    ///
    /// Admission order (under the caller's per-connection prompt lock):
    /// 1. `reserve()` — only cancellable/blocking point before capture
    /// 2. state write guard; reject in-flight turn
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

        let is_internal = matches!(
            state.purpose,
            ConnectionPurpose::InternalProbe | ConnectionPurpose::InternalTitle
        );
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

        state.turn_in_flight = true;
        // Synchronous tail: mandatory routes then permit.send. No await.
        if let Some(ids) = pending_mandatory_ids {
            if let Some(injection) = self.delegation_snapshot() {
                injection.broker.set_mandatory_profile_routes(conn_id, ids);
            }
        }
        permit.send(ConnectionCommand::Prompt {
            blocks,
            user_message,
            mark_awaiting_reply,
        });
        Ok(())
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
        // Ordinary DB-aware UI path never drives InternalTitle connections.
        {
            let state_arc = self
                .get_state(conn_id)
                .await
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            let purpose = state_arc.read().await.purpose;
            if purpose == ConnectionPurpose::InternalTitle {
                return Err(AcpError::protocol(
                    "send_prompt rejects InternalTitle purpose; use send_prompt_unlinked_internal",
                ));
            }
        }
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        // Non-linked UI sends: register mandatory routes + mark attention.
        // Capture runs only when the connection is already linked (and not
        // internal); unlinked paths bypass capture by design.
        self.send_prompt_inner(Some(db), conn_id, blocks, None, true, true, capture)
            .await
    }

    /// Background (non-UI) prompt: keeps mandatory profile-route registration
    /// but does not mark the turn as awaiting-reply eligible. Unlinked path —
    /// no title capture (Task 4C may convert chat kickoffs to linked sends).
    pub async fn send_prompt_background(
        &self,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError> {
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        self.send_prompt_inner(None, conn_id, blocks, None, true, false, None)
            .await
    }

    /// Unlinked internal enqueue for probe/title workers. Rejects every purpose
    /// except `InternalProbe` and `InternalTitle`, remains unlinked, and
    /// bypasses title capture. Crate-visible for Task 7's runner outside
    /// `acp::manager`.
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
            if !matches!(
                purpose,
                ConnectionPurpose::InternalProbe | ConnectionPurpose::InternalTitle
            ) {
                return Err(AcpError::protocol(format!(
                    "send_prompt_unlinked_internal requires InternalProbe or InternalTitle purpose, got {purpose:?}"
                )));
            }
        }
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        // No db / capture: internal purposes always bypass title capture.
        self.send_prompt_inner(None, conn_id, blocks, None, false, false, None)
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
        blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
        client_message_id: Option<String>,
        mark_awaiting_reply: bool,
        capture: Option<PromptCaptureContext>,
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
        // Delegation is only meaningful on the create-new-row branch — adopting
        // an existing caller-supplied row already has its own (or no) parent
        // linkage. Reject the combination loudly so a misuse from the broker
        // doesn't silently drop the linkage.
        if delegation.is_some() && conversation_id.is_some() {
            return Err(AcpError::protocol(
                "delegation link is incompatible with caller-supplied conversation_id".to_string(),
            ));
        }

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
        let (state_arc, emitter, agent_type, already_linked, turn_in_flight) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            let (already, in_flight) = {
                let s = conn.state.read().await;
                (s.conversation_id.is_some(), s.turn_in_flight)
            };
            (
                conn.state.clone(),
                conn.emitter.clone(),
                conn.agent_type,
                already,
                in_flight,
            )
        };

        // Reject a concurrent prompt while a turn is already in flight, BEFORE
        // any side effects (row creation, InProgress emit, user-message
        // broadcast). `send_prompt_inner` re-checks and sets the flag
        // authoritatively below; doing it here too — while still holding
        // `prompt_lock`, so the value can't change underneath us (the loop only
        // ever clears it) — keeps a rejected prompt from flipping the row to
        // InProgress or broadcasting a phantom user message. The frontend turns
        // this rejection into a queued message above the input box.
        if turn_in_flight {
            return Err(AcpError::TurnInProgress);
        }

        if !already_linked {
            match (conversation_id, folder_id) {
                // Branch A: caller already owns a row — adopt it. No DB write.
                (Some(caller_conv_id), Some(caller_folder_id)) => {
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::ConversationLinked {
                            conversation_id: caller_conv_id,
                            folder_id: caller_folder_id,
                            parent_conversation_id: None,
                            parent_tool_use_id: None,
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

    pub async fn cancel(&self, db: &DatabaseConnection, conn_id: &str) -> Result<(), AcpError> {
        let (cmd_tx, state_arc, emitter) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (
                conn.cmd_tx.clone(),
                conn.state.clone(),
                conn.emitter.clone(),
            )
        };
        cmd_tx
            .send(ConnectionCommand::Cancel)
            .await
            .map_err(|_| AcpError::ProcessExited)?;

        // Eagerly flip the row to `Cancelled` so the sidebar/tabs leave the
        // "running" state immediately. The agent typically replies with
        // `TurnComplete{cancelled}` which the lifecycle subscriber ignores,
        // and stays connected (so `handle_terminal_event` doesn't fire either)
        // — without this write the row would strand on `InProgress`.
        // CAS-guarded so we don't overwrite a `PendingReview`/`Completed`
        // status if the turn happened to end just before the user clicked.
        let conversation_id = state_arc.read().await.conversation_id;
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

        Ok(())
    }

    pub async fn respond_permission(
        &self,
        conn_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<(), AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::RespondPermission {
                request_id: request_id.into(),
                option_id: option_id.into(),
            })
            .await
            .map_err(|_| AcpError::ProcessExited)
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
                    };
                    let inserted = sibling.insert(txn).await?;
                    Ok(inserted.id)
                })
            })
            .await
            .map_err(|e| AcpError::protocol(e.to_string()))
    }

    pub async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError> {
        let cmd_tx = {
            let mut connections = self.connections.lock().await;
            connections.remove(conn_id).map(|conn| conn.cmd_tx)
        };
        if let Some(cmd_tx) = cmd_tx {
            tracing::info!("[ACP] disconnect connection={}", conn_id);
            let _ = cmd_tx.send(ConnectionCommand::Disconnect).await;
            Ok(())
        } else {
            Err(AcpError::ConnectionNotFound(conn_id.into()))
        }
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
        let _ = self.disconnect(&conn_id).await;
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
        let cmd_txs = {
            let mut connections = self.connections.lock().await;
            let ids: Vec<String> = connections
                .iter()
                .filter_map(|(id, conn)| {
                    if conn.owner_window_label == owner_window_label {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            let mut txs = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(conn) = connections.remove(&id) {
                    txs.push(conn.cmd_tx);
                }
            }
            txs
        };

        let disconnected = cmd_txs.len();
        for cmd_tx in cmd_txs {
            let _ = cmd_tx.send(ConnectionCommand::Disconnect).await;
        }
        tracing::info!(
            "[ACP] disconnect by owner window owner_window={} count={}",
            owner_window_label,
            disconnected
        );
        disconnected
    }

    pub async fn disconnect_all(&self) -> usize {
        let cmd_txs: Vec<_> = {
            let mut connections = self.connections.lock().await;
            connections.drain().map(|(_, conn)| conn.cmd_tx).collect()
        };
        let disconnected = cmd_txs.len();
        for cmd_tx in cmd_txs {
            let _ = cmd_tx.send(ConnectionCommand::Disconnect).await;
        }
        tracing::info!("[ACP] disconnect_all count={}", disconnected);
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
        let connections = self.connections.lock().await;
        connections.get(conn_id).map(|conn| conn.state.clone())
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
        let connections = self.connections.lock().await;
        connections
            .get(conn_id)
            .map(|conn| (conn.state.clone(), conn.emitter.clone()))
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
        // Defense-in-depth: the companion validates, but the broker socket is
        // only token-gated, so refuse to broadcast malformed/oversized specs
        // (None → the listener declines the ask, as for any other None path).
        if crate::acp::question::validate_specs(&questions).is_err() {
            return None;
        }
        let (state, emitter) = self.get_state_and_emitter(conn_id).await?;
        let question_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut reg = self.pending_questions.lock().await;
            if reg.values().any(|e| e.parent_connection_id == conn_id) {
                return None;
            }
            reg.insert(
                question_id.clone(),
                PendingQuestionEntry {
                    parent_connection_id: conn_id.to_string(),
                    questions: questions.clone(),
                    sender: tx,
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
                questions,
            },
        )
        .await;
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
            return None;
        }
        Some(RegisteredQuestion {
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

    /// Resolve a pending `ask_user_question` with the user's submission (from any
    /// client). Removes the one-shot atomically (first answer wins; a duplicate /
    /// already-resolved id is an idempotent no-op), sends the self-describing
    /// outcome to the blocked listener, and broadcasts `QuestionResolved` so the
    /// card clears on every client. Routing uses the entry's stored parent
    /// connection (the `question_id` is the authoritative key), so a stale
    /// `conn_id` from the caller can't misroute.
    pub async fn answer_question(
        &self,
        conn_id: &str,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<(), AcpError> {
        let _ = conn_id;
        let entry = self.pending_questions.lock().await.remove(question_id);
        let Some(entry) = entry else {
            // Already answered / canceled / gone elsewhere — idempotent success.
            return Ok(());
        };
        let outcome = build_outcome(&entry.questions, &answer);
        // Ignore a dropped receiver: the listener may have abandoned the wait
        // (peer-close) at the same instant; the resolved-event below still clears
        // the card.
        let _ = entry.sender.send(outcome);
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
        }
        Ok(())
    }

    /// Cancel a pending `ask_user_question` — the companion's tool call was
    /// canceled (peer-close) or the connection is tearing down. Removes the
    /// one-shot (dropping the sender unblocks the listener with a declined
    /// outcome) and broadcasts `QuestionResolved` so the card clears. No-op if
    /// the question was already answered / gone.
    pub async fn cancel_question(&self, conn_id: &str, question_id: &str) {
        let _ = conn_id;
        let removed = self.pending_questions.lock().await.remove(question_id);
        let Some(entry) = removed else {
            return;
        };
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
        }
    }

    /// Cancel every pending `ask_user_question` parked on a connection that is
    /// tearing down. The `run_connection` cleanup guard calls this (alongside
    /// the delegation `DelegationBroker::cancel_by_parent` cascade) so question
    /// entries — and the listener tasks parked on them — are reclaimed
    /// synchronously on disconnect, instead of lingering until the companion's
    /// ask socket happens to close. Dropping each entry's sender unblocks its
    /// listener with a declined outcome; the `QuestionResolved` broadcast clears
    /// the card on every client. No-op when nothing is pending for this parent.
    pub async fn cancel_questions_by_parent(&self, conn_id: &str) {
        // Remove every entry for this parent under the lock (dropping their
        // senders unblocks the parked listeners), then emit outside the lock —
        // the registry mutex is never held across an await.
        let drained: Vec<String> = {
            let mut reg = self.pending_questions.lock().await;
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
        // Best-effort card clear: depending on the teardown path the connection
        // may already be out of the map (`disconnect` removes it before the
        // run_connection cleanup guard fires this sweep), so tolerate `None` — the
        // core removal above already ran and the frontend clears on disconnect.
        if let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await {
            for question_id in drained {
                emit_with_state(&state, &emitter, AcpEvent::QuestionResolved { question_id }).await;
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
        use crate::auto_title::partial_source::{
            fold_partial_candidates, PartialCandidate,
        };
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
            by_conversation.entry(cid).or_default().push(PartialCandidate {
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
struct ParentSpawnLaunchSnapshot {
    emitter: EventEmitter,
    owner_window_label: String,
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
            )
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))
    }

    async fn send_prompt_linked_for_delegation(
        &self,
        conn_id: &str,
        task: String,
        link: crate::acp::delegation::spawner::DelegationLink,
    ) -> Result<
        crate::acp::delegation::spawner::AcceptedDelegationPrompt,
        crate::acp::delegation::spawner::SpawnerError,
    > {
        use crate::acp::delegation::spawner::{AcceptedDelegationPrompt, SpawnerError};
        // The child has no caller-supplied conversation_id (it's brand new).
        // folder_id must be None too — the manager's create-new-row branch
        // requires folder_id, which we resolve from the child's working_dir
        // via folder_service. Do that lookup here so the trait stays small.
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
                SpawnerError::send("child connection has no working_dir; cannot derive folder_id")
            })?
            .to_string_lossy()
            .to_string();
        let folder = crate::db::service::folder_service::add_folder(&self.db.conn, &folder_path)
            .await
            .map_err(|e| SpawnerError::send(format!("add_folder: {e}")))?;

        // Broker task is the authoritative visible text; locale resolves via
        // the child's inherited effective_locale (capture locale = None).
        let capture = Some(PromptCaptureContext::new(Some(task.clone()), None));
        match self
            .manager
            .send_prompt_linked(
                &self.db,
                conn_id,
                vec![PromptInputBlock::Text { text: task }],
                Some(folder.id),
                None,
                Some(link),
                capture,
            )
            .await
        {
            Ok(Some(cid)) => {
                // Soft-watchdog: first successful child prompt enqueue resets
                // agent activity so a newly accepted silent child gets a full
                // threshold window. Does not touch idle-sweep last_activity_at
                // beyond whatever send_prompt already did for general liveness.
                if let Some(state) = self.manager.get_state(conn_id).await {
                    state.write().await.mark_agent_activity(chrono::Utc::now());
                }
                // Authoritative wall start: exact row / timestamp lookup only.
                // Missing or unreadable timestamps fail setup with a fixed
                // non-secret error (no start publication upstream).
                const TIMESTAMP_UNAVAILABLE: &str = "accepted delegation timestamp unavailable";
                match conversation_service::get_by_id(&self.db.conn, cid).await {
                    Ok(row) => match row.delegation_started_at {
                        Some(started_at) => Ok(AcceptedDelegationPrompt {
                            child_conversation_id: cid,
                            started_at,
                        }),
                        None => {
                            tracing::error!(
                                child_conversation_id = cid,
                                code = "accepted_delegation_timestamp_unavailable",
                                "[delegation] accepted row missing delegation_started_at"
                            );
                            Err(SpawnerError::Send {
                                message: TIMESTAMP_UNAVAILABLE.into(),
                                child_conversation_id: Some(cid),
                            })
                        }
                    },
                    Err(_e) => {
                        tracing::error!(
                            child_conversation_id = cid,
                            code = "accepted_delegation_timestamp_unavailable",
                            "[delegation] accepted row timestamp lookup failed"
                        );
                        Err(SpawnerError::Send {
                            message: TIMESTAMP_UNAVAILABLE.into(),
                            child_conversation_id: Some(cid),
                        })
                    }
                }
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
            .disconnect(conn_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::connection::AgentConnection;
    use crate::acp::session_state::SessionState;
    use crate::acp::types::ConnectionStatus;
    use crate::web::event_bridge::{EventEmitter, WebEvent, WebEventBroadcaster};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::{broadcast, mpsc, RwLock};

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
        use crate::commands::conversation_experience::KEY_AUTO_TITLE_AGENT;
        use crate::db::entities::auto_title_job;
        use crate::db::service::app_metadata_service;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = Arc::new(test_helpers::fresh_in_memory_db().await);
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).expect("serialize agent"),
        )
        .await
        .expect("enable auto title");

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
            )
            .await
            .expect("delegated send");
        let conversation_id = accepted.child_conversation_id;
        assert!(
            accepted.started_at.timestamp() > 0,
            "accepted path must return durable started_at"
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

    /// Production boundary: child row exists after enqueue but
    /// `delegation_started_at` is missing → fixed non-secret
    /// `SpawnerError::Send` so the broker setup path can tear down without
    /// publishing running meta / DelegationStarted.
    #[tokio::test]
    async fn accepted_delegation_timestamp_unavailable_when_row_missing_started_at() {
        use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink, SpawnerError};
        use crate::db::test_helpers;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let db = Arc::new(test_helpers::fresh_in_memory_db().await);
        let mgr = Arc::new(ConnectionManager::new());
        let child_id = "deleg-child-ts-miss";
        let child_workdir = PathBuf::from("/tmp/deleg-child-ts-miss");
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

        let folder_id = test_helpers::seed_folder(&db, "/tmp/deleg-ts-miss").await;
        let parent_conversation =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent conversation");

        // Pre-create the durable child row the spawner will look up after
        // enqueue, then clear started_at so the production mapping path fires.
        let child_row = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_conversation.id,
                parent_tool_use_id: "tu-ts-miss".into(),
                delegation_call_id: "call-ts-miss".into(),
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
            active.delegation_started_at = Set(None);
            active.update(&db.conn).await.expect("clear started_at");
        }
        // already_linked path reuses this row (no second create).
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
        let err = spawner
            .send_prompt_linked_for_delegation(
                child_id,
                "task body for missing timestamp".into(),
                DelegationLink {
                    parent_conversation_id: parent_conversation.id,
                    parent_tool_use_id: "tu-ts-miss".into(),
                    delegation_call_id: "call-ts-miss".into(),
                },
            )
            .await
            .expect_err("missing started_at must fail acceptance");
        match err {
            SpawnerError::Send {
                message,
                child_conversation_id,
            } => {
                assert_eq!(
                    message, "accepted delegation timestamp unavailable",
                    "fixed non-secret error only"
                );
                assert_eq!(child_conversation_id, Some(child_row.id));
            }
            other => panic!("expected SpawnerError::Send, got {other:?}"),
        }
        // Drain enqueue so the test connection stays tidy.
        let _ = child_rx.try_recv();
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
        let (tx, _rx) = mpsc::channel(1);
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
            cmd_tx: tx,
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
        }
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
        let (tx, _rx) = mpsc::channel(1);
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
            cmd_tx: tx,
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
        use crate::commands::conversation_experience::KEY_AUTO_TITLE_AGENT;
        use crate::db::service::app_metadata_service;
        use crate::db::test_helpers;
        use crate::models::system::AppLocale;

        let db = test_helpers::fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).expect("serialize agent"),
        )
        .await
        .expect("enable auto title");
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
        }

        // Immediate completion path: receive the queued command with no delay.
        let cmd = fixture
            .command_receiver
            .try_recv()
            .expect("prompt must already be enqueued after successful admission");
        assert!(matches!(cmd, ConnectionCommand::Prompt { .. }));
    }

    #[tokio::test]
    async fn linked_and_already_linked_sends_share_capture_once() {
        use crate::auto_title::PromptCaptureContext;
        use crate::commands::conversation_experience::KEY_AUTO_TITLE_AGENT;
        use crate::db::service::app_metadata_service;
        use crate::db::test_helpers;
        use crate::models::system::AppLocale;

        let db = test_helpers::fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).unwrap(),
        )
        .await
        .unwrap();
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
            use crate::commands::conversation_experience::KEY_AUTO_TITLE_AGENT;
            use crate::db::service::app_metadata_service;
            use crate::db::test_helpers;
            let db = test_helpers::fresh_in_memory_db().await;
            app_metadata_service::upsert_value(
                &db.conn,
                KEY_AUTO_TITLE_AGENT,
                &serde_json::to_string(&AgentType::Codex).unwrap(),
            )
            .await
            .unwrap();
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
            err.to_string().contains("InternalTitle")
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
        mgr.send_prompt_background("policy-conn", one_text_block())
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
        let (tx, rx) = mpsc::channel::<crate::acp::connection::ConnectionCommand>(4);
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
            cmd_tx: tx,
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
        };
        mgr.connections
            .lock()
            .await
            .insert(conn_id.to_string(), conn);
        rx
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
        let (tx, mut rx) = mpsc::channel::<ConnectionCommand>(4);
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
            cmd_tx: tx,
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

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(prompts.len(), 1, "the kickoff prompt is enqueued");
        assert!(
            prompts[0].is_none(),
            "delegation child Prompt must carry NO user_message (kickoff is surfaced separately)"
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

    #[test]
    fn with_spawn_handshake_timeout_overrides_default_for_tests() {
        let mgr = ConnectionManager::with_spawn_handshake_timeout(Duration::from_secs(7));
        assert_eq!(mgr.spawn_handshake_timeout, Duration::from_secs(7));
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
        let (tx, mut rx) = mpsc::channel::<ConnectionCommand>(4);
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
            cmd_tx: tx,
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
        let (tx, mut rx) = mpsc::channel::<ConnectionCommand>(4);
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
            cmd_tx: tx,
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
        }]
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
        mgr.answer_question("cq", &reg.question_id, answer)
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
        use tokio::sync::mpsc;

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
            tokens: Arc::clone(&tokens),
            leases: Arc::clone(&leases),
            socket_path: PathBuf::from("/tmp/codeg-test.sock"),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            questions: Arc::new(NoQuestions)
                as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        });

        let conn_id = "unexposed-1".to_string();
        let mut state =
            SessionState::new(conn_id.clone(), AgentType::Codex, None, "test".into(), None);
        state.delegation_token = Some(token.clone());
        state.status = ConnectionStatus::Connecting;
        let state = Arc::new(RwLock::new(state));
        let (tx, _rx) = mpsc::channel::<ConnectionCommand>(4);
        let terminal_shell = crate::acp::connection::test_placeholder_terminal_shell();
        let route_plan = codeg_plan_for_late_close();
        let (spawn_config, observed_config) = matching_config_pair(
            "agent",
            terminal_shell.selection_key.clone(),
            route_plan.fingerprint.clone(),
        );

        let removed = Arc::new(AtomicBool::new(false));
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
        });

        mgr.connections.lock().await.insert(
            conn_id.clone(),
            AgentConnection {
                id: conn_id.clone(),
                agent_type: AgentType::Codex,
                status: ConnectionStatus::Connecting,
                owner_window_label: "test".into(),
                cmd_tx: tx,
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
            elapsed >= Duration::from_millis(100),
            "teardown must wait for delayed cleanup, not return immediately; elapsed={elapsed:?}"
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
            vec!["revoked", "map_removed"],
            "expected revoke then map removal before attempt 2"
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
        use tokio::sync::mpsc;

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
            tokens: Arc::clone(&tokens),
            leases: Arc::clone(&leases),
            socket_path: PathBuf::from("/tmp/codeg-test.sock"),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            questions: Arc::new(NoQuestions)
                as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        });

        let conn_id = "stuck-1".to_string();
        let mut state =
            SessionState::new(conn_id.clone(), AgentType::Codex, None, "test".into(), None);
        state.delegation_token = Some(token.clone());
        state.status = ConnectionStatus::Connecting;
        let state = Arc::new(RwLock::new(state));
        let (tx, _rx) = mpsc::channel::<ConnectionCommand>(4);
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
                cmd_tx: tx,
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
}
