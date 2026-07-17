//! Isolated hidden agent runner for automatic conversation titles.
//!
//! Spawns a Codeg-owned temporary connection (`ConnectionPurpose::InternalTitle`,
//! `EventEmitter::Noop`), registers the external session before prompting, and
//! normalizes a single concise title from private-stream `ContentDelta` text.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::time::{timeout, timeout_at, Instant};
use tokio_util::sync::CancellationToken;

use crate::acp::error::AcpError;
use crate::acp::manager::ConnectionManager;
use crate::acp::terminal_context::{build_acp_launch_inputs, AcpLaunchInputs, AcpRouteRequest};
use crate::acp::types::{AcpEvent, EventEnvelope, PromptInputBlock};
use crate::auto_title::internal_sessions::{InternalAgentSessionRegistry, InternalSessionPurpose};
use crate::auto_title::types::{
    AutoTitleAttempt, AutoTitleRunError, ConnectionLaunchContext, ConnectionPurpose,
};
use crate::commands::acp::acp_get_agent_status_core;
use crate::commands::delegation::DelegationRuntimeSnapshot;
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::models::system::AppLocale;
use crate::web::event_bridge::EventEmitter;

/// Fixed owner label for internal title connections (never a real window).
pub(crate) const INTERNAL_TITLE_OWNER: &str = "internal:auto-title";

const OVERALL_DEADLINE_SECS: u64 = 90;
const DISCOVERY_LEASE_SECS: u64 = 15;
const DISCONNECT_CLEANUP_SECS: u64 = 5;
const MAX_TITLE_SCALARS: usize = 80;

/// Production-facing title runner contract (Task 8 coordinator consumes this).
#[async_trait]
pub trait TitleAgentRunner: Send + Sync {
    async fn run(
        &self,
        attempt: AutoTitleAttempt,
        cancellation: CancellationToken,
    ) -> Result<String, AutoTitleRunError>;
}

/// Crate-private connection surface used by [`HiddenAgentRunner`].
/// Exact contract: spawn, identity_and_subscribe, send_internal, disconnect.
#[async_trait]
pub(crate) trait TitleConnectionDriver: Send + Sync {
    async fn spawn_internal_title(
        &self,
        agent: AgentType,
        working_dir: PathBuf,
        launch_inputs: AcpLaunchInputs,
        locale: AppLocale,
    ) -> Result<String, AcpError>;

    async fn identity_and_subscribe(
        &self,
        conn_id: &str,
    ) -> Result<(Option<String>, broadcast::Receiver<Arc<EventEnvelope>>), AcpError>;

