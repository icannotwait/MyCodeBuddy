//! Bounded diagnostics. Thinking and secrets never enter the retained excerpt.

use roundtable_protocol::{Hash256, RtResult};

pub struct DiagnosticInput {
    pub assistant: String,
    pub thinking: Option<String>,
    pub secret: Option<String>,
}

pub struct DiagnosticRef {
    pub assistant_excerpt_bytes: usize,
    pub total_bytes: usize,
    pub content_hash: Hash256,
    pub truncated: bool,
    pub text: String,
}

const EXCERPT_LIMIT: usize = 65_536;

pub async fn seal_diagnostic(input: DiagnosticInput) -> RtResult<DiagnosticRef> {
    let _ = input.thinking;
    let mut text = input.assistant;
    if let Some(secret) = input.secret.as_deref() {
        if !secret.is_empty() {
            text = text.replace(secret, "[redacted]");
        }
    }
    let total_bytes = text.len();
    let truncated = total_bytes > EXCERPT_LIMIT;
    if truncated {
        let head = EXCERPT_LIMIT / 2;
        let tail = EXCERPT_LIMIT - head;
        let suffix = text.len().saturating_sub(tail);
        text = format!("{}{}", &text[..head], &text[suffix..]);
    }
    let content_hash = Hash256::sha256(text.as_bytes());
    Ok(DiagnosticRef {
        assistant_excerpt_bytes: text.len(),
        total_bytes,
        content_hash,
        truncated,
        text,
    })
}
