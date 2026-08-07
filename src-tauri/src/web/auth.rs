use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::acp::delegation::types::CompletionMutationContext;

pub const WS_EVENT_PROTOCOL: &str = "codeg-events";
pub const COMPLETION_CONTEXT_HEADER: &str = "x-codeg-completion-context";
const WS_TOKEN_PROTOCOL_PREFIX: &str = "codeg-token.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionRootScope {
    GlobalOperator,
    Conversation(i32),
}

#[derive(Debug, Clone)]
struct CompletionAuthorizationEntry {
    parent_conversation_id: i32,
    actor_identity: String,
}

#[derive(Default)]
struct CompletionAuthorizationState {
    by_token: HashMap<String, CompletionAuthorizationEntry>,
    by_root: HashMap<i32, String>,
}

/// Process-local opaque capabilities issued by authenticated workflow
/// snapshots. Tokens are root-scoped, bounded to one per durable root, and
/// intentionally invalidated by process restart.
#[derive(Default)]
pub struct CompletionAuthorizationRegistry {
    state: Mutex<CompletionAuthorizationState>,
}

impl CompletionAuthorizationRegistry {
    pub fn issue(&self, parent_conversation_id: i32) -> String {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(token) = state.by_root.get(&parent_conversation_id) {
            return token.clone();
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        state.by_token.insert(
            token.clone(),
            CompletionAuthorizationEntry {
                parent_conversation_id,
                actor_identity: format!("web_completion_root:{parent_conversation_id}"),
            },
        );
        state.by_root.insert(parent_conversation_id, token.clone());
        token
    }

    pub fn authenticate(&self, token: &str) -> Option<AuthenticatedApplication> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entry = state.by_token.get(token)?;
        Some(AuthenticatedApplication {
            actor_identity: entry.actor_identity.clone(),
            completion_root_scope: CompletionRootScope::Conversation(entry.parent_conversation_id),
        })
    }
}

/// Identity and root scope established by the transport authentication layer.
/// The raw bearer credential is never retained or written to completion audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedApplication {
    actor_identity: String,
    completion_root_scope: CompletionRootScope,
}

impl AuthenticatedApplication {
    fn server_operator() -> Self {
        Self {
            actor_identity: "web_server_operator".into(),
            completion_root_scope: CompletionRootScope::GlobalOperator,
        }
    }

    pub fn authorize_completion_root(
        &self,
        parent_conversation_id: i32,
    ) -> Option<CompletionMutationContext> {
        match self.completion_root_scope {
            CompletionRootScope::Conversation(owned) if owned == parent_conversation_id => {
                Some(CompletionMutationContext::authenticated(
                    parent_conversation_id,
                    self.actor_identity.clone(),
                ))
            }
            CompletionRootScope::GlobalOperator | CompletionRootScope::Conversation(_) => None,
        }
    }
}

fn token_from_ws_protocols(value: &str) -> Option<String> {
    value
        .split(',')
        .map(str::trim)
        .find_map(|protocol| protocol.strip_prefix(WS_TOKEN_PROTOCOL_PREFIX))
        .and_then(|encoded| URL_SAFE_NO_PAD.decode(encoded).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

pub async fn require_token(request: Request, next: Next, token: String) -> Response {
    require_token_with_completion_authorizations(
        request,
        next,
        token,
        Arc::new(CompletionAuthorizationRegistry::default()),
    )
    .await
}

pub async fn require_token_with_completion_authorizations(
    mut request: Request,
    next: Next,
    token: String,
    completion_authorizations: Arc<CompletionAuthorizationRegistry>,
) -> Response {
    // Fail closed on a misconfigured empty token: otherwise `Bearer ` (an empty
    // bearer value) would match it and silently disable authentication.
    if token.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Server token is not configured").into_response();
    }

    if let Some(auth_header) = request.headers().get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.strip_prefix("Bearer ").is_some_and(|t| t == token) {
                let authenticated = match request.headers().get(COMPLETION_CONTEXT_HEADER) {
                    Some(value) => value
                        .to_str()
                        .ok()
                        .and_then(|value| completion_authorizations.authenticate(value)),
                    None => Some(AuthenticatedApplication::server_operator()),
                };
                let Some(authenticated) = authenticated else {
                    return (StatusCode::UNAUTHORIZED, "Invalid completion context")
                        .into_response();
                };
                request.extensions_mut().insert(authenticated);
                return next.run(request).await;
            }
        }
    }

    if let Some(protocol_header) = request.headers().get("sec-websocket-protocol") {
        if let Ok(protocols) = protocol_header.to_str() {
            if token_from_ws_protocols(protocols).is_some_and(|t| t == token) {
                request
                    .extensions_mut()
                    .insert(AuthenticatedApplication::server_operator());
                return next.run(request).await;
            }
        }
    }

    (StatusCode::UNAUTHORIZED, "Invalid or missing token").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn parses_token_from_ws_protocols() {
        let token = "secret/token+value";
        let encoded = URL_SAFE_NO_PAD.encode(token);
        assert_eq!(
            token_from_ws_protocols(&format!("codeg-events, codeg-token.{encoded}")).as_deref(),
            Some(token)
        );
    }

    #[test]
    fn ignores_invalid_ws_protocol_token() {
        assert!(token_from_ws_protocols("codeg-events, codeg-token.not-valid-@@@@").is_none());
    }
}
