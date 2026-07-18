use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

pub const WS_EVENT_PROTOCOL: &str = "codeg-events";
const WS_TOKEN_PROTOCOL_PREFIX: &str = "codeg-token.";

/// Compare two tokens without leaking their contents through timing.
///
/// A naive `a == b` short-circuits on the first differing byte, so the
/// response latency reveals how many leading bytes matched — enough for a
/// patient remote attacker to reconstruct the access token one byte at a
/// time. Hashing both sides to a fixed-width SHA-256 digest first removes
/// the length side-channel, and the XOR-accumulate loop below always
/// inspects every byte so the running time is independent of where (or
/// whether) the inputs diverge.
fn tokens_match(candidate: &str, expected: &str) -> bool {
    let candidate_digest = Sha256::digest(candidate.as_bytes());
    let expected_digest = Sha256::digest(expected.as_bytes());
    let mut diff = 0u8;
    for (a, b) in candidate_digest.iter().zip(expected_digest.iter()) {
        diff |= a ^ b;
    }
    diff == 0
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
    // Fail closed on a misconfigured empty token: otherwise `Bearer ` (an empty
    // bearer value) would match it and silently disable authentication.
    if token.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Server token is not configured").into_response();
    }

    if let Some(auth_header) = request.headers().get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str
                .strip_prefix("Bearer ")
                .is_some_and(|t| tokens_match(t, &token))
            {
                return next.run(request).await;
            }
        }
    }

    if let Some(protocol_header) = request.headers().get("sec-websocket-protocol") {
        if let Ok(protocols) = protocol_header.to_str() {
            if token_from_ws_protocols(protocols).is_some_and(|t| tokens_match(&t, &token)) {
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

    #[test]
    fn tokens_match_only_on_exact_equality() {
        assert!(tokens_match("s3cr3t-token", "s3cr3t-token"));
        assert!(!tokens_match("s3cr3t-token", "s3cr3t-toke"));
        assert!(!tokens_match("s3cr3t-token", "s3cr3t-tokeX"));
        assert!(!tokens_match("", "s3cr3t-token"));
        assert!(!tokens_match("s3cr3t-token", ""));
        // Two empty tokens hash equal, but `require_token` rejects an empty
        // configured token before this is ever reached (see the fail-closed
        // guard), so this only documents the primitive's behavior.
        assert!(tokens_match("", ""));
    }
}
