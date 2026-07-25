//! Direct OpenAI-compatible chat-completions runner for automatic titles.
//!
//! Production uses a lazy reqwest client (built on first request so proxy env
//! from `init_proxy_from_db` is already applied). Tests inject
//! [`TitleHttpTransport`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::auto_title::runner::{build_title_prompt, normalize_generated_title};
use crate::auto_title::types::{AutoTitleAttempt, AutoTitleRunError};

/// HTTP timeout for a single title completion request.
const TITLE_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// ── Transport surface ───────────────────────────────────────────────────────

/// Minimal response from a title completion POST.
#[derive(Debug, Clone)]
pub struct TitleHttpResponse {
    pub status: u16,
    pub body: String,
}

/// Safe transport errors only — never carry URL, bearer, prompt, or body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TitleHttpError {
    #[error("title http cancelled")]
    Cancelled,
    #[error("title http timeout")]
    Timeout,
    #[error("title http transport error")]
    Transport,
}

/// Injectable HTTP transport for title completions.
#[async_trait]
pub trait TitleHttpTransport: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        bearer: &str,
        body: &Value,
        cancel: &CancellationToken,
    ) -> Result<TitleHttpResponse, TitleHttpError>;
}

/// Production transport: builds `reqwest::Client` lazily on first use so
/// process proxy env applied by `init_proxy_from_db` is observed.
///
/// **Factory contract:** constructing this struct must not build a client.
/// Only the first [`post_json`] (or an explicit `ensure_client` in tests)
/// constructs reqwest.
pub struct LazyReqwestTitleTransport {
    client: std::sync::OnceLock<reqwest::Client>,
}

impl LazyReqwestTitleTransport {
    pub fn new() -> Self {
        Self {
            client: std::sync::OnceLock::new(),
        }
    }

    /// True when a client has already been constructed (tests / diagnostics).
    pub fn client_constructed(&self) -> bool {
        self.client.get().is_some()
    }

    fn client(&self) -> Result<&reqwest::Client, TitleHttpError> {
        if let Some(c) = self.client.get() {
            return Ok(c);
        }
        let built = reqwest::Client::builder()
            .timeout(TITLE_HTTP_TIMEOUT)
            .build()
            .map_err(|_| TitleHttpError::Transport)?;
        let _ = self.client.set(built);
        self.client.get().ok_or(TitleHttpError::Transport)
    }
}

impl Default for LazyReqwestTitleTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TitleHttpTransport for LazyReqwestTitleTransport {
    async fn post_json(
        &self,
        url: &str,
        bearer: &str,
        body: &Value,
        cancel: &CancellationToken,
    ) -> Result<TitleHttpResponse, TitleHttpError> {
        if cancel.is_cancelled() {
            return Err(TitleHttpError::Cancelled);
        }
        let client = self.client()?;
        let request = client
            .post(url)
            .header("Authorization", format!("Bearer {bearer}"))
            .header("Content-Type", "application/json")
            .json(body);

        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(TitleHttpError::Cancelled),
            result = request.send() => {
                match result {
                    Ok(resp) => resp,
                    Err(err) if err.is_timeout() => return Err(TitleHttpError::Timeout),
                    Err(err) if err.is_connect() || err.is_request() => {
                        // Cancelled mid-flight can surface as request error.
                        if cancel.is_cancelled() {
                            return Err(TitleHttpError::Cancelled);
                        }
                        return Err(TitleHttpError::Transport);
                    }
                    Err(_) => return Err(TitleHttpError::Transport),
                }
            }
        };

        let status = response.status().as_u16();
        let body = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(TitleHttpError::Cancelled),
            result = response.text() => {
                result.map_err(|_| TitleHttpError::Transport)?
            }
        };

        Ok(TitleHttpResponse { status, body })
    }
}

// ── Endpoint + response helpers ─────────────────────────────────────────────