    async fn send_internal(
        &self,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError>;

    async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError>;
}

/// Launch-policy helper: internal title connections always use silent Noop
/// delivery (no ACP bus / transport target).
pub(crate) fn internal_title_event_emitter() -> EventEmitter {
    EventEmitter::Noop
}

/// Production driver that shares existing [`ConnectionManager`] internals.
pub struct ManagerTitleConnectionDriver {
    manager: Arc<ConnectionManager>,
}

#[allow(dead_code)] // Constructed by Task 8 coordinator / AppState wiring.
impl ManagerTitleConnectionDriver {
    pub(crate) fn new(manager: Arc<ConnectionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl TitleConnectionDriver for ManagerTitleConnectionDriver {
    async fn spawn_internal_title(
        &self,
        agent: AgentType,
        working_dir: PathBuf,
        launch_inputs: AcpLaunchInputs,
        locale: AppLocale,
    ) -> Result<String, AcpError> {
        self.manager
            .spawn_agent(
                agent,
                Some(working_dir.to_string_lossy().into_owned()),
                None,
                launch_inputs,
                INTERNAL_TITLE_OWNER.to_string(),
                internal_title_event_emitter(),
                None,
                std::collections::BTreeMap::new(),
                ConnectionLaunchContext {
                    purpose: ConnectionPurpose::InternalTitle,
                    inherited_locale: Some(locale),
                },
            )
            .await
    }

    async fn identity_and_subscribe(
        &self,
        conn_id: &str,
    ) -> Result<(Option<String>, broadcast::Receiver<Arc<EventEnvelope>>), AcpError> {
        self.manager.identity_and_subscribe(conn_id).await
    }

    async fn send_internal(
        &self,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError> {
        self.manager
            .send_prompt_unlinked_internal(conn_id, blocks)
            .await
    }

    async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError> {
        self.manager.disconnect(conn_id).await
    }
}

/// Isolated title runner: status check → exclusive lease → spawn → identity
/// register → prompt → collect → cleanup.
pub struct HiddenAgentRunner {
    db: Arc<AppDatabase>,
    driver: Arc<dyn TitleConnectionDriver>,
    registry: Arc<InternalAgentSessionRegistry>,
    data_dir: PathBuf,
}

// Task 8 wires these constructors into AppState; until then they are only
// exercised from unit tests and must remain crate-visible for that call site.
#[allow(dead_code)]
impl HiddenAgentRunner {
    pub(crate) fn new(
        db: Arc<AppDatabase>,
        driver: Arc<dyn TitleConnectionDriver>,
        registry: Arc<InternalAgentSessionRegistry>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            db,
            driver,
            registry,
            data_dir,
        }
    }
}

#[async_trait]
impl TitleAgentRunner for HiddenAgentRunner {
    async fn run(
        &self,
        attempt: AutoTitleAttempt,
        cancellation: CancellationToken,
    ) -> Result<String, AutoTitleRunError> {
        let overall_deadline = Instant::now() + Duration::from_secs(OVERALL_DEADLINE_SECS);

        // --- status / availability ---
        let status = match phase(
            &cancellation,
            overall_deadline,
            acp_get_agent_status_core(attempt.agent, self.db.as_ref()),
        )
        .await
        {
            PhaseOutcome::Cancelled => return Err(AutoTitleRunError::Cancelled),
            PhaseOutcome::Timeout => return Err(AutoTitleRunError::Timeout),
            PhaseOutcome::Ready(Ok(s)) => s,
            PhaseOutcome::Ready(Err(_)) => {
                return Err(AutoTitleRunError::Unavailable);
            }
        };
        if !status.available || !status.enabled {
            return Err(AutoTitleRunError::Unavailable);
        }

        // --- launch inputs (config) ---
        let launch_inputs = match phase(
            &cancellation,
            overall_deadline,
            build_acp_launch_inputs(
                self.db.as_ref(),
                attempt.agent,
                None,
                &self.data_dir,
                AcpRouteRequest::root(None, None),
                &DelegationRuntimeSnapshot::default(),
            ),
        )
        .await
        {
            PhaseOutcome::Cancelled => return Err(AutoTitleRunError::Cancelled),
            PhaseOutcome::Timeout => return Err(AutoTitleRunError::Timeout),
            PhaseOutcome::Ready(Ok(inputs)) => inputs,
            PhaseOutcome::Ready(Err(e)) => {
                return Err(AutoTitleRunError::Spawn(e.to_string()));
            }
        };

        let run_dir = self
            .registry
            .reserved_root()
            .join(uuid::Uuid::new_v4().to_string());
        if let Err(e) = std::fs::create_dir_all(&run_dir) {
            return Err(AutoTitleRunError::Spawn(format!("create run dir: {e}")));
        }

        // Exclusive discovery lease immediately before spawn. Race through the
        // shared phase helper so cancellation/timeout prevent later phases.
        let lease_deadline = Instant::now() + Duration::from_secs(DISCOVERY_LEASE_SECS);
        let mut lease = match phase(
            &cancellation,
            overall_deadline,
            self.registry.exclusive_discovery_lease(),
        )
        .await
        {
            PhaseOutcome::Cancelled => {
                let _ = best_effort_remove_dir(&run_dir);
                return Err(AutoTitleRunError::Cancelled);
            }
            PhaseOutcome::Timeout => {
                let _ = best_effort_remove_dir(&run_dir);
                return Err(AutoTitleRunError::Timeout);
            }
            PhaseOutcome::Ready(guard) => Some(guard),
        };

        // --- spawn ---
        let spawn_result = phase(
            &cancellation,
            overall_deadline,
            self.driver.spawn_internal_title(
                attempt.agent,
                run_dir.clone(),
                launch_inputs,
                attempt.locale,
            ),
        )
        .await;

        let conn_id = match spawn_result {
            PhaseOutcome::Cancelled => {
                drop(lease.take());
                let _ = best_effort_remove_dir(&run_dir);
                return Err(AutoTitleRunError::Cancelled);
            }
            PhaseOutcome::Timeout => {
                drop(lease.take());
                let _ = best_effort_remove_dir(&run_dir);
                return Err(AutoTitleRunError::Timeout);
            }
            PhaseOutcome::Ready(Err(e)) => {
                drop(lease.take());
                let _ = best_effort_remove_dir(&run_dir);
                return Err(AutoTitleRunError::Spawn(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(id)) => id,
        };

        // Connection exists: every exit path disconnects exactly once.
        let outcome = self
            .run_after_spawn(
                &attempt,
                &cancellation,
                overall_deadline,
                lease_deadline,
                &mut lease,
                &conn_id,
            )
            .await;

        // Cleanup outside the already-expired overall deadline.
        cleanup_after_run(self.driver.as_ref(), &conn_id, &run_dir, lease.take()).await;

        outcome
    }
}

impl HiddenAgentRunner {
    async fn run_after_spawn(
        &self,
        attempt: &AutoTitleAttempt,
        cancellation: &CancellationToken,
        overall_deadline: Instant,
        lease_deadline: Instant,
        lease: &mut Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
        conn_id: &str,
    ) -> Result<String, AutoTitleRunError> {
        // --- identity snapshot + subscribe ---
        let (initial_id, mut rx) = match phase(
            cancellation,
            overall_deadline,
            self.driver.identity_and_subscribe(conn_id),
        )
        .await
        {
            PhaseOutcome::Cancelled => return Err(AutoTitleRunError::Cancelled),
            PhaseOutcome::Timeout => return Err(AutoTitleRunError::Timeout),
            PhaseOutcome::Ready(Err(e)) => {
                return Err(AutoTitleRunError::Identity(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(pair)) => pair,
        };

        let external_id = if let Some(id) = initial_id {
            id
        } else {
            // One atomic snapshot+subscription: await SessionStarted on that
            // private receiver only (no peek/poll API).
            match wait_for_session_identity(
                cancellation,
                overall_deadline,
                lease_deadline,
                lease,
                &mut rx,
            )
            .await
            {
                Ok(id) => id,
                Err(e) => return Err(e),
            }
        };

        // --- durable registration before prompt ---
        let reg_result = if let Some(ref mut guard) = lease {
            self.registry
                .register_with_lease(
                    guard,
                    attempt.agent,
                    &external_id,
                    InternalSessionPurpose::Title,
                )
                .await
        } else {
            self.registry
                .register(attempt.agent, &external_id, InternalSessionPurpose::Title)
                .await
        };
        // Drop long lease after register_with_lease (or if already released).
        drop(lease.take());

        if let Err(e) = reg_result {
            return Err(AutoTitleRunError::Registry(e.to_string()));
        }

        // --- prompt ---
        let prompt = build_title_prompt(
            attempt.locale,
            &attempt.first_user_text,
            &attempt.first_assistant_text,
        );
        let blocks = vec![PromptInputBlock::Text { text: prompt }];
        match phase(
            cancellation,
            overall_deadline,
            self.driver.send_internal(conn_id, blocks),
        )
        .await
        {
            PhaseOutcome::Cancelled => return Err(AutoTitleRunError::Cancelled),
            PhaseOutcome::Timeout => return Err(AutoTitleRunError::Timeout),
            PhaseOutcome::Ready(Err(e)) => {
                return Err(AutoTitleRunError::Spawn(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(())) => {}
        }

        // --- completion / collect ---
        let raw = match collect_title_output(cancellation, overall_deadline, &mut rx).await {
            Ok(text) => text,
            Err(e) => return Err(e),
        };

        match normalize_generated_title(&raw) {
            Some(title) => Ok(title),
            None => Err(AutoTitleRunError::EmptyOutput),
        }
    }
}

enum PhaseOutcome<T> {
    Cancelled,
    Timeout,
    Ready(T),
}

/// Select between cancellation, overall deadline, and a phase future.
async fn phase<F, T>(
    cancellation: &CancellationToken,
    overall_deadline: Instant,
    fut: F,
) -> PhaseOutcome<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => PhaseOutcome::Cancelled,
        result = timeout_at(overall_deadline, fut) => {
            match result {
                Ok(v) => PhaseOutcome::Ready(v),
                Err(_) => PhaseOutcome::Timeout,
            }
        }
    }
}

/// Await `SessionStarted` on the single receiver from `identity_and_subscribe`.
/// Races cancellation, the overall deadline, and the 15s discovery-lease deadline.
/// `Lagged` / `Closed` fail safely — no peek/poll recovery path.
async fn wait_for_session_identity(
    cancellation: &CancellationToken,
    overall_deadline: Instant,
    lease_deadline: Instant,
    lease: &mut Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
    rx: &mut broadcast::Receiver<Arc<EventEnvelope>>,
) -> Result<String, AutoTitleRunError> {
    loop {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return Err(AutoTitleRunError::Cancelled);
            }
            _ = tokio::time::sleep_until(overall_deadline) => {
                return Err(AutoTitleRunError::Timeout);
            }
            _ = tokio::time::sleep_until(lease_deadline), if lease.is_some() => {
                // Release only the long-held exclusive discovery lease.
                drop(lease.take());
            }
            msg = rx.recv() => {
                match msg {
                    Ok(envelope) => {
                        if let Some(err) = classify_stream_event(&envelope.payload) {
                            return Err(err);
                        }
                        if let AcpEvent::SessionStarted { session_id } = &envelope.payload {
                            return Ok(session_id.clone());
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return Err(AutoTitleRunError::Identity(
                            "private stream lagged before SessionStarted".into(),
                        ));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(AutoTitleRunError::Identity(
                            "private stream closed before SessionStarted".into(),
                        ));
                    }
                }
            }
        }
    }
}

async fn collect_title_output(
    cancellation: &CancellationToken,
    overall_deadline: Instant,
    rx: &mut broadcast::Receiver<Arc<EventEnvelope>>,
) -> Result<String, AutoTitleRunError> {
    let mut buf = String::new();
    loop {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return Err(AutoTitleRunError::Cancelled);
            }
            _ = tokio::time::sleep_until(overall_deadline) => {
                return Err(AutoTitleRunError::Timeout);
            }
            msg = rx.recv() => {
                match msg {
                    Ok(envelope) => {
                        match &envelope.payload {
                            AcpEvent::ContentDelta { text } => {
                                buf.push_str(text);
                            }
                            AcpEvent::TurnComplete { stop_reason, .. } => {
                                if stop_reason == "end_turn" {
                                    return Ok(buf);
                                }
                                return Err(AutoTitleRunError::AbnormalStop(stop_reason.clone()));
                            }
                            other => {
                                if let Some(err) = classify_stream_event(other) {
                                    return Err(err);
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(AutoTitleRunError::AbnormalStop(
                            "private stream closed".into(),
                        ));
                    }
                }
            }
        }
    }
}

/// Map interactive / disconnect / protocol failures observed on the private stream.
fn classify_stream_event(payload: &AcpEvent) -> Option<AutoTitleRunError> {
    match payload {
        AcpEvent::PermissionRequest { .. } | AcpEvent::QuestionRequest { .. } => {
            Some(AutoTitleRunError::Interactive)
        }
        AcpEvent::Error { message, .. } => Some(AutoTitleRunError::AbnormalStop(message.clone())),
        AcpEvent::StatusChanged { status }
            if matches!(
                status,
                crate::acp::types::ConnectionStatus::Disconnected
                    | crate::acp::types::ConnectionStatus::Error
            ) =>
        {
            Some(AutoTitleRunError::AbnormalStop(format!("{status:?}")))
        }
        _ => None,
    }
}

async fn cleanup_after_run(
    driver: &dyn TitleConnectionDriver,
    conn_id: &str,
    run_dir: &Path,
    lease: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
) {
    drop(lease);
    match timeout(
        Duration::from_secs(DISCONNECT_CLEANUP_SECS),
        driver.disconnect(conn_id),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("[auto_title] disconnect during cleanup: {e}");
        }
        Err(_) => {
            tracing::warn!(
                "[auto_title] disconnect timed out after {DISCONNECT_CLEANUP_SECS}s for {conn_id}"
            );
        }
    }
    let _ = best_effort_remove_dir(run_dir);
}

fn best_effort_remove_dir(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// Display language name for the title prompt (not the serde wire id).
pub(crate) fn locale_display_name(locale: AppLocale) -> &'static str {
    match locale {
        AppLocale::En => "English",
        AppLocale::ZhCn => "Simplified Chinese",
        AppLocale::ZhTw => "Traditional Chinese",
        AppLocale::Ja => "Japanese",
        AppLocale::Ko => "Korean",
        AppLocale::Es => "Spanish",
        AppLocale::De => "German",
        AppLocale::Fr => "French",
        AppLocale::Pt => "Portuguese",
        AppLocale::Ar => "Arabic",
    }
}

/// Normalize raw model output into a single clean title (≤80 Unicode scalars).
pub fn normalize_generated_title(raw: &str) -> Option<String> {
    let first_line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;

    let mut s = first_line.to_string();

    // One heading / list prefix.
    s = strip_heading_or_list_prefix(&s);

    // One paired outer quote / backtick / emphasis layer.
    s = strip_one_outer_wrapper(&s);

    // Remove non-whitespace control characters.
    s = s
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect();

    // Collapse whitespace.
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    // Truncate to 80 Unicode scalars.
    let truncated: String = collapsed.chars().take(MAX_TITLE_SCALARS).collect();
    if truncated.is_empty() {
        None
    } else {
        Some(truncated)
    }
}

fn strip_heading_or_list_prefix(s: &str) -> String {
    let t = s.trim();
    // Markdown heading: one or more # followed by space.
    if let Some(rest) = t.strip_prefix('#') {
        let rest = rest.trim_start_matches('#').trim_start();
        if rest.len() < t.len() {
            return rest.to_string();
        }
    }
    // Unordered list: - or * followed by space.
    for prefix in ["- ", "* ", "• "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    // Ordered list: digits + . + space
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        return t[i + 2..].to_string();
    }
    t.to_string()
}

fn strip_one_outer_wrapper(s: &str) -> String {
    let t = s.trim();
    let pairs = [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('*', '*'),
        ('_', '_'),
        ('“', '”'),
        ('‘', '’'),
    ];
    let mut chars = t.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return String::new(),
    };
    let last = match chars.next_back() {
        Some(c) => c,
        None => return t.to_string(),
    };
    for (open, close) in pairs {
        if first == open && last == close {
            let inner: String = t
                .chars()
                .skip(1)
                .take(t.chars().count().saturating_sub(2))
                .collect();
            return inner.trim().to_string();
        }
    }
    t.to_string()
}

fn build_title_prompt(locale: AppLocale, first_user: &str, first_assistant: &str) -> String {
    format!(
        "Return only one concise conversation title in {}.\n\
Do not use tools. Do not add Markdown, quotes, a prefix, or an explanation.\n\
\n\
Task:\n\
{}\n\
\n\
Final response:\n\
{}",
        locale_display_name(locale),
        first_user,
        first_assistant
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use tokio::sync::Notify;

    use crate::web::event_bridge::emit_with_state;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Scenario {
        Happy,
        Permission,
        /// Private-stream QuestionRequest (no Codeg MCP question UI path).
        Question,
        Refusal,
        Disconnect,
        MalformedOutput,
        RegistryFailure,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BlockedPhase {
        StatusConfig,
        Spawn,
        Identity,
        PromptSend,
        Completion,
    }

    #[derive(Default)]
    struct Gate {
        release: Notify,
        entered: AtomicBool,
        waiters: AtomicUsize,
    }

    impl Gate {
        async fn wait_if_armed(&self, armed: bool) {
            if !armed {
                return;
            }
            self.entered.store(true, Ordering::SeqCst);
            self.waiters.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            self.waiters.fetch_sub(1, Ordering::SeqCst);
        }

        fn release(&self) {
            self.release.notify_waiters();
        }

        fn was_entered(&self) -> bool {
            self.entered.load(Ordering::SeqCst)
        }
    }

    struct FakeAgent {
        scenario: Scenario,
        conn_id: String,
        external_id: String,
        manager: ConnectionManager,
        /// Keep live cmd receiver so send_internal can reserve.
        _cmd_rx: StdMutex<
            Option<tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>>,
        >,
        prompt_count: AtomicUsize,
        disconnect_count: AtomicUsize,
        registered_before_prompt: AtomicBool,
        prompt_after_registration: AtomicBool,
        registry_at_prompt: StdMutex<Option<bool>>,
        order: StdMutex<Vec<&'static str>>,
        spawn_gate: Gate,
        identity_gate: Gate,
        prompt_gate: Gate,
        completion_gate: Gate,
        disconnect_gate: Gate,
        block_disconnect: AtomicBool,
        emit_started_before_subscribe: AtomicBool,
        hold_session_started: AtomicBool,
        session_started_release: Notify,
        finish_text: StdMutex<Option<String>>,
        spawn_cancelled_no_id: AtomicBool,
        force_spawn_fail: AtomicBool,
        /// Count of events observed on the private stream after Noop emit.
        private_stream_events: AtomicUsize,
        /// Set after fake `identity_and_subscribe` returns (test sync only).
        identity_ready: AtomicBool,
    }

    impl FakeAgent {
        fn new(manager: ConnectionManager, scenario: Scenario) -> Arc<Self> {
            Arc::new(Self {
                scenario,
                conn_id: "title-conn-1".into(),
                external_id: "internal-1".into(),
                manager,
                _cmd_rx: StdMutex::new(None),
                prompt_count: AtomicUsize::new(0),
                disconnect_count: AtomicUsize::new(0),
                registered_before_prompt: AtomicBool::new(false),
                prompt_after_registration: AtomicBool::new(false),
                registry_at_prompt: StdMutex::new(None),
                order: StdMutex::new(Vec::new()),
                spawn_gate: Gate::default(),
                identity_gate: Gate::default(),
                prompt_gate: Gate::default(),
                completion_gate: Gate::default(),
                disconnect_gate: Gate::default(),
                block_disconnect: AtomicBool::new(false),
                emit_started_before_subscribe: AtomicBool::new(false),
                hold_session_started: AtomicBool::new(false),
                session_started_release: Notify::new(),
                finish_text: StdMutex::new(None),
                spawn_cancelled_no_id: AtomicBool::new(false),
                force_spawn_fail: AtomicBool::new(false),
                private_stream_events: AtomicUsize::new(0),
                identity_ready: AtomicBool::new(false),
            })
        }

        fn record(&self, step: &'static str) {
            self.order.lock().unwrap().push(step);
        }

        async fn emit(&self, payload: AcpEvent) {
            let Some(state) = self.manager.get_state(&self.conn_id).await else {
                // Connection already cleaned up — ignore late scenario emits.
                return;
            };
            // Probe private stream before emit so Noop delivery is observed.
            let mut private_rx = {
                let guard = state.read().await;
                guard.event_stream().subscribe()
            };
            emit_with_state(&state, &EventEmitter::Noop, payload).await;
            if private_rx.try_recv().is_ok() {
                self.private_stream_events.fetch_add(1, Ordering::SeqCst);
            }
        }

        async fn emit_session_started(&self) {
            self.emit(AcpEvent::SessionStarted {
                session_id: self.external_id.clone(),
            })
            .await;
        }

        fn emit_session_started_before_subscription(&self, external_id: &str) {
            // Fixture external_id is fixed at "internal-1"; the argument
            // documents the intended session id for the brief's call site.
            assert_eq!(
                external_id, self.external_id,
                "fixture external_id is fixed; use agent.external_id"
            );
            self.emit_started_before_subscribe
                .store(true, Ordering::SeqCst);
        }

        fn finish_with(&self, text: &str) {
            *self.finish_text.lock().unwrap() = Some(text.to_string());
        }

        fn prompt_was_sent_after_registration(&self) -> bool {
            self.prompt_after_registration.load(Ordering::SeqCst)
        }

        fn was_disconnected(&self) -> bool {
            self.disconnect_count.load(Ordering::SeqCst) > 0
        }

        fn prompt_count(&self) -> usize {
            self.prompt_count.load(Ordering::SeqCst)
        }
    }

    struct FakeTitleConnectionDriver {
        agent: Arc<FakeAgent>,
        registry: Arc<InternalAgentSessionRegistry>,
        agent_type: AgentType,
        block_spawn: AtomicBool,
        block_identity: AtomicBool,
        block_prompt: AtomicBool,
        block_completion: AtomicBool,
        /// When set, spawn waits on gate and can be cancelled with no conn id.
        cancelable_spawn: AtomicBool,
        /// Exact `working_dir` last passed to `spawn_internal_title`.
        last_working_dir: StdMutex<Option<PathBuf>>,
    }

    impl FakeTitleConnectionDriver {
        fn new(agent: Arc<FakeAgent>, registry: Arc<InternalAgentSessionRegistry>) -> Arc<Self> {
            Arc::new(Self {
                agent,
                registry,
                agent_type: AgentType::Codex,
                block_spawn: AtomicBool::new(false),
                block_identity: AtomicBool::new(false),
                block_prompt: AtomicBool::new(false),
                block_completion: AtomicBool::new(false),
                cancelable_spawn: AtomicBool::new(false),
                last_working_dir: StdMutex::new(None),
            })
        }

        fn last_working_dir(&self) -> Option<PathBuf> {
            self.last_working_dir.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TitleConnectionDriver for FakeTitleConnectionDriver {
        async fn spawn_internal_title(
            &self,
            agent: AgentType,
            working_dir: PathBuf,
            _launch_inputs: AcpLaunchInputs,
            _locale: AppLocale,
        ) -> Result<String, AcpError> {
            let _ = agent;
            self.agent.record("spawn_enter");
            *self.last_working_dir.lock().unwrap() = Some(working_dir.clone());

            if self.agent.force_spawn_fail.load(Ordering::SeqCst) {
                return Err(AcpError::protocol("forced spawn failure"));
            }

            if self.cancelable_spawn.load(Ordering::SeqCst)
                || self.block_spawn.load(Ordering::SeqCst)
            {
                // Model production boundary: no connection ID / map entry until
                // spawn returns. Cancellation here yields no manager entry.
                self.agent.spawn_gate.wait_if_armed(true).await;
                if self.agent.spawn_cancelled_no_id.load(Ordering::SeqCst) {
                    return Err(AcpError::protocol("spawn cancelled before insert"));
                }
            }

            let rx = self
                .agent
                .manager
                .insert_test_connection_live(
                    &self.agent.conn_id,
                    self.agent_type,
                    Some(working_dir),
                    // Mirror production spawn policy (Noop only).
                    internal_title_event_emitter(),
                )
                .await;
            {
                let state = self
                    .agent
                    .manager
                    .get_state(&self.agent.conn_id)
                    .await
                    .expect("state");
                let mut s = state.write().await;
                s.purpose = ConnectionPurpose::InternalTitle;
            }
            *self.agent._cmd_rx.lock().unwrap() = Some(rx);

            if self
                .agent
                .emit_started_before_subscribe
                .load(Ordering::SeqCst)
            {
                self.agent.emit_session_started().await;
            }

            self.agent.record("spawn_return");
            Ok(self.agent.conn_id.clone())
        }

        async fn identity_and_subscribe(
            &self,
            conn_id: &str,
        ) -> Result<(Option<String>, broadcast::Receiver<Arc<EventEnvelope>>), AcpError> {
            self.agent.record("identity_subscribe");
            if self.block_identity.load(Ordering::SeqCst) {
                self.agent.identity_gate.wait_if_armed(true).await;
            }
            let result = self.agent.manager.identity_and_subscribe(conn_id).await;
            // Signal only after subscribe returns so tests can await the real
            // transition into wait_for_session_identity (not a fixed yield count).
            self.agent.identity_ready.store(true, Ordering::SeqCst);
            result
        }

        async fn send_internal(
            &self,
            conn_id: &str,
            blocks: Vec<PromptInputBlock>,
        ) -> Result<(), AcpError> {
            if self.block_prompt.load(Ordering::SeqCst) {
                self.agent.prompt_gate.wait_if_armed(true).await;
            }
            let registered = {
                let (_, filter) = self.registry.shared_filter().await.expect("filter");
                filter.contains(self.agent_type, Some(&self.agent.external_id), None)
            };
            *self.agent.registry_at_prompt.lock().unwrap() = Some(registered);
            if registered {
                self.agent
                    .registered_before_prompt
                    .store(true, Ordering::SeqCst);
                self.agent
                    .prompt_after_registration
                    .store(true, Ordering::SeqCst);
            }
            self.agent.prompt_count.fetch_add(1, Ordering::SeqCst);
            self.agent.record("prompt");

            let result = self
                .agent
                .manager
                .send_prompt_unlinked_internal(conn_id, blocks)
                .await;

            // Drive completion scenarios after a successful enqueue.
            if result.is_ok() {
                let agent = Arc::clone(&self.agent);
                let scenario = self.agent.scenario;
                let block_completion = self.block_completion.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    if block_completion {
                        agent.completion_gate.wait_if_armed(true).await;
                    }
                    // Optional hold for slow SessionStarted tests (identity path).
                    if agent.hold_session_started.load(Ordering::SeqCst) {
                        agent.session_started_release.notified().await;
                    }
                    match scenario {
                        Scenario::Happy => {
                            let text = agent
                                .finish_text
                                .lock()
                                .unwrap()
                                .clone()
                                .unwrap_or_else(|| "default title".into());
                            agent.emit(AcpEvent::ContentDelta { text }).await;
                            agent
                                .emit(AcpEvent::TurnComplete {
                                    session_id: agent.external_id.clone(),
                                    stop_reason: "end_turn".into(),
                                    agent_type: "codex".into(),
                                    mark_awaiting_reply: false,
                                })
                                .await;
                        }
                        Scenario::Permission => {
                            agent
                                .emit(AcpEvent::PermissionRequest {
                                    request_id: "p1".into(),
                                    tool_call: serde_json::json!({}),
                                    options: vec![],
                                })
                                .await;
                        }
                        Scenario::Question => {
                            // Codeg MCP question UI is not injected for InternalTitle;
                            // private-stream QuestionRequest is still an attempt failure.
                            agent
                                .emit(AcpEvent::QuestionRequest {
                                    question_id: "q1".into(),
                                    questions: vec![],
                                })
                                .await;
                        }
                        Scenario::Refusal => {
                            agent
                                .emit(AcpEvent::TurnComplete {
                                    session_id: agent.external_id.clone(),
                                    stop_reason: "refusal".into(),
                                    agent_type: "codex".into(),
                                    mark_awaiting_reply: false,
                                })
                                .await;
                        }
                        Scenario::Disconnect => {
                            agent
                                .emit(AcpEvent::StatusChanged {
                                    status: crate::acp::types::ConnectionStatus::Disconnected,
                                })
                                .await;
                        }
                        Scenario::MalformedOutput => {
                            agent
                                .emit(AcpEvent::ContentDelta {
                                    text: "   \n\t  ".into(),
                                })
                                .await;
                            agent
                                .emit(AcpEvent::TurnComplete {
                                    session_id: agent.external_id.clone(),
                                    stop_reason: "end_turn".into(),
                                    agent_type: "codex".into(),
                                    mark_awaiting_reply: false,
                                })
                                .await;
                        }
                        Scenario::RegistryFailure => {
                            // Should never prompt; if we get here the runner is wrong.
                        }
                    }
                });
            }
            result
        }

        async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError> {
            self.agent.record("disconnect");
            self.agent.disconnect_count.fetch_add(1, Ordering::SeqCst);
            if self.agent.block_disconnect.load(Ordering::SeqCst) {
                self.agent.disconnect_gate.wait_if_armed(true).await;
                // Never completes if gate never released — models stuck disconnect.
                return Ok(());
            }
            self.agent.manager.disconnect(conn_id).await
        }
    }

    struct HiddenRunnerFixture {
        runner: HiddenAgentRunner,
        agent: Arc<FakeAgent>,
        driver: Arc<FakeTitleConnectionDriver>,
        registry: Arc<InternalAgentSessionRegistry>,
        data_dir: tempfile::TempDir,
        _db: Arc<AppDatabase>,
        // Hold cmd rx alive indirectly via agent
    }

    impl HiddenRunnerFixture {
        fn attempt(&self, locale: AppLocale) -> AutoTitleAttempt {
            AutoTitleAttempt {
                conversation_id: 42,
                attempt: 1,
                agent: AgentType::Codex,
                locale,
                first_user_text: "Fix the README search".into(),
                first_assistant_text: "Updated search section".into(),
            }
        }

        fn was_disconnected(&self) -> bool {
            self.agent.was_disconnected()
        }

        fn prompt_count(&self) -> usize {
            self.agent.prompt_count()
        }
    }

    async fn hidden_runner_fixture() -> HiddenRunnerFixture {
        hidden_runner_fixture_for(Scenario::Happy).await
    }

    async fn hidden_runner_fixture_for(scenario: Scenario) -> HiddenRunnerFixture {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        let registry = if matches!(scenario, Scenario::RegistryFailure) {
            let reg =
                InternalAgentSessionRegistry::new_empty_for_test(db.conn.clone(), data_dir.path())
                    .expect("registry");
            // Drop the persistence table so register fails while memory path still runs.
            use sea_orm::{ConnectionTrait, Statement};
            db.conn
                .execute(Statement::from_string(
                    db.conn.get_database_backend(),
                    "DROP TABLE IF EXISTS internal_agent_sessions".to_string(),
                ))
                .await
                .expect("drop table");
            reg
        } else {
            InternalAgentSessionRegistry::new_empty_for_test(db.conn.clone(), data_dir.path())
                .expect("registry")
        };

        let manager = ConnectionManager::new();
        let agent = FakeAgent::new(manager, scenario);
        let driver = FakeTitleConnectionDriver::new(Arc::clone(&agent), Arc::clone(&registry));
        let runner = HiddenAgentRunner::new(
            Arc::clone(&db),
            driver.clone() as Arc<dyn TitleConnectionDriver>,
            Arc::clone(&registry),
            data_dir.path().to_path_buf(),
        );
        HiddenRunnerFixture {
            runner,
            agent,
            driver,
            registry,
            data_dir,
            _db: db,
        }
    }

    #[tokio::test]
    async fn runner_registers_identity_before_sending_and_returns_clean_title() {
        let fixture = hidden_runner_fixture().await;
        fixture
            .agent
            .emit_session_started_before_subscription("internal-1");
        fixture
            .agent
            .finish_with("## \"  修复 README   搜索  \"\nexplanation");

        let title = fixture
            .runner
            .run(fixture.attempt(AppLocale::ZhCn), CancellationToken::new())
            .await
            .expect("title");

        assert_eq!(title, "修复 README 搜索");
        let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
        assert!(filter.contains(AgentType::Codex, Some("internal-1"), None));
        assert!(fixture.agent.prompt_was_sent_after_registration());
        // Isolation: Noop has no ACP bus/transport target; private stream
        // is the only delivery surface used by the fake emit path.
        assert!(
            internal_title_event_emitter().acp_event_bus().is_none(),
            "EventEmitter::Noop must not expose an ACP internal bus"
        );
        assert!(
            fixture.agent.private_stream_events.load(Ordering::SeqCst) > 0,
            "private connection stream must receive Noop-emitted events"
        );
    }

    #[test]
    fn manager_title_spawn_policy_uses_noop_emitter_without_acp_bus() {
        let emitter = internal_title_event_emitter();
        assert!(
            matches!(emitter, EventEmitter::Noop),
            "production title spawn must use EventEmitter::Noop"
        );
        assert!(
            emitter.acp_event_bus().is_none(),
            "Noop must have no ACP internal bus / transport target"
        );
    }

    #[tokio::test]
    async fn permission_abnormal_stop_and_registry_failure_are_attempt_failures() {
        for scenario in [
            Scenario::Permission,
            Scenario::Question,
            Scenario::Refusal,
            Scenario::Disconnect,
            Scenario::MalformedOutput,
        ] {
            let fixture = hidden_runner_fixture_for(scenario).await;
            fixture
                .agent
                .emit_session_started_before_subscription("internal-1");
            let err = fixture
                .runner
                .run(fixture.attempt(AppLocale::En), CancellationToken::new())
                .await
                .expect_err("scenario must fail the attempt");
            match scenario {
                Scenario::Permission | Scenario::Question => {
                    assert!(
                        matches!(err, AutoTitleRunError::Interactive),
                        "{scenario:?} must classify as Interactive, got {err:?}"
                    );
                }
                Scenario::Refusal | Scenario::Disconnect => {
                    assert!(
                        matches!(err, AutoTitleRunError::AbnormalStop(_)),
                        "{scenario:?} must classify as AbnormalStop, got {err:?}"
                    );
                }
                Scenario::MalformedOutput => {
                    assert!(
                        matches!(err, AutoTitleRunError::EmptyOutput),
                        "{scenario:?} must classify as EmptyOutput, got {err:?}"
                    );
                }
                Scenario::Happy | Scenario::RegistryFailure => unreachable!(),
            }
            assert!(fixture.was_disconnected());
        }
        let registry_failure = hidden_runner_fixture_for(Scenario::RegistryFailure).await;
        registry_failure
            .agent
            .emit_session_started_before_subscription("internal-1");
        let reg_err = registry_failure
            .runner
            .run(
                registry_failure.attempt(AppLocale::En),
                CancellationToken::new(),
            )
            .await
            .expect_err("registry failure must fail");
        assert!(
            matches!(reg_err, AutoTitleRunError::Registry(_)),
            "registry failure must classify as Registry, got {reg_err:?}"
        );
        assert_eq!(
            registry_failure.prompt_count(),
            0,
            "registry failure must suppress the title prompt"
        );
        assert!(registry_failure.was_disconnected());
    }

    #[tokio::test]
    async fn overall_timeout_is_shared_across_spawn_handshake_and_completion() {
        let fixture = hidden_runner_fixture().await;
        // Pause only after DB/fixture setup — start_paused breaks sqlite pool open.
        tokio::time::pause();
        fixture.driver.block_spawn.store(true, Ordering::SeqCst);
        fixture.driver.block_identity.store(true, Ordering::SeqCst);
        fixture
            .driver
            .block_completion
            .store(true, Ordering::SeqCst);

        let attempt = fixture.attempt(AppLocale::En);
        let agent = Arc::clone(&fixture.agent);
        let driver = Arc::clone(&fixture.driver);
        let runner = fixture.runner;

        let handle =
            tokio::spawn(async move { runner.run(attempt, CancellationToken::new()).await });

        // Wait until spawn gate entered, advance 40s, release spawn.
        while !agent.spawn_gate.was_entered() {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(40)).await;
        // Identity will block after spawn; release spawn so identity runs.
        agent.spawn_gate.release();

        // Allow identity subscribe to start, then hold SessionStarted path:
        // release identity gate after another 40s of wall budget.
        while !agent.identity_gate.was_entered() {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(40)).await;
        agent.identity_gate.release();
        // identity_and_subscribe returns (None, rx); deliver SessionStarted
        // for registration then leave completion blocked.
        tokio::task::yield_now().await;
        agent.emit_session_started().await;

        // Wait until prompt has been sent (registration complete) then
        // advance remaining 10s to hit the shared 90s deadline.
        for _ in 0..200 {
            if agent.prompt_count.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        assert!(
            agent.prompt_count.load(Ordering::SeqCst) > 0,
            "prompt must be sent before final timeout slice"
        );
        // Shared deadline is exactly 90s: settle before awaiting the join.
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..100 {
            if handle.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            handle.is_finished(),
            "runner must settle at overall second 90 (shared deadline)"
        );

        let result = handle.await.expect("join");
        assert!(
            matches!(result, Err(AutoTitleRunError::Timeout)),
            "expected Timeout, got {result:?}"
        );
        assert!(agent.was_disconnected());
        let _ = driver;
    }

    #[tokio::test]
    async fn slow_handshake_releases_discovery_lease_at_15_seconds_but_sends_only_after_registration(
    ) {
        let fixture = hidden_runner_fixture().await;
        // Do NOT emit SessionStarted before subscribe — hold it past 15s.
        // Keep real time until spawn returns a live connection and identity
        // subscribe completes. Fixed yield counts flake under full-suite load
        // (status/config/lease/spawn need real scheduler progress). Pause only
        // for the strict virtual-time 15s discovery-lease release sequence.
        // lease_deadline Instant is already set before identity wait; remaining
        // budget is still ~15s, so advancing 15 virtual seconds still fires it.
        fixture.agent.finish_with("Slow handshake title");

        let registry = Arc::clone(&fixture.registry);
        let agent = Arc::clone(&fixture.agent);
        let attempt = fixture.attempt(AppLocale::En);
        let runner = fixture.runner;

        let handle =
            tokio::spawn(async move { runner.run(attempt, CancellationToken::new()).await });

        // Bounded real-time wait for the actual spawn→identity transition.
        timeout(Duration::from_secs(5), async {
            loop {
                let live = agent.manager.get_state(&agent.conn_id).await.is_some();
                let identity = agent.identity_ready.load(Ordering::SeqCst);
                if live && identity {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("spawn must have returned a live connection before the 15s lease race");
        // Let the runner task enter wait_for_session_identity's select so the
        // lease timer is armed before we freeze the clock.
        tokio::task::yield_now().await;
        assert!(
            agent.manager.get_state(&agent.conn_id).await.is_some(),
            "spawn must have returned a live connection before the 15s lease race"
        );
        assert_eq!(
            agent.prompt_count(),
            0,
            "no SessionStarted yet: must not prompt before registration"
        );

        tokio::time::pause();

        // Advance past the 15s discovery-lease budget one second at a time so
        // the lease timer in wait_for_session_identity can fire.
        for _ in 0..15 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        // Exclusive lease released: a shared discovery read must not block.
        let shared = timeout(Duration::from_millis(100), registry.shared_filter()).await;
        // timeout itself needs virtual time under pause — advance while racing.
        let shared = match shared {
            Ok(v) => v,
            Err(_) => {
                // Manually poll with advances.
                let fut = registry.shared_filter();
                tokio::pin!(fut);
                let mut resolved = None;
                for _ in 0..50 {
                    tokio::select! {
                        biased;
                        res = &mut fut => { resolved = Some(res); break; }
                        _ = async {
                            tokio::time::advance(Duration::from_millis(10)).await;
                        } => {}
                    }
                }
                resolved.expect("shared_filter must resolve after lease release")
            }
        };
        assert!(
            shared.is_ok(),
            "shared discovery lease must be acquirable after 15s"
        );
        // Drop the shared guard before the runner registers (needs exclusive).
        drop(shared);
        assert_eq!(
            agent.prompt_count(),
            0,
            "must not prompt before registration"
        );

        // Deliver identity before the overall 90s deadline.
        agent.emit_session_started().await;
        {
            let state = agent
                .manager
                .get_state(&agent.conn_id)
                .await
                .expect("state after SessionStarted emit");
            let external = state.read().await.external_id.clone();
            assert_eq!(
                external.as_deref(),
                Some(agent.external_id.as_str()),
                "SessionStarted must apply external_id on session state"
            );
        }
        for _ in 0..200 {
            tokio::task::yield_now().await;
            if handle.is_finished() {
                break;
            }
            tokio::time::advance(Duration::from_millis(5)).await;
        }
        assert!(handle.is_finished(), "runner must settle after end_turn");
        let title = handle.await.expect("task").expect("title ok");
        assert_eq!(title, "Slow handshake title");
        assert_eq!(
            agent.prompt_count(),
            1,
            "exactly one prompt after durable registration"
        );
        assert!(agent.prompt_was_sent_after_registration());
    }

    #[tokio::test]
    async fn spawn_and_registry_failures_leave_reserved_root_sessions_filtered() {
        // Spawn failure: fake records the exact working_dir the runner passed.
        // After cleanup removes that directory, the path string remains under
        // reserved_root and is still matched by the lexical registry filter.
        let fixture = hidden_runner_fixture().await;
        fixture.agent.force_spawn_fail.store(true, Ordering::SeqCst);
        let reserved = fixture.registry.reserved_root().to_path_buf();

        let err = fixture
            .runner
            .run(fixture.attempt(AppLocale::En), CancellationToken::new())
            .await;
        assert!(
            matches!(err, Err(AutoTitleRunError::Spawn(_))),
            "forced spawn failure must surface Spawn, got {err:?}"
        );

        let run_dir = fixture
            .driver
            .last_working_dir()
            .expect("spawn must record the exact working_dir passed by the runner");
        assert!(
            crate::auto_title::internal_sessions::is_lexically_below(
                &run_dir.to_string_lossy(),
                &reserved,
            ),
            "runner spawn working_dir must be lexically under reserved_root: {run_dir:?}"
        );
        assert!(
            !run_dir.exists(),
            "per-run directory must be removed after spawn failure cleanup"
        );

        let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
        assert!(
            filter.contains(AgentType::Codex, None, Some(&run_dir.to_string_lossy()),),
            "recorded run path must remain matched by reserved-root filter after removal"
        );

        // Outside reserved_root is not hidden by path fallback.
        let outside = fixture.data_dir.path().join("outside-reserved-run");
        assert!(
            !filter.contains(AgentType::Codex, None, Some(&outside.to_string_lossy())),
            "path outside reserved_root must not match the filter"
        );

        // Registry failure path: durable id and/or reserved-root path hide.
        let fixture2 = hidden_runner_fixture_for(Scenario::RegistryFailure).await;
        fixture2
            .agent
            .emit_session_started_before_subscription("internal-1");
        let err = fixture2
            .runner
            .run(fixture2.attempt(AppLocale::En), CancellationToken::new())
            .await;
        assert!(err.is_err(), "registry failure must fail the attempt");
        let recorded2 = fixture2
            .driver
            .last_working_dir()
            .expect("registry-failure path still spawns");
        let (_, filter2) = fixture2.registry.shared_filter().await.expect("filter");
        let path_hit = filter2.contains(AgentType::Codex, None, Some(&recorded2.to_string_lossy()));
        let id_hit = filter2.contains(AgentType::Codex, Some("internal-1"), None);
        assert!(
            path_hit || id_hit,
            "registry failure must still leave reserved-root sessions filtered (path={path_hit} id={id_hit})"
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_each_runner_phase_and_disconnects_after_spawn() {
        // Status/config are wrapped by the shared `phase()` helper in production.
        // There is no deterministic DB gate for status; StatusConfig cancels
        // immediately pre-run (limitation: does not prove mid-DB cancellation).
        // Spawn/identity/prompt/completion use explicit mid-phase gates.
        for phase in [
            BlockedPhase::StatusConfig,
            BlockedPhase::Spawn,
            BlockedPhase::Identity,
            BlockedPhase::PromptSend,
            BlockedPhase::Completion,
        ] {
            let fixture = hidden_runner_fixture().await;
            let cancel = CancellationToken::new();
            match phase {
                BlockedPhase::StatusConfig => {
                    // Immediate pre-run cancel — status/config use phase() but
                    // have no injectable mid-call DB gate in this fixture.
                    cancel.cancel();
                }
                BlockedPhase::Spawn => {
                    fixture
                        .driver
                        .cancelable_spawn
                        .store(true, Ordering::SeqCst);
                    fixture.driver.block_spawn.store(true, Ordering::SeqCst);
                    fixture
                        .agent
                        .spawn_cancelled_no_id
                        .store(true, Ordering::SeqCst);
                }
                BlockedPhase::Identity => {
                    fixture
                        .agent
                        .emit_session_started_before_subscription("internal-1");
                    fixture.driver.block_identity.store(true, Ordering::SeqCst);
                }
                BlockedPhase::PromptSend => {
                    fixture
                        .agent
                        .emit_session_started_before_subscription("internal-1");
                    fixture.driver.block_prompt.store(true, Ordering::SeqCst);
                }
                BlockedPhase::Completion => {
                    fixture
                        .agent
                        .emit_session_started_before_subscription("internal-1");
                    fixture
                        .driver
                        .block_completion
                        .store(true, Ordering::SeqCst);
                    fixture.agent.finish_with("should not accept");
                }
            }

            let agent = Arc::clone(&fixture.agent);
            let registry = Arc::clone(&fixture.registry);
            let attempt = fixture.attempt(AppLocale::En);
            let cancel2 = cancel.clone();
            let runner = fixture.runner;

            let handle = tokio::spawn(async move { runner.run(attempt, cancel2).await });

            // Enter blocked phase then cancel (representative mid-phase gates).
            match phase {
                BlockedPhase::StatusConfig => {}
                BlockedPhase::Spawn => {
                    while !agent.spawn_gate.was_entered() {
                        tokio::task::yield_now().await;
                    }
                    cancel.cancel();
                    agent.spawn_gate.release();
                }
                BlockedPhase::Identity => {
                    while !agent.identity_gate.was_entered() {
                        tokio::task::yield_now().await;
                    }
                    cancel.cancel();
                    agent.identity_gate.release();
                }
                BlockedPhase::PromptSend => {
                    while !agent.prompt_gate.was_entered() {
                        tokio::task::yield_now().await;
                    }
                    cancel.cancel();
                    agent.prompt_gate.release();
                }
                BlockedPhase::Completion => {
                    while agent.prompt_count() == 0 {
                        tokio::task::yield_now().await;
                    }
                    cancel.cancel();
                    agent.completion_gate.release();
                }
            }

            let result = handle.await.expect("join");
            assert!(
                matches!(result, Err(AutoTitleRunError::Cancelled)),
                "phase {phase:?} expected Cancelled, got {result:?}"
            );

            match phase {
                BlockedPhase::StatusConfig | BlockedPhase::Spawn => {
                    assert_eq!(agent.disconnect_count.load(Ordering::SeqCst), 0);
                    assert_eq!(agent.prompt_count(), 0);
                    assert!(
                        agent.manager.get_state("title-conn-1").await.is_none(),
                        "no manager entry before spawn returns"
                    );
                }
                _ => {
                    assert_eq!(agent.disconnect_count.load(Ordering::SeqCst), 1);
                    // No later successful output acceptance: prompt may have
                    // started but completion must not commit a title.
                }
            }

            // Discovery lease released: shared_filter must acquire.
            let shared = timeout(Duration::from_millis(200), registry.shared_filter()).await;
            assert!(shared.is_ok(), "lease released after cancel at {phase:?}");
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_blocked_discovery_lease_acquisition() {
        let fixture = hidden_runner_fixture().await;
        let cancel = CancellationToken::new();
        let agent = Arc::clone(&fixture.agent);
        let registry = Arc::clone(&fixture.registry);
        let attempt = fixture.attempt(AppLocale::En);
        let cancel2 = cancel.clone();
        let runner = fixture.runner;

        // Hold exclusive discovery lease so runner blocks before spawn.
        let held_lease = registry.exclusive_discovery_lease().await;

        let mut handle = tokio::spawn(async move { runner.run(attempt, cancel2).await });

        // Yield enough for status/config to finish and reach lease acquisition.
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Prove fake spawn has not started while the external lease is held.
        assert!(
            !agent.order.lock().unwrap().contains(&"spawn_enter"),
            "spawn must not start while discovery lease is blocked"
        );
        assert_eq!(agent.prompt_count(), 0);
        assert_eq!(agent.disconnect_count.load(Ordering::SeqCst), 0);
        assert!(
            agent.manager.get_state("title-conn-1").await.is_none(),
            "no manager entry before spawn while discovery lease is blocked"
        );

        // Cancel while the external held lease is still held.
        cancel.cancel();

        // Bounded wait: OLD code hangs on uncancelable exclusive_discovery_lease
        // and fails this assertion rather than hanging the suite indefinitely.
        let settled = timeout(Duration::from_millis(500), &mut handle).await;
        assert!(
            settled.is_ok(),
            "runner must settle on cancellation while discovery lease is blocked"
        );
        let result = settled.unwrap().expect("join");
        assert!(
            matches!(result, Err(AutoTitleRunError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );

        assert_eq!(agent.prompt_count(), 0);
        assert_eq!(agent.disconnect_count.load(Ordering::SeqCst), 0);
        assert!(
            !agent.order.lock().unwrap().contains(&"spawn_enter"),
            "spawn must not run after cancellation during lease wait"
        );
        assert!(
            agent.manager.get_state("title-conn-1").await.is_none(),
            "no manager entry after cancelled lease acquisition"
        );

        // Release the held lease only after cancellation result assertions.
        drop(held_lease);
    }

    #[tokio::test]
    async fn blocked_disconnect_cleanup_is_bounded_and_releases_the_attempt() {
        let fixture = hidden_runner_fixture().await;
        tokio::time::pause();
        fixture
            .agent
            .emit_session_started_before_subscription("internal-1");
        fixture
            .driver
            .block_completion
            .store(true, Ordering::SeqCst);
        fixture.agent.block_disconnect.store(true, Ordering::SeqCst);

        // Sibling sentinel under reserved_root must survive per-run cleanup.
        let reserved = fixture.registry.reserved_root().to_path_buf();
        let sentinel_dir = reserved.join("sentinel-sibling");
        std::fs::create_dir_all(&sentinel_dir).expect("sentinel dir");
        let sentinel_file = sentinel_dir.join("keep.txt");
        std::fs::write(&sentinel_file, b"keep").expect("sentinel file");

        let agent = Arc::clone(&fixture.agent);
        let driver = Arc::clone(&fixture.driver);
        let attempt = fixture.attempt(AppLocale::En);
        let runner = fixture.runner;

        let handle =
            tokio::spawn(async move { runner.run(attempt, CancellationToken::new()).await });

        // Reach completion wait then expire overall deadline.
        for _ in 0..200 {
            tokio::task::yield_now().await;
            if agent.prompt_count() > 0 {
                break;
            }
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        assert!(agent.prompt_count() > 0, "must reach completion phase");
        let run_dir = driver
            .last_working_dir()
            .expect("spawn records exact per-run working_dir");
        assert!(
            run_dir.exists(),
            "per-run directory must exist before cleanup"
        );
        assert_ne!(
            run_dir, sentinel_dir,
            "run dir must not be the sentinel sibling"
        );

        // Force timeout by advancing remaining overall budget.
        tokio::time::advance(Duration::from_secs(90)).await;

        // Disconnect is blocked; advance 5s cleanup budget.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(5)).await;

        let result = timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "runner must settle after cleanup budget");
        let outcome = result.unwrap().expect("join");
        // Original outcome preserved (Timeout).
        assert!(
            matches!(outcome, Err(AutoTitleRunError::Timeout)),
            "expected Timeout, got {outcome:?}"
        );
        assert!(
            agent.disconnect_count.load(Ordering::SeqCst) >= 1,
            "disconnect must be attempted"
        );
        assert!(
            !run_dir.exists(),
            "exact per-run directory must be removed after bounded cleanup: {run_dir:?}"
        );
        assert!(
            sentinel_dir.exists() && sentinel_file.exists(),
            "sibling sentinel under reserved_root must remain"
        );
    }

    #[test]
    fn locale_display_names_cover_all_ten_supported_locales() {
        let cases = [
            (AppLocale::En, "English"),
            (AppLocale::ZhCn, "Simplified Chinese"),
            (AppLocale::ZhTw, "Traditional Chinese"),
            (AppLocale::Ja, "Japanese"),
            (AppLocale::Ko, "Korean"),
            (AppLocale::Es, "Spanish"),
            (AppLocale::De, "German"),
            (AppLocale::Fr, "French"),
            (AppLocale::Pt, "Portuguese"),
            (AppLocale::Ar, "Arabic"),
        ];
        for (locale, expected) in cases {
            assert_eq!(locale_display_name(locale), expected);
            let prompt = build_title_prompt(locale, "t", "r");
            assert!(
                prompt.contains(expected),
                "prompt must embed {expected}, got {prompt}"
            );
            assert!(
                !prompt.contains("zh_cn") && !prompt.contains("zh_tw"),
                "prompt must not use serde wire ids"
            );
        }
    }

    #[test]
    fn normalize_generated_title_strips_markdown_and_caps_at_80_scalars() {
        assert_eq!(
            normalize_generated_title("## \"  修复 README   搜索  \"\nexplanation"),
            Some("修复 README 搜索".into())
        );
        assert_eq!(
            normalize_generated_title("* `hello world`"),
            Some("hello world".into())
        );
        let long: String = "字".repeat(100);
        let got = normalize_generated_title(&long).expect("title");
        assert_eq!(got.chars().count(), 80);
        assert_eq!(normalize_generated_title("   \n\t"), None);
    }
}
