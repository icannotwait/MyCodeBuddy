//! Process-wide document translation admission and lifecycle.
//!
//! Capacity 1: busy reject (no queue). Detached owned task holds the permit
//! until the runner finishes disconnect+rmdir even if the request future is
//! dropped (HTTP client gone).
//!
//! Overall deadline starts at service entry (before protect/locale) so the
//! total wall budget stays within [`DEADLINE_SECS`] before cleanup.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Semaphore};
use tokio::time::Instant;

use crate::acp::manager::ConnectionManager;
use crate::auto_title::app_locale_to_wire;
use crate::auto_title::internal_sessions::InternalAgentSessionRegistry;
use crate::auto_title::parse_supported_app_locale;
use crate::commands::conversation_experience::load_document_translate_agent_from;
use crate::db::AppDatabase;
use crate::document_translate::protect::{
    protect_markdown, restore_markdown_detailed, ProtectError,
};
#[cfg(any(test, feature = "test-utils"))]
use crate::document_translate::runner::InertDocumentTranslateAgent;
use crate::document_translate::runner::{
    DocumentConnectionDriver, DocumentTranslateAgent, DocumentTranslateRunner,
    ManagerDocumentConnectionDriver,
};
use crate::document_translate::types::{
    DocumentTranslateError, DocumentTranslateFormat, TranslateDocumentParams,
    TranslateDocumentResult, DEADLINE_SECS, MAX_INPUT_SCALARS, TRANSLATE_CAPACITY,
};
use crate::models::system::AppLocale;

/// Process-wide translation service (shared by Tauri + Axum AppState).
pub struct DocumentTranslationService {
    db: Arc<AppDatabase>,
    runner: Arc<dyn DocumentTranslateAgent>,
    capacity: Arc<Semaphore>,
}

impl DocumentTranslationService {
    pub fn new(db: Arc<AppDatabase>, runner: Arc<dyn DocumentTranslateAgent>) -> Arc<Self> {
        Arc::new(Self {
            db,
            runner,
            capacity: Arc::new(Semaphore::new(TRANSLATE_CAPACITY)),
        })
    }

    /// Inert service for test AppState constructors that never call translate.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_inert(db: Arc<AppDatabase>) -> Arc<Self> {
        Self::new(db, Arc::new(InertDocumentTranslateAgent))
    }

    /// Translate a document: validate → admit → protect → run → restore.
    pub async fn translate(
        self: &Arc<Self>,
        params: TranslateDocumentParams,
    ) -> Result<TranslateDocumentResult, DocumentTranslateError> {
        // Wall-clock overall deadline arms at service entry so protect/locale/
        // agent load count toward the DEADLINE_SECS budget (cleanup is outside it).
        let overall_deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);

        // --- cheap validation before admission ---
        if params.content.trim().is_empty() {
            return Err(DocumentTranslateError::ContentEmpty);
        }
        let format = DocumentTranslateFormat::parse_wire(&params.format)?;

        // Protect Markdown before size check and capacity acquire so:
        // - eligibility is measured on the agent-facing body (code → placeholders)
        // - integrity setup does not hold capacity on collision (rare)
        let (body_for_agent, protected) = match format {
            DocumentTranslateFormat::Markdown => match protect_markdown(&params.content) {
                Ok(p) => {
                    let text = p.text.clone();
                    (text, Some(p))
                }
                Err(_) => {
                    return Err(DocumentTranslateError::Failed(
                        "failed to protect markdown code regions".into(),
                    ));
                }
            },
            DocumentTranslateFormat::PlainText => (params.content.clone(), None),
        };
        if body_for_agent.chars().count() > MAX_INPUT_SCALARS {
            return Err(DocumentTranslateError::ContentTooLarge);
        }

        let agent = match load_document_translate_agent_from(&self.db.conn).await {
            Ok(Some(a)) => a,
            Ok(None) => return Err(DocumentTranslateError::AgentNotConfigured),
            Err(e) => {
                return Err(DocumentTranslateError::Failed(format!(
                    "load document translate agent: {e}"
                )));
            }
        };

        let locale = resolve_locale(&self.db, params.locale.as_deref()).await;

        let permit = match Arc::clone(&self.capacity).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Err(DocumentTranslateError::Busy),
        };

        tracing::info!(
            agent = %agent,
            locale = ?locale,
            format = ?format,
            body_chars = body_for_agent.chars().count(),
            "[document_translate] admitted run"
        );

        let runner = Arc::clone(&self.runner);
        let (tx, rx) = oneshot::channel();

        // Owned task: permit held until run (including cleanup) completes,
        // even if the request future is cancelled / client disconnects.
        tokio::spawn(async move {
            let result = runner
                .run(agent, locale, &body_for_agent, overall_deadline)
                .await;
            let mapped = match result {
                Ok(raw) => {
                    if let Some(ref protected) = protected {
                        match restore_markdown_detailed(&raw, protected) {
                            Ok(outcome) => {
                                if outcome.inline_reorder_count > 0 {
                                    tracing::debug!(
                                        inline_reorder_count = outcome.inline_reorder_count,
                                        "[document_translate] accepted inline placeholder reordering"
                                    );
                                }
                                Ok(outcome.text)
                            }
                            Err(ProtectError::IntegrityFailed(kind)) => {
                                tracing::warn!(
                                    kind = ?kind,
                                    "[document_translate] placeholder integrity failed"
                                );
                                Err(DocumentTranslateError::PlaceholderIntegrity)
                            }
                            Err(ProtectError::NonceCollision) => {
                                Err(DocumentTranslateError::PlaceholderIntegrity)
                            }
                        }
                    } else {
                        Ok(raw)
                    }
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(mapped.map(|translated_content| TranslateDocumentResult {
                translated_content,
                locale: app_locale_to_wire(locale).to_string(),
                format,
            }));
            drop(permit);
        });

        match rx.await {
            Ok(Ok(result)) => {
                tracing::info!(
                    locale = %result.locale,
                    format = ?result.format,
                    chars = result.translated_content.chars().count(),
                    "[document_translate] completed successfully"
                );
                Ok(result)
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "[document_translate] run failed after admission"
                );
                Err(err)
            }
            Err(_) => {
                tracing::error!("[document_translate] owned task ended without result");
                Err(DocumentTranslateError::Failed(
                    "translation task ended without result".into(),
                ))
            }
        }
    }
}