/// Defensive endpoint normalization for the runner (settings already validate
/// on save). Never put the resulting URL into errors.
///
/// 1. Trim; parse; reject userinfo; scheme/host/port/path only.
/// 2. Strip trailing `/` on path (except root).
/// 3. If path ends with `/chat/completions`, use as-is; else append it.
pub fn normalize_chat_completions_url(raw: &str) -> Result<String, AutoTitleRunError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AutoTitleRunError::Unavailable);
    }

    let mut parsed = reqwest::Url::parse(trimmed).map_err(|_| AutoTitleRunError::Unavailable)?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err(AutoTitleRunError::Unavailable),
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AutoTitleRunError::Unavailable);
    }

    if parsed.host_str().is_none() {
        return Err(AutoTitleRunError::Unavailable);
    }

    parsed.set_query(None);
    parsed.set_fragment(None);

    let mut path = parsed.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if !path.ends_with("/chat/completions") {
        if path == "/" || path.is_empty() {
            path = "/chat/completions".to_string();
        } else {
            path = format!("{path}/chat/completions");
        }
    }
    parsed.set_path(&path);

    Ok(parsed.to_string())
}

/// Extract `choices[0].message.content` as a plain string, or concatenate
/// array text parts. Empty / missing → `None`.
pub fn extract_completion_content(body: &str) -> Result<Option<String>, AutoTitleRunError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| AutoTitleRunError::AbnormalStop("invalid_json".into()))?;

    let content = value
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"));

    let Some(content) = content else {
        return Ok(None);
    };

    match content {
        Value::String(s) => Ok(Some(s.clone())),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                } else if let Some(t) = part.as_str() {
                    out.push_str(t);
                }
            }
            if out.is_empty() {
                Ok(None)
            } else {
                Ok(Some(out))
            }
        }
        _ => Ok(None),
    }
}

// ── Runner ──────────────────────────────────────────────────────────────────

/// Production title runner: one non-streaming chat-completions POST.
pub struct DirectCompletionTitleRunner {
    transport: Arc<dyn TitleHttpTransport>,
}

impl DirectCompletionTitleRunner {
    pub fn new(transport: Arc<dyn TitleHttpTransport>) -> Self {
        Self { transport }
    }

    /// Convenience constructor with the production lazy reqwest transport.
    pub fn with_lazy_reqwest() -> Self {
        Self::new(Arc::new(LazyReqwestTitleTransport::new()))
    }
}

