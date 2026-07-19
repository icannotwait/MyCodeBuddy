//! Process-wide document translation admission and lifecycle.
//!
//! Capacity 1: busy reject (no queue). Detached owned task holds the permit
//! until the runner finishes disconnect+rmdir even if the request future is
//! dropped (HTTP client gone).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{oneshot, Semaphore};

use crate::acp::manager::ConnectionManager;
use crate::auto_title::app_locale_to_wire;
use crate::auto_title::internal_sessions::InternalAgentSessionRegistry;
use crate::auto_title::parse_supported_app_locale;
use crate::commands::conversation_experience::load_auto_title_agent_from;
use crate::db::AppDatabase;
use crate::document_translate::protect::{protect_markdown, restore_markdown};
use crate::document_translate::runner::{
    DocumentConnectionDriver, DocumentTranslateAgent, DocumentTranslateRunner,
    ManagerDocumentConnectionDriver,
};
#[cfg(any(test, feature = "test-utils"))]
use crate::document_translate::runner::InertDocumentTranslateAgent;
use crate::document_translate::types::{
    DocumentTranslateError, DocumentTranslateFormat, TranslateDocumentParams,
    TranslateDocumentResult, MAX_INPUT_SCALARS, TRANSLATE_CAPACITY,
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
        // --- cheap validation before admission ---
        if params.content.trim().is_empty() {
            return Err(DocumentTranslateError::ContentEmpty);
        }
        if params.content.chars().count() > MAX_INPUT_SCALARS {
            return Err(DocumentTranslateError::ContentTooLarge);
        }

        let agent = match load_auto_title_agent_from(&self.db.conn).await {
            Ok(Some(a)) => a,
            Ok(None) => return Err(DocumentTranslateError::AgentNotConfigured),
            Err(e) => {
                return Err(DocumentTranslateError::Failed(format!(
                    "load auto title agent: {e}"
                )));
            }
        };

        let locale = resolve_locale(&self.db, params.locale.as_deref()).await;
        let format = params.format;

        // Protect Markdown before acquiring so integrity setup does not hold
        // capacity on collision (rare). Protect is pure/fast.
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

        let permit = match Arc::clone(&self.capacity).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Err(DocumentTranslateError::Busy),
        };

        let runner = Arc::clone(&self.runner);
        let (tx, rx) = oneshot::channel();

        // Owned task: permit held until run (including cleanup) completes,
        // even if the request future is cancelled / client disconnects.
        tokio::spawn(async move {
            let result = runner.run(agent, locale, &body_for_agent).await;
            let mapped = match result {
                Ok(raw) => {
                    if let Some(ref protected) = protected {
                        match restore_markdown(&raw, protected) {
                            Ok(restored) => Ok(restored),
                            Err(_) => Err(DocumentTranslateError::PlaceholderIntegrity),
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
            Ok(outcome) => outcome,
            Err(_) => Err(DocumentTranslateError::Failed(
                "translation task ended without result".into(),
            )),
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
        ) -> Result<String, DocumentTranslateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_body.lock().unwrap() = Some(body.to_string());
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

    fn params(content: &str, format: DocumentTranslateFormat) -> TranslateDocumentParams {
        TranslateDocumentParams {
            content: content.into(),
            format,
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
            .translate(params("hello", DocumentTranslateFormat::PlainText))
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
            .translate(params("   \n\t  ", DocumentTranslateFormat::PlainText))
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
            .translate(params(&big, DocumentTranslateFormat::PlainText))
            .await
            .expect_err("oversize");
        assert_eq!(err, DocumentTranslateError::ContentTooLarge);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 0);
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
        let first = tokio::spawn(async move {
            svc1.translate(params("first document", DocumentTranslateFormat::PlainText))
                .await
        });

        wait_until_holding(&agent).await;
        assert_eq!(agent.holding.load(Ordering::SeqCst), 1);

        let err = svc
            .translate(params("second", DocumentTranslateFormat::PlainText))
            .await
            .expect_err("busy");
        assert_eq!(err, DocumentTranslateError::Busy);

        agent.release();
        let ok = first.await.expect("join").expect("first ok");
        assert_eq!(ok.translated_content, "ok");
        assert_eq!(ok.locale, "en");
    }

    #[tokio::test]
    async fn plaintext_happy_path_calls_runner() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("translated".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let result = svc
            .translate(params("Hello", DocumentTranslateFormat::PlainText))
            .await
            .expect("ok");
        assert_eq!(result.translated_content, "translated");
        assert_eq!(result.format, DocumentTranslateFormat::PlainText);
        assert_eq!(agent.calls.load(Ordering::SeqCst), 1);
        assert_eq!(agent.last_body.lock().unwrap().as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn same_language_still_calls_runner() {
        // English content + English locale still runs (no short-circuit).
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("Hello again".into()));
        let svc =
            DocumentTranslationService::new(db, agent.clone() as Arc<dyn DocumentTranslateAgent>);
        let mut p = params("Hello", DocumentTranslateFormat::PlainText);
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
            .translate(params(content, DocumentTranslateFormat::Markdown))
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
            ) -> Result<String, DocumentTranslateError> {
                // Simulate model keeping placeholders, translating prose.
                Ok(format!("TR: {body}"))
            }
        }
        let svc = DocumentTranslationService::new(db, Arc::new(EchoAgent));
        let content = "Hello `code` world";
        let result = svc
            .translate(params(content, DocumentTranslateFormat::Markdown))
            .await
            .expect("ok");
        assert!(result.translated_content.starts_with("TR: "));
        assert!(result.translated_content.contains("`code`"));
        assert!(!result.translated_content.contains('⟦'));
    }

    #[tokio::test]
    async fn invalid_locale_falls_back_to_system_default() {
        let db = db_with_agent(Some(AgentType::Codex)).await;
        let agent = ControllableAgent::new(Ok("ok".into()));
        let svc = DocumentTranslationService::new(db, agent as Arc<dyn DocumentTranslateAgent>);
        let mut p = params("Hello", DocumentTranslateFormat::PlainText);
        p.locale = Some("not-a-locale".into());
        let result = svc.translate(p).await.expect("ok");
        // System default is English.
        assert_eq!(result.locale, "en");
    }
}