async fn resolve_locale(db: &AppDatabase, wire: Option<&str>) -> AppLocale {
    if let Some(locale) = parse_supported_app_locale(wire) {
        return locale;
    }
    crate::commands::system_settings::load_system_language_settings(&db.conn)
        .await
        .map(|s| s.language)
        .unwrap_or_default()
}

/// Build the production service (manager driver + reserved-root runner).
pub fn build_production_document_translation_service(
    db: Arc<AppDatabase>,
    connection_manager: ConnectionManager,
    registry: Arc<InternalAgentSessionRegistry>,
    data_dir: PathBuf,
) -> Arc<DocumentTranslationService> {
    let driver: Arc<dyn DocumentConnectionDriver> = Arc::new(ManagerDocumentConnectionDriver::new(
        Arc::new(connection_manager.clone_ref()),
    ));
    let runner: Arc<dyn DocumentTranslateAgent> = Arc::new(DocumentTranslateRunner::new(
        Arc::clone(&db),
        driver,
        registry,
        data_dir,
    ));
    DocumentTranslationService::new(db, runner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use tokio::sync::Notify;

    use crate::commands::conversation_experience::KEY_AUTO_TITLE_AGENT;
    use crate::db::service::app_metadata_service;
    use crate::models::agent::AgentType;

    struct ControllableAgent {
        calls: AtomicUsize,
        /// When true, run blocks until `release` is notified.
        block: AtomicUsize,
        release: Notify,
        holding: AtomicUsize,
        response: StdMutex<Result<String, DocumentTranslateError>>,
        last_body: StdMutex<Option<String>>,
        last_deadline: StdMutex<Option<Instant>>,
    }

    impl ControllableAgent {
        fn new(response: Result<String, DocumentTranslateError>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                block: AtomicUsize::new(0),
                release: Notify::new(),
                holding: AtomicUsize::new(0),
                response: StdMutex::new(response),
                last_body: StdMutex::new(None),
                last_deadline: StdMutex::new(None),
            })
        }

        fn enable_block(&self) {
            self.block.store(1, Ordering::SeqCst);
        }

        fn release(&self) {
            self.release.notify_waiters();
        }
    }

    #[async_trait]
    impl DocumentTranslateAgent for ControllableAgent {
        async fn run(
            &self,
            _agent: AgentType,
            _locale: AppLocale,
            body: &str,
            overall_deadline: Instant,
        ) -> Result<String, DocumentTranslateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_body.lock().unwrap() = Some(body.to_string());
            *self.last_deadline.lock().unwrap() = Some(overall_deadline);
            if self.block.load(Ordering::SeqCst) != 0 {
                self.holding.fetch_add(1, Ordering::SeqCst);
                self.release.notified().await;
                self.holding.fetch_sub(1, Ordering::SeqCst);
            }
            self.response.lock().unwrap().clone()
        }
    }

    async fn wait_until_holding(agent: &ControllableAgent) {
        for _ in 0..200 {
            if agent.holding.load(Ordering::SeqCst) > 0 {
                return;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("runner never entered holding state");
    }

    async fn db_with_agent(agent: Option<AgentType>) -> Arc<AppDatabase> {
        let db = Arc::new(crate::db::test_helpers::fresh_in_memory_db().await);
        if let Some(agent) = agent {
            let raw = serde_json::to_string(&agent).expect("agent json");
            app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, &raw)
                .await
                .expect("set agent");
        }
        db
    }

    fn params(content: &str, format: &str) -> TranslateDocumentParams {
        TranslateDocumentParams {
            content: content.into(),
            format: format.into(),
            locale: Some("en".into()),
            display_name: Some("doc.md".into()),
        }
    }

    #[tokio::test]
    async fn agent_none_rejects_without_calling_runner() {
        let db = db_with_agent(None).await;
        let agent = ControllableAgent::new(Ok("x".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let err = svc
            .translate(params("hello", "plainText"))
            .await
            .expect_err("agent none");
        assert_eq!(err, DocumentTranslateError::AgentNotConfigured);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn empty_content_rejects() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("x".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let err = svc
            .translate(params("   \n\t  ", "plainText"))
            .await
            .expect_err("empty");
        assert_eq!(err, DocumentTranslateError::ContentEmpty);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oversize_content_rejects() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("x".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let big = "a".repeat(MAX_INPUT_SCALARS + 1);
        let err = svc
            .translate(params(&big, "plainText"))
            .await
            .expect_err("oversize");
        assert_eq!(err, DocumentTranslateError::ContentTooLarge);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oversize_protected_markdown_rejects() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("x".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        // No fenced code → protected body ≈ raw; still over limit.
        let big = "a".repeat(MAX_INPUT_SCALARS + 1);
        let err = svc
            .translate(params(&big, "markdown"))
            .await
            .expect_err("oversize protected");
        assert_eq!(err, DocumentTranslateError::ContentTooLarge);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn size_limit_uses_protected_body_not_raw() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        // Placeholder-less reply → restore fails after admission; that is fine.
        let agent = ControllableAgent::new(Ok("translated".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let huge_code = "x".repeat(MAX_INPUT_SCALARS + 2_000);
        let content = format!("Intro prose\n\n```\n{huge_code}\n```\n\nOutro prose");
        assert!(
            content.chars().count() > MAX_INPUT_SCALARS,
            "raw must exceed limit so this tests post-protect admission"
        );

        let result = svc.translate(params(&content, "markdown")).await;
        assert_eq!(
            agent.calls.load(Ordering::SeqCst),
            1,
            "admission must pass when protected body is under limit"
        );
        let body = agent
            .last_body
            .lock()
            .unwrap()
            .clone()
            .expect("runner body recorded");
        assert!(
            body.chars().count() <= MAX_INPUT_SCALARS,
            "agent body must be the protected (shrunken) document"
        );
        assert!(
            !body.contains(&huge_code),
            "fenced code must not be sent raw to the agent"
        );
        assert!(
            !matches!(result, Err(DocumentTranslateError::ContentTooLarge)),
            "must not reject on raw size when protected size is fine: {result:?}"
        );
    }

    #[tokio::test]
    async fn unsupported_format_rejects_with_domain_error() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("x".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let err = svc
            .translate(params("hello", "docx"))
            .await
            .expect_err("unsupported");
        assert_eq!(err, DocumentTranslateError::UnsupportedFormat);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 0);
        let app = err.into_app_command_error();
        assert_eq!(
            app.i18n_key.as_deref(),
            Some(crate::document_translate::types::I18N_UNSUPPORTED_FORMAT)
        );
    }

    #[tokio::test]
    async fn busy_second_call_while_first_holds() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("ok".into()));
        agent.enable_block();
        let svc = DocumentTranslationService::new(
            Arc::clone(&db),
            agent.clone() as Arc<dyn DocumentTranslateAgent>,
        );

        let svc1 = Arc::clone(&svc);
        let first =
            tokio::spawn(
                async move { svc1.translate(params("first document", "plainText")).await },
            );

        wait_until_holding(&agent).await;
        assert_eq!(agent.holding.load(Ordering::SeqCst), 1);

        let err = svc
            .translate(params("second", "plainText"))
            .await
            .expect_err("busy");
        assert_eq!(err, DocumentTranslateError::Busy);

        agent.release();
        let ok = first.await.expect("join").expect("first ok");
        assert_eq!(ok.translated_content, "ok");
        assert_eq!(ok.locale, "en");
    }

    #[tokio::test]
    async fn busy_while_first_still_cleaning_spawn_timeout() {
        // Contract: service capacity stays occupied for the full `run()`
        // lifetime. On spawn-timeout the runner awaits JoinHandle settle +
        // disconnect/rmdir before returning Timeout — so a second translate
        // must see Busy while that cleanup hold is active.
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Err(DocumentTranslateError::Timeout));
        agent.enable_block();
        let svc = DocumentTranslationService::new(
            Arc::clone(&db),
            agent.clone() as Arc<dyn DocumentTranslateAgent>,
        );

        let svc1 = Arc::clone(&svc);
        let first =
            tokio::spawn(
                async move { svc1.translate(params("first document", "plainText")).await },
            );

        wait_until_holding(&agent).await;
        assert_eq!(agent.holding.load(Ordering::SeqCst), 1);

        let err = svc
            .translate(params("second", "plainText"))
            .await
            .expect_err("busy during spawn-timeout cleanup");
        assert_eq!(err, DocumentTranslateError::Busy);

        agent.release();
        let first_result = first.await.expect("join");
        assert!(
            matches!(first_result, Err(DocumentTranslateError::Timeout)),
            "first returns Timeout only after cleanup hold ends: {first_result:?}"
        );

        // Permit free after cleanup: a non-blocking run must admit.
        agent.block.store(0, Ordering::SeqCst);
        *agent.response.lock().unwrap() = Ok("after-cleanup".into());
        let ok = svc
            .translate(params("third", "plainText"))
            .await
            .expect("capacity free after spawn cleanup");
        assert_eq!(ok.translated_content, "after-cleanup");
    }

    #[tokio::test]
    async fn plaintext_happy_path_calls_runner() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("translated".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let result = svc
            .translate(params("Hello", "plainText"))
            .await
            .expect("ok");
        assert_eq!(result.translated_content, "translated");
        assert_eq!(result.format, DocumentTranslateFormat::PlainText);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 1);
        assert_eq!(agent.last_body.lock().unwrap().as_deref(), Some("Hello"));
        // Deadline is armed before the runner is invoked (service entry).
        let deadline = agent.last_deadline.lock().unwrap().expect("deadline");
        assert!(
            deadline > Instant::now(),
            "deadline should still be in the future on a fast path"
        );
        let remaining = deadline.duration_since(Instant::now());
        assert!(
            remaining <= Duration::from_secs(DEADLINE_SECS),
            "remaining must not exceed DEADLINE_SECS"
        );
    }

    #[tokio::test]
    async fn same_language_still_calls_runner() {
        // English content + English locale still runs (no short-circuit).
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("Hello again".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let mut p = params("Hello", "plainText");
        p.locale = Some("en".into());
        let _ = svc.translate(p).await.expect("ok");
        assert_eq!(agent.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn markdown_placeholder_integrity_fail() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        // Return body without required placeholders.
        let agent = ControllableAgent::new(Ok("translated without tokens".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let content = "See `code` here";
        let err = svc
            .translate(params(content, "markdown"))
            .await
            .expect_err("integrity");
        assert_eq!(err, DocumentTranslateError::PlaceholderIntegrity);
        // Runner still called with protected body (placeholders present).
        assert_eq!(agent.calls.load(Ordering::SeqCst), 1);
        let body = agent.last_body.lock().unwrap().clone().unwrap();
        assert!(body.contains('⟦') || body != content);
    }

    #[tokio::test]
    async fn markdown_happy_path_restores_code() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        struct EchoAgent;
        #[async_trait]
        impl DocumentTranslateAgent for EchoAgent {
            async fn run(
                &self,
                _agent: AgentType,
                _locale: AppLocale,
                body: &str,
                _overall_deadline: Instant,
            ) -> Result<String, DocumentTranslateError> {
                // Simulate model keeping placeholders, translating prose.
                Ok(format!("TR: {body}"))
            }
        }
        let svc = DocumentTranslationService::new(db, Arc::new(EchoAgent));
        let content = "Hello `code` world";
        let result = svc
            .translate(params(content, "markdown"))
            .await
            .expect("ok");
        assert!(result.translated_content.starts_with("TR: "));
        assert!(result.translated_content.contains("`code`"));
        assert!(!result.translated_content.contains('⟦'));
    }

    fn take_current_nonce_tokens(body: &str, kind: &str) -> Vec<String> {
        let prefix = format!("⟦{kind}_");
        let close = '⟧';
        let mut tokens = Vec::new();
        let mut rest = body;
        while let Some(start) = rest.find(&prefix) {
            let after = &rest[start + prefix.len()..];
            let Some(end) = after.find(close) else {
                break;
            };
            tokens.push(rest[start..start + prefix.len() + end + close.len_utf8()].to_string());
            rest = &after[end + close.len_utf8()..];
        }
        tokens
    }

    #[tokio::test]
    async fn markdown_same_region_inline_reorder_restores_original_spans() {
        struct ReorderAgent;
        #[async_trait]
        impl DocumentTranslateAgent for ReorderAgent {
            async fn run(
                &self,
                _agent: AgentType,
                _locale: AppLocale,
                body: &str,
                _overall_deadline: Instant,
            ) -> Result<String, DocumentTranslateError> {
                let tokens = take_current_nonce_tokens(body, "CGINLINE");
                assert_eq!(
                    tokens.len(),
                    2,
                    "protected body must contain two CGINLINE tokens"
                );
                let t0 = &tokens[0];
                let t1 = &tokens[1];
                Ok(format!("It queries-from {t1} the fields {t0}."))
            }
        }

        let db = db_with_agent(Some(AgentType::Codex)).await;
        let svc = DocumentTranslationService::new(db, Arc::new(ReorderAgent));
        let result = svc
            .translate(params("It queries `id, data` from `blobs`.", "markdown"))
            .await
            .expect("same-region inline reorder must succeed");
        assert_eq!(
            result.translated_content,
            "It queries-from `blobs` the fields `id, data`."
        );
        assert!(!result.translated_content.contains('⟦'));
    }

    #[tokio::test]
    async fn markdown_fenced_reorder_still_maps_to_placeholder_integrity() {
        struct SwapFences;
        #[async_trait]
        impl DocumentTranslateAgent for SwapFences {
            async fn run(
                &self,
                _agent: AgentType,
                _locale: AppLocale,
                body: &str,
                _overall_deadline: Instant,
            ) -> Result<String, DocumentTranslateError> {
                let tokens = take_current_nonce_tokens(body, "CGCODE");
                assert_eq!(tokens.len(), 2, "expected two CGCODE tokens");
                let swapped = body
                    .replacen(&tokens[0], "@@TMP@@", 1)
                    .replacen(&tokens[1], &tokens[0], 1)
                    .replacen("@@TMP@@", &tokens[1], 1);
                Ok(swapped)
            }
        }

        let db = db_with_agent(Some(AgentType::Codex)).await;
        let svc = DocumentTranslationService::new(db, Arc::new(SwapFences));
        let content = "```\none\n```\n\n```\ntwo\n```\n";
        let err = svc
            .translate(params(content, "markdown"))
            .await
            .expect_err("fenced reorder must fail closed");
        assert_eq!(err, DocumentTranslateError::PlaceholderIntegrity);
        let app = err.into_app_command_error();
        assert_eq!(
            app.i18n_key.as_deref(),
            Some(crate::document_translate::types::I18N_PLACEHOLDER)
        );
    }

    #[tokio::test]
    async fn invalid_locale_falls_back_to_system_default() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("ok".into()));
        let svc = DocumentTranslationService::new(db, agent as Arc<dyn DocumentTranslateAgent>);
        let mut p = params("Hello", "plainText");
        p.locale = Some("not-a-locale".into());
        let result = svc.translate(p).await.expect("ok");
        // System default is English.
        assert_eq!(result.locale, "en");
    }
}