#[async_trait]
impl crate::auto_title::runner::TitleAgentRunner for DirectCompletionTitleRunner {
    async fn run(
        &self,
        attempt: AutoTitleAttempt,
        cancellation: CancellationToken,
    ) -> Result<String, AutoTitleRunError> {
        if cancellation.is_cancelled() {
            return Err(AutoTitleRunError::Cancelled);
        }

        if !attempt.config.is_enabled() {
            return Err(AutoTitleRunError::Unavailable);
        }

        let endpoint = normalize_chat_completions_url(&attempt.config.api_url)?;
        let prompt = build_title_prompt(
            attempt.locale,
            &attempt.first_user_text,
            &attempt.first_assistant_text,
        );

        let body = json!({
            "model": attempt.config.model.trim(),
            "stream": false,
            "temperature": 0,
            "max_tokens": 128,
            "messages": [
                {
                    "role": "user",
                    "content": prompt,
                }
            ]
        });

        let response = self
            .transport
            .post_json(
                &endpoint,
                attempt.config.api_key.trim(),
                &body,
                &cancellation,
            )
            .await
            .map_err(|e| match e {
                TitleHttpError::Cancelled => AutoTitleRunError::Cancelled,
                TitleHttpError::Timeout => AutoTitleRunError::Timeout,
                TitleHttpError::Transport => {
                    AutoTitleRunError::AbnormalStop("transport_error".into())
                }
            })?;

        match response.status {
            401 | 403 => return Err(AutoTitleRunError::Unavailable),
            200..=299 => {}
            code => {
                return Err(AutoTitleRunError::AbnormalStop(format!(
                    "http_status={code}"
                )));
            }
        }

        let content = extract_completion_content(&response.body)?;
        let Some(raw) = content else {
            return Err(AutoTitleRunError::EmptyOutput);
        };
        let Some(title) = normalize_generated_title(&raw) else {
            return Err(AutoTitleRunError::EmptyOutput);
        };

        tracing::info!(
            conversation_id = attempt.conversation_id,
            attempt = attempt.attempt,
            config_gen = attempt.config_gen,
            "auto-title direct completion succeeded"
        );

        Ok(title)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::auto_title::runner::TitleAgentRunner;
    use crate::auto_title::types::AutoTitleApiConfig;
    use crate::models::system::AppLocale;

    fn sample_config() -> AutoTitleApiConfig {
        AutoTitleApiConfig {
            api_url: "https://api.example.com/v1".into(),
            api_key: "sk-test-secret-key".into(),
            model: "gpt-4o-mini".into(),
        }
    }

    fn sample_attempt() -> AutoTitleAttempt {
        AutoTitleAttempt {
            conversation_id: 7,
            attempt: 1,
            locale: AppLocale::En,
            first_user_text: "Fix the README".into(),
            first_assistant_text: "Updated docs".into(),
            config: sample_config(),
            config_gen: 3,
        }
    }

    struct MockTransport {
        /// When set, next call returns this result.
        next: Mutex<Option<Result<TitleHttpResponse, TitleHttpError>>>,
        calls: AtomicUsize,
        last_url: Mutex<Option<String>>,
        last_bearer: Mutex<Option<String>>,
        last_body: Mutex<Option<Value>>,
        hang_until_cancel: AtomicBool,
    }

    impl MockTransport {
        fn with_response(resp: TitleHttpResponse) -> Arc<Self> {
            Arc::new(Self {
                next: Mutex::new(Some(Ok(resp))),
                calls: AtomicUsize::new(0),
                last_url: Mutex::new(None),
                last_bearer: Mutex::new(None),
                last_body: Mutex::new(None),
                hang_until_cancel: AtomicBool::new(false),
            })
        }

        fn with_error(err: TitleHttpError) -> Arc<Self> {
            Arc::new(Self {
                next: Mutex::new(Some(Err(err))),
                calls: AtomicUsize::new(0),
                last_url: Mutex::new(None),
                last_bearer: Mutex::new(None),
                last_body: Mutex::new(None),
                hang_until_cancel: AtomicBool::new(false),
            })
        }

        fn hang() -> Arc<Self> {
            Arc::new(Self {
                next: Mutex::new(None),
                calls: AtomicUsize::new(0),
                last_url: Mutex::new(None),
                last_bearer: Mutex::new(None),
                last_body: Mutex::new(None),
                hang_until_cancel: AtomicBool::new(true),
            })
        }
    }

    #[async_trait]
    impl TitleHttpTransport for MockTransport {
        async fn post_json(
            &self,
            url: &str,
            bearer: &str,
            body: &Value,
            cancel: &CancellationToken,
        ) -> Result<TitleHttpResponse, TitleHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_url.lock().unwrap() = Some(url.to_string());
            *self.last_bearer.lock().unwrap() = Some(bearer.to_string());
            *self.last_body.lock().unwrap() = Some(body.clone());

            if self.hang_until_cancel.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err(TitleHttpError::Cancelled);
            }

            self.next
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(TitleHttpError::Transport))
        }
    }

    #[test]
    fn normalize_appends_chat_completions() {
        assert_eq!(
            normalize_chat_completions_url("https://api.example.com/v1").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_completions_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_completions_url("https://api.example.com/v1/chat/completions").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_completions_url("https://api.example.com/v1/chat/completions/").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn normalize_rejects_userinfo_and_empty() {
        assert!(matches!(
            normalize_chat_completions_url("https://user:pass@api.example.com/v1"),
            Err(AutoTitleRunError::Unavailable)
        ));
        assert!(matches!(
            normalize_chat_completions_url("   "),
            Err(AutoTitleRunError::Unavailable)
        ));
        assert!(matches!(
            normalize_chat_completions_url("ftp://api.example.com/v1"),
            Err(AutoTitleRunError::Unavailable)
        ));
    }

    #[test]
    fn extract_content_string_and_array_parts() {
        let s = r#"{"choices":[{"message":{"content":"  Hello title  "}}]}"#;
        assert_eq!(
            extract_completion_content(s).unwrap().as_deref(),
            Some("  Hello title  ")
        );

        let arr = r#"{"choices":[{"message":{"content":[{"text":"Part "},{"text":"A"}]}}]}"#;
        assert_eq!(
            extract_completion_content(arr).unwrap().as_deref(),
            Some("Part A")
        );

        let missing = r#"{"choices":[]}"#;
        assert_eq!(extract_completion_content(missing).unwrap(), None);

        assert!(matches!(
            extract_completion_content("not-json"),
            Err(AutoTitleRunError::AbnormalStop(msg)) if msg == "invalid_json"
        ));
    }

    #[tokio::test]
    async fn runner_happy_path_normalizes_title() {
        let mock = MockTransport::with_response(TitleHttpResponse {
            status: 200,
            body: serde_json::json!({
                "choices": [{
                    "message": { "content": "## \"  Fix README  \"\n" }
                }]
            })
            .to_string(),
        });
        let runner = DirectCompletionTitleRunner::new(mock.clone());
        let title = runner
            .run(sample_attempt(), CancellationToken::new())
            .await
            .expect("title");
        assert_eq!(title, "Fix README");
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);

        let url = mock.last_url.lock().unwrap().clone().unwrap();
        assert!(url.ends_with("/chat/completions"));
        assert!(!url.contains("sk-"));

        let body = mock.last_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["stream"], false);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], 128);
        assert_eq!(body["model"], "gpt-4o-mini");
    }

    #[tokio::test]
    async fn runner_401_maps_to_unavailable() {
        let mock = MockTransport::with_response(TitleHttpResponse {
            status: 401,
            body: r#"{"error":"bad key sk-leaked"}"#.into(),
        });
        let runner = DirectCompletionTitleRunner::new(mock);
        let err = runner
            .run(sample_attempt(), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err, AutoTitleRunError::Unavailable);
        let msg = err.to_string();
        assert!(!msg.contains("sk-"));
        assert!(!msg.contains("api.example"));
    }

    #[tokio::test]
    async fn runner_empty_content_is_empty_output() {
        let mock = MockTransport::with_response(TitleHttpResponse {
            status: 200,
            body: r#"{"choices":[{"message":{"content":"   \n\t  "}}]}"#.into(),
        });
        let runner = DirectCompletionTitleRunner::new(mock);
        let err = runner
            .run(sample_attempt(), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err, AutoTitleRunError::EmptyOutput);
    }

    #[tokio::test]
    async fn runner_timeout_maps_cleanly() {
        let mock = MockTransport::with_error(TitleHttpError::Timeout);
        let runner = DirectCompletionTitleRunner::new(mock);
        let err = runner
            .run(sample_attempt(), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err, AutoTitleRunError::Timeout);
        assert!(!err.to_string().contains("sk-test"));
    }

    #[tokio::test]
    async fn runner_cancel_aborts_in_flight() {
        let mock = MockTransport::hang();
        let runner = DirectCompletionTitleRunner::new(mock);
        let cancel = CancellationToken::new();
        let child = cancel.child_token();
        let handle = tokio::spawn(async move { runner.run(sample_attempt(), child).await });
        // Let the mock enter hang.
        tokio::task::yield_now().await;
        cancel.cancel();
        let err = handle.await.expect("join").unwrap_err();
        assert_eq!(err, AutoTitleRunError::Cancelled);
    }

    #[tokio::test]
    async fn safe_error_strings_contain_no_url_key_prompt() {
        let mock = MockTransport::with_response(TitleHttpResponse {
            status: 500,
            body: "server exploded with sk-test-secret-key and Fix the README".into(),
        });
        let runner = DirectCompletionTitleRunner::new(mock);
        let err = runner
            .run(sample_attempt(), CancellationToken::new())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("http_status=500"), "{msg}");
        assert!(!msg.contains("sk-test"));
        assert!(!msg.contains("api.example"));
        assert!(!msg.contains("Fix the README"));
        assert!(!msg.contains("Updated docs"));
    }

    #[test]
    fn lazy_transport_does_not_construct_client_on_new() {
        let transport = LazyReqwestTitleTransport::new();
        assert!(
            !transport.client_constructed(),
            "factory must not build reqwest until first post"
        );
    }

    #[test]
    fn title_http_error_display_is_safe() {
        for err in [
            TitleHttpError::Cancelled,
            TitleHttpError::Timeout,
            TitleHttpError::Transport,
        ] {
            let s = err.to_string();
            assert!(!s.contains("http://"));
            assert!(!s.contains("Bearer"));
            assert!(!s.contains("sk-"));
        }
    }
}
