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
        let mut head = EXCERPT_LIMIT / 2;
        while !text.is_char_boundary(head) {
            head -= 1;
        }
        let tail = EXCERPT_LIMIT - head;
        let mut suffix = text.len().saturating_sub(tail);
        while !text.is_char_boundary(suffix) {
            suffix += 1;
        }
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

/// Streaming capture keeps possible secret prefixes across chunk boundaries.
/// Redaction happens before the bounded UTF-8 excerpt is clipped.
#[derive(Clone)]
pub struct DiagnosticCapture {
    secrets: Vec<String>,
    pending: String,
    retained: String,
    total: usize,
    redacted_bytes: usize,
    hash: sha2::Sha256,
}
impl DiagnosticCapture {
    pub fn new(secrets: Vec<String>) -> Self {
        use sha2::Digest;
        Self {
            secrets: secrets
                .into_iter()
                .filter(|secret| !secret.is_empty())
                .collect(),
            pending: String::new(),
            retained: String::new(),
            total: 0,
            redacted_bytes: 0,
            hash: sha2::Sha256::new(),
        }
    }
    pub fn push(&mut self, chunk: &str) {
        use sha2::Digest;
        self.total = self.total.saturating_add(chunk.len());
        self.hash.update(chunk.as_bytes());
        self.pending.push_str(chunk);
        self.consume(false);
    }
    fn consume(&mut self, finished: bool) {
        let mut offset = 0;
        while offset < self.pending.len() {
            let rest = &self.pending[offset..];
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| rest.starts_with(secret.as_str()))
            {
                offset += secret.len();
                self.retained.push_str("[redacted]");
                self.redacted_bytes = self.redacted_bytes.saturating_add(10);
            } else if self.secrets.iter().any(|secret| secret.starts_with(rest)) {
                if !finished {
                    break;
                }
                // An interrupted stream may end inside a secret. Preserve the
                // conservative redaction even when it overlaps ordinary text.
                offset = self.pending.len();
                self.retained.push_str("[redacted]");
                self.redacted_bytes = self.redacted_bytes.saturating_add(10);
            } else {
                let ch = rest.chars().next().expect("nonempty pending");
                offset += ch.len_utf8();
                self.retained.push(ch);
                self.redacted_bytes = self.redacted_bytes.saturating_add(ch.len_utf8());
            }
        }
        self.pending.drain(..offset);
        if self.retained.len() > EXCERPT_LIMIT {
            let mut head = EXCERPT_LIMIT / 2;
            while !self.retained.is_char_boundary(head) {
                head -= 1;
            }
            let mut tail = self.retained.len() - (EXCERPT_LIMIT - head);
            while !self.retained.is_char_boundary(tail) {
                tail += 1;
            }
            self.retained.replace_range(head..tail, "");
        }
    }
    pub async fn finish(mut self) -> RtResult<(DiagnosticRef, Hash256)> {
        use sha2::Digest;
        self.consume(true);
        let mut excerpt = seal_diagnostic(DiagnosticInput {
            assistant: self.retained,
            thinking: None,
            secret: None,
        })
        .await?;
        excerpt.total_bytes = self.total;
        excerpt.truncated = self.redacted_bytes > EXCERPT_LIMIT;
        let digest: [u8; 32] = self.hash.finalize().into();
        Ok((excerpt, Hash256::from_bytes(digest)))
    }
}
