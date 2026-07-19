//! Isolated hidden agent runner for on-demand document translation.
//!
//! Parallel to [`crate::auto_title::HiddenAgentRunner`] — does **not** extend
//! `TitleAgentRunner`. Uses `ConnectionPurpose::InternalTranslate` and
//! `InternalSessionPurpose::Translate` under reserved_root.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::time::{timeout, timeout_at, Instant};

use crate::acp::error::AcpError;
use crate::acp::manager::ConnectionManager;
use crate::acp::terminal_context::{build_acp_launch_inputs, AcpLaunchInputs, AcpRouteRequest};
use crate::acp::types::{AcpEvent, EventEnvelope, PromptInputBlock};
use crate::auto_title::internal_sessions::{InternalAgentSessionRegistry, InternalSessionPurpose};
use crate::auto_title::types::{ConnectionLaunchContext, ConnectionPurpose};
use crate::commands::acp::acp_get_agent_status_core;
use crate::commands::delegation::DelegationRuntimeSnapshot;
use crate::db::AppDatabase;
use crate::document_translate::types::{
    build_translate_prompt, DocumentTranslateError, DEADLINE_SECS, MAX_OUTPUT_BYTES,
};
use crate::models::agent::AgentType;
use crate::models::system::AppLocale;
use crate::web::event_bridge::EventEmitter;

/// Fixed owner label for internal translate connections (never a real window).
pub(crate) const INTERNAL_TRANSLATE_OWNER: &str = "internal:document-translate";

const DISCOVERY_LEASE_SECS: u64 = 15;
const DISCONNECT_CLEANUP_SECS: u64 = 5;

/// Production-facing translate runner contract (service consumes this).
#[async_trait]
pub trait DocumentTranslateAgent: Send + Sync {
    async fn run(
        &self,
        agent: AgentType,
        locale: AppLocale,
        body: &str,
    ) -> Result<String, DocumentTranslateError>;
}

/// Crate-private connection surface used by [`DocumentTranslateRunner`].
#[async_trait]
pub(crate) trait DocumentConnectionDriver: Send + Sync {
    async fn spawn_internal_translate(
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

pub(crate) fn internal_translate_event_emitter() -> EventEmitter {
    EventEmitter::Noop
}

/// Production driver that shares existing [`ConnectionManager`] internals.
pub struct ManagerDocumentConnectionDriver {
    manager: Arc<ConnectionManager>,
}

impl ManagerDocumentConnectionDriver {
    pub(crate) fn new(manager: Arc<ConnectionManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl DocumentConnectionDriver for ManagerDocumentConnectionDriver {
    async fn spawn_internal_translate(
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
                INTERNAL_TRANSLATE_OWNER.to_string(),
                internal_translate_event_emitter(),
                None,
                std::collections::BTreeMap::new(),
                ConnectionLaunchContext {
                    purpose: ConnectionPurpose::InternalTranslate,
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

/// Isolated translate runner: status → lease → spawn → register → prompt → collect → cleanup.
pub struct DocumentTranslateRunner {
    db: Arc<AppDatabase>,
    driver: Arc<dyn DocumentConnectionDriver>,
    registry: Arc<InternalAgentSessionRegistry>,
    data_dir: PathBuf,
}

impl DocumentTranslateRunner {
    pub(crate) fn new(
        db: Arc<AppDatabase>,
        driver: Arc<dyn DocumentConnectionDriver>,
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
impl DocumentTranslateAgent for DocumentTranslateRunner {
    async fn run(
        &self,
        agent: AgentType,
        locale: AppLocale,
        body: &str,
    ) -> Result<String, DocumentTranslateError> {
        let overall_deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);

        // --- status / availability ---
        let status = match phase(
            overall_deadline,
            acp_get_agent_status_core(agent, self.db.as_ref()),
        )
        .await
        {
            PhaseOutcome::Timeout => return Err(DocumentTranslateError::Timeout),
            PhaseOutcome::Ready(Ok(s)) => s,
            PhaseOutcome::Ready(Err(_)) => {
                return Err(DocumentTranslateError::Unavailable);
            }
        };
        if !status.available || !status.enabled {
            return Err(DocumentTranslateError::Unavailable);
        }

        // --- launch inputs ---
        let launch_inputs = match phase(
            overall_deadline,
            build_acp_launch_inputs(
                self.db.as_ref(),
                agent,
                None,
                &self.data_dir,
                AcpRouteRequest::root(None, None),
                &DelegationRuntimeSnapshot::default(),
            ),
        )
        .await
        {
            PhaseOutcome::Timeout => return Err(DocumentTranslateError::Timeout),
            PhaseOutcome::Ready(Ok(inputs)) => inputs,
            PhaseOutcome::Ready(Err(e)) => {
                return Err(DocumentTranslateError::Spawn(e.to_string()));
            }
        };

        let run_dir = self
            .registry
            .reserved_root()
            .join(uuid::Uuid::new_v4().to_string());
        if let Err(e) = std::fs::create_dir_all(&run_dir) {
            return Err(DocumentTranslateError::Spawn(format!("create run dir: {e}")));
        }

        let (guard, lease_deadline) =
            match acquire_discovery_lease(self.registry.as_ref(), overall_deadline).await {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = best_effort_remove_dir(&run_dir);
                    return Err(e);
                }
            };
        let mut lease = Some(guard);

        let spawn_result = phase(
            overall_deadline,
            self.driver
                .spawn_internal_translate(agent, run_dir.clone(), launch_inputs, locale),
        )
        .await;

        let conn_id = match spawn_result {
            PhaseOutcome::Timeout => {
                drop(lease.take());
                let _ = best_effort_remove_dir(&run_dir);
                return Err(DocumentTranslateError::Timeout);
            }
            PhaseOutcome::Ready(Err(e)) => {
                drop(lease.take());
                let _ = best_effort_remove_dir(&run_dir);
                return Err(DocumentTranslateError::Spawn(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(id)) => id,
        };

        let outcome = self
            .run_after_spawn(
                agent,
                locale,
                body,
                overall_deadline,
                lease_deadline,
                &mut lease,
                &conn_id,
            )
            .await;

        // Cleanup always: disconnect + rmdir even if caller dropped.
        cleanup_after_run(self.driver.as_ref(), &conn_id, &run_dir, lease.take()).await;

        outcome
    }
}

impl DocumentTranslateRunner {
    async fn run_after_spawn(
        &self,
        agent: AgentType,
        locale: AppLocale,
        body: &str,
        overall_deadline: Instant,
        lease_deadline: Instant,
        lease: &mut Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
        conn_id: &str,
    ) -> Result<String, DocumentTranslateError> {
        let (initial_id, mut rx) = match phase(
            overall_deadline,
            self.driver.identity_and_subscribe(conn_id),
        )
        .await
        {
            PhaseOutcome::Timeout => return Err(DocumentTranslateError::Timeout),
            PhaseOutcome::Ready(Err(e)) => {
                return Err(DocumentTranslateError::Identity(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(pair)) => pair,
        };

        let external_id = if let Some(id) = initial_id {
            id
        } else {
            match wait_for_session_identity(overall_deadline, lease_deadline, lease, &mut rx).await {
                Ok(id) => id,
                Err(e) => return Err(e),
            }
        };

        let reg_outcome = phase(overall_deadline, async {
            if let Some(ref mut guard) = lease {
                self.registry
                    .register_with_lease(
                        guard,
                        agent,
                        &external_id,
                        InternalSessionPurpose::Translate,
                    )
                    .await
            } else {
                self.registry
                    .register(agent, &external_id, InternalSessionPurpose::Translate)
                    .await
            }
        })
        .await;
        drop(lease.take());

        match reg_outcome {
            PhaseOutcome::Timeout => return Err(DocumentTranslateError::Timeout),
            PhaseOutcome::Ready(Err(e)) => {
                return Err(DocumentTranslateError::Registry(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(())) => {}
        }

        let prompt = build_translate_prompt(locale, body);
        let blocks = vec![PromptInputBlock::Text { text: prompt }];
        match phase(overall_deadline, self.driver.send_internal(conn_id, blocks)).await {
            PhaseOutcome::Timeout => return Err(DocumentTranslateError::Timeout),
            PhaseOutcome::Ready(Err(e)) => {
                return Err(DocumentTranslateError::Spawn(e.to_string()));
            }
            PhaseOutcome::Ready(Ok(())) => {}
        }

        collect_translate_output(overall_deadline, &mut rx).await
    }
}

enum PhaseOutcome<T> {
    Timeout,
    Ready(T),
}

async fn phase<F, T>(overall_deadline: Instant, fut: F) -> PhaseOutcome<T>
where
    F: std::future::Future<Output = T>,
{
    match timeout_at(overall_deadline, fut).await {
        Ok(v) => PhaseOutcome::Ready(v),
        Err(_) => PhaseOutcome::Timeout,
    }
}

async fn acquire_discovery_lease(
    registry: &InternalAgentSessionRegistry,
    overall_deadline: Instant,
) -> Result<(tokio::sync::OwnedRwLockWriteGuard<()>, Instant), DocumentTranslateError> {
    match phase(overall_deadline, registry.exclusive_discovery_lease()).await {
        PhaseOutcome::Timeout => Err(DocumentTranslateError::Timeout),
        PhaseOutcome::Ready(guard) => {
            let lease_deadline = Instant::now() + Duration::from_secs(DISCOVERY_LEASE_SECS);
            Ok((guard, lease_deadline))
        }
    }
}

async fn wait_for_session_identity(
    overall_deadline: Instant,
    lease_deadline: Instant,
    lease: &mut Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
    rx: &mut broadcast::Receiver<Arc<EventEnvelope>>,
) -> Result<String, DocumentTranslateError> {
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(overall_deadline) => {
                return Err(DocumentTranslateError::Timeout);
            }
            _ = tokio::time::sleep_until(lease_deadline), if lease.is_some() => {
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
                        return Err(DocumentTranslateError::Identity(
                            "private stream lagged before SessionStarted".into(),
                        ));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(DocumentTranslateError::Identity(
                            "private stream closed before SessionStarted".into(),
                        ));
                    }
                }
            }
        }
    }
}

async fn collect_translate_output(
    overall_deadline: Instant,
    rx: &mut broadcast::Receiver<Arc<EventEnvelope>>,
) -> Result<String, DocumentTranslateError> {
    let mut buf = String::new();
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(overall_deadline) => {
                return Err(DocumentTranslateError::Timeout);
            }
            msg = rx.recv() => {
                match msg {
                    Ok(envelope) => {
                        match &envelope.payload {
                            AcpEvent::ContentDelta { text } => {
                                let next_len = buf.len().saturating_add(text.len());
                                if next_len > MAX_OUTPUT_BYTES {
                                    return Err(DocumentTranslateError::OutputTooLarge);
                                }
                                buf.push_str(text);
                            }
                            AcpEvent::TurnComplete { stop_reason, .. } => {
                                if stop_reason == "end_turn" {
                                    if buf.trim().is_empty() {
                                        return Err(DocumentTranslateError::EmptyOutput);
                                    }
                                    return Ok(buf);
                                }
                                return Err(DocumentTranslateError::AbnormalStop(
                                    stop_reason.clone(),
                                ));
                            }
                            other => {
                                if let Some(err) = classify_stream_event(other) {
                                    return Err(err);
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        return Err(DocumentTranslateError::AbnormalStop(
                            "private stream lagged".into(),
                        ));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(DocumentTranslateError::AbnormalStop(
                            "private stream closed".into(),
                        ));
                    }
                }
            }
        }
    }
}

fn classify_stream_event(payload: &AcpEvent) -> Option<DocumentTranslateError> {
    match payload {
        AcpEvent::PermissionRequest { .. } | AcpEvent::QuestionRequest { .. } => {
            Some(DocumentTranslateError::Interactive)
        }
        AcpEvent::Error { message, .. } => {
            Some(DocumentTranslateError::AbnormalStop(message.clone()))
        }
        AcpEvent::StatusChanged { status }
            if matches!(
                status,
                crate::acp::types::ConnectionStatus::Disconnected
                    | crate::acp::types::ConnectionStatus::Error
            ) =>
        {
            Some(DocumentTranslateError::AbnormalStop(format!("{status:?}")))
        }
        _ => None,
    }
}

async fn cleanup_after_run(
    driver: &dyn DocumentConnectionDriver,
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
            tracing::warn!("[document_translate] disconnect during cleanup: {e}");
        }
        Err(_) => {
            tracing::warn!(
                "[document_translate] disconnect timed out after {DISCONNECT_CLEANUP_SECS}s for {conn_id}"
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

/// Inert runner for AppState tests that never expect a real translate call.
pub struct InertDocumentTranslateAgent;

#[async_trait]
impl DocumentTranslateAgent for InertDocumentTranslateAgent {
    async fn run(
        &self,
        _agent: AgentType,
        _locale: AppLocale,
        _body: &str,
    ) -> Result<String, DocumentTranslateError> {
        Err(DocumentTranslateError::Failed(
            "inert document translate agent".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use crate::web::event_bridge::emit_with_state;

    struct FakeAgent {
        conn_id: String,
        external_id: String,
        manager: ConnectionManager,
        _cmd_rx: StdMutex<
            Option<tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>>,
        >,
        prompt_count: AtomicUsize,
        disconnect_count: AtomicUsize,
        finish_text: StdMutex<Option<String>>,
        force_spawn_fail: AtomicBool,
        emit_started_before_subscribe: AtomicBool,
        last_prompt: StdMutex<Option<String>>,
        last_working_dir: StdMutex<Option<PathBuf>>,
    }

    impl FakeAgent {
        fn new(manager: ConnectionManager) -> Arc<Self> {
            Arc::new(Self {
                conn_id: "translate-conn-1".into(),
                external_id: "translate-session-1".into(),
                manager,
                _cmd_rx: StdMutex::new(None),
                prompt_count: AtomicUsize::new(0),
                disconnect_count: AtomicUsize::new(0),
                finish_text: StdMutex::new(None),
                force_spawn_fail: AtomicBool::new(false),
                emit_started_before_subscribe: AtomicBool::new(false),
                last_prompt: StdMutex::new(None),
                last_working_dir: StdMutex::new(None),
            })
        }

        async fn emit(&self, payload: AcpEvent) {
            let Some(state) = self.manager.get_state(&self.conn_id).await else {
                return;
            };
            emit_with_state(&state, &EventEmitter::Noop, payload).await;
        }

        fn finish_with(&self, text: &str) {
            *self.finish_text.lock().unwrap() = Some(text.to_string());
        }
    }

    struct FakeDocumentConnectionDriver {
        agent: Arc<FakeAgent>,
        agent_type: AgentType,
    }

    impl FakeDocumentConnectionDriver {
        fn new(agent: Arc<FakeAgent>) -> Arc<Self> {
            Arc::new(Self {
                agent,
                agent_type: AgentType::Codex,
            })
        }
    }

    #[async_trait]
    impl DocumentConnectionDriver for FakeDocumentConnectionDriver {
        async fn spawn_internal_translate(
            &self,
            _agent: AgentType,
            working_dir: PathBuf,
            _launch_inputs: AcpLaunchInputs,
            _locale: AppLocale,
        ) -> Result<String, AcpError> {
            *self.agent.last_working_dir.lock().unwrap() = Some(working_dir.clone());
            if self.agent.force_spawn_fail.load(Ordering::SeqCst) {
                return Err(AcpError::protocol("forced spawn failure"));
            }
            let rx = self
                .agent
                .manager
                .insert_test_connection_live(
                    &self.agent.conn_id,
                    self.agent_type,
                    Some(working_dir),
                    internal_translate_event_emitter(),
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
                s.purpose = ConnectionPurpose::InternalTranslate;
            }
            *self.agent._cmd_rx.lock().unwrap() = Some(rx);
            if self
                .agent
                .emit_started_before_subscribe
                .load(Ordering::SeqCst)
            {
                self.agent
                    .emit(AcpEvent::SessionStarted {
                        session_id: self.agent.external_id.clone(),
                    })
                    .await;
            }
            Ok(self.agent.conn_id.clone())
        }

        async fn identity_and_subscribe(
            &self,
            conn_id: &str,
        ) -> Result<(Option<String>, broadcast::Receiver<Arc<EventEnvelope>>), AcpError> {
            self.agent.manager.identity_and_subscribe(conn_id).await
        }

        async fn send_internal(
            &self,
            conn_id: &str,
            blocks: Vec<PromptInputBlock>,
        ) -> Result<(), AcpError> {
            self.agent.prompt_count.fetch_add(1, Ordering::SeqCst);
            if let Some(PromptInputBlock::Text { text }) = blocks.first() {
                *self.agent.last_prompt.lock().unwrap() = Some(text.clone());
            }
            let result = self
                .agent
                .manager
                .send_prompt_unlinked_internal(conn_id, blocks)
                .await;
            if result.is_ok() {
                let text = self
                    .agent
                    .finish_text
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "translated body".into());
                let agent = Arc::clone(&self.agent);
                tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    agent
                        .emit(AcpEvent::ContentDelta {
                            text: text.clone(),
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
                });
            }
            result
        }

        async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError> {
            self.agent.disconnect_count.fetch_add(1, Ordering::SeqCst);
            self.agent.manager.disconnect(conn_id).await
        }
    }

    async fn fixture() -> (
        DocumentTranslateRunner,
        Arc<FakeAgent>,
        tempfile::TempDir,
        Arc<InternalAgentSessionRegistry>,
    ) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        let registry =
            InternalAgentSessionRegistry::new_empty_for_test(db.conn.clone(), data_dir.path())
                .expect("registry");
        let manager = ConnectionManager::new();
        let agent = FakeAgent::new(manager);
        let driver = FakeDocumentConnectionDriver::new(Arc::clone(&agent));
        let runner = DocumentTranslateRunner::new(
            Arc::clone(&db),
            driver as Arc<dyn DocumentConnectionDriver>,
            Arc::clone(&registry),
            data_dir.path().to_path_buf(),
        );
        (runner, agent, data_dir, registry)
    }

    #[tokio::test]
    async fn fake_driver_happy_path_returns_body_and_disconnects() {
        let (runner, agent, _dir, registry) = fixture().await;
        agent.emit_started_before_subscribe.store(true, Ordering::SeqCst);
        agent.finish_with("Bonjour le monde");

        let out = runner
            .run(AgentType::Codex, AppLocale::Fr, "Hello world")
            .await
            .expect("translate");
        assert_eq!(out, "Bonjour le monde");
        assert!(agent.prompt_count.load(Ordering::SeqCst) >= 1);
        assert!(agent.disconnect_count.load(Ordering::SeqCst) >= 1);
        let wd = agent.last_working_dir.lock().unwrap().clone().unwrap();
        assert!(
            crate::auto_title::internal_sessions::is_lexically_below(
                &wd.to_string_lossy(),
                registry.reserved_root()
            ),
            "working_dir must be under reserved_root"
        );
        let prompt = agent.last_prompt.lock().unwrap().clone().unwrap();
        assert!(prompt.contains("French"));
        assert!(prompt.contains("Hello world"));
    }

    #[tokio::test]
    async fn spawn_failure_returns_spawn_error_and_no_dir_left() {
        let (runner, agent, _dir, registry) = fixture().await;
        agent.force_spawn_fail.store(true, Ordering::SeqCst);
        let err = runner
            .run(AgentType::Codex, AppLocale::En, "x")
            .await
            .expect_err("spawn fail");
        assert!(matches!(err, DocumentTranslateError::Spawn(_)));
        // reserved root itself remains; no leftover uuid dirs with connections
        let _ = registry.reserved_root();
    }

    #[tokio::test]
    async fn output_byte_cap_fails_closed() {
        let (runner, agent, _dir, _registry) = fixture().await;
        agent.emit_started_before_subscribe.store(true, Ordering::SeqCst);
        let huge = "x".repeat(MAX_OUTPUT_BYTES + 1);
        agent.finish_with(&huge);
        let err = runner
            .run(AgentType::Codex, AppLocale::En, "doc")
            .await
            .expect_err("oversize output");
        assert!(matches!(err, DocumentTranslateError::OutputTooLarge));
        assert!(agent.disconnect_count.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn noop_emitter_has_no_acp_bus() {
        let e = internal_translate_event_emitter();
        assert!(matches!(e, EventEmitter::Noop));
        assert!(e.acp_event_bus().is_none());
    }
}
