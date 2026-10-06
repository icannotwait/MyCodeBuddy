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

#[cfg(unix)]
fn open_auth_file(path: &std::path::Path) -> RtResult<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| super::rt_error(roundtable_protocol::ErrorCode::CapabilityUnqualified,"auth_redaction_unavailable"))
}
#[cfg(not(unix))]
fn open_auth_file(_path: &std::path::Path) -> RtResult<std::fs::File> {
    // File-auth execution has no qualified no-follow/nonblocking boundary here.
    Err(super::rt_error(roundtable_protocol::ErrorCode::CapabilityUnqualified,"auth_redaction_unavailable"))
}

/// Authentication files are untrusted in size and format. Keep their raw text
/// and every JSON string value so value-only or reordered output is covered.
/// Collecting all string leaves is deliberately conservative across supported
/// adapter formats (Codex, Cursor, Grok and OAuth variants).
pub(crate) fn auth_file_secrets(path: &std::path::Path) -> RtResult<Vec<String>> {
    use std::io::Read;
    let invalid = || super::rt_error(roundtable_protocol::ErrorCode::CapabilityUnqualified, "auth_redaction_unavailable");
    let file = open_auth_file(path)?;
    if !file.metadata().map_err(|_| invalid())?.is_file() { return Err(invalid()); }
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes).map_err(|_| invalid())?;
    if bytes.len() > 65_536 { return Err(invalid()); }
    let raw = String::from_utf8(bytes).map_err(|_| invalid())?;
    if raw.trim().is_empty() { return Err(invalid()); }
    let mut secrets = vec![raw.clone()];
    let trimmed = raw.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let json: serde_json::Value = serde_json::from_str(trimmed).map_err(|_| invalid())?;
        let mut pending = vec![&json];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::String(value) if !value.is_empty() => {
                    secrets.push(value.clone());
                    // JSON-escaped output must not expose a transformed credential.
                    let quoted = serde_json::to_string(value).map_err(|_| invalid())?;
                    secrets.push(quoted[1..quoted.len() - 1].to_owned());
                }
                serde_json::Value::Object(values) => pending.extend(values.values()),
                serde_json::Value::Array(values) => pending.extend(values),
                _ => {}
            }
            if secrets.len() > 2_048 { return Err(invalid()); }
        }
    } else { secrets.push(trimmed.to_owned()); }
    secrets.sort();
    secrets.dedup();
    Ok(secrets)
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
    pub fn new(mut secrets: Vec<String>) -> Self {
        use sha2::Digest;
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
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
            if self.secrets.iter().any(|secret| secret.len() > rest.len() && secret.starts_with(rest)) {
                if !finished { break; }
                // EOF can interrupt the longer of two overlapping secrets.
                offset = self.pending.len();
                self.retained.push_str("[redacted]");
                self.redacted_bytes = self.redacted_bytes.saturating_add(10);
                continue;
            }
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| rest.starts_with(secret.as_str()))
            {
                offset += secret.len();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_json_values_redact_reordered_split_and_interrupted_streams() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(&path, r#"{"tokens":{"access_token":"secret-access-123","refresh_token":"secret-refresh-456"},"OPENAI_API_KEY":"secret-api-789"}"#).unwrap();
        let secrets = auth_file_secrets(&path).unwrap();
        for chunks in [
            vec!["secret-access-123"],
            vec!["{\"refresh_token\":\"secret-refresh-456\",\"access_token\":\"secret-access-123\"}"],
            vec!["value: secret-api-", "789 end"],
            vec!["value: secret-ref"],
        ] {
            let mut capture = DiagnosticCapture::new(secrets.clone());
            for chunk in chunks { capture.push(chunk); }
            let (result, _) = capture.finish().await.unwrap();
            assert!(!result.text.contains("secret-"), "{}", result.text);
            assert!(result.text.contains("[redacted]"));
        }
    }

    #[tokio::test]
    async fn overlapping_secret_prefix_is_not_released_early() {
        let mut capture = DiagnosticCapture::new(vec!["short".into(), "short-long-token".into()]);
        capture.push("short");
        capture.push("-long-token");
        let (result, _) = capture.finish().await.unwrap();
        assert_eq!(result.text, "[redacted]");
        let mut partial = DiagnosticCapture::new(vec!["short".into(), "short-long-token".into()]);
        partial.push("short-long");
        assert_eq!(partial.finish().await.unwrap().0.text, "[redacted]");
    }

    #[test]
    fn rejected_payload_logging_never_retains_short_or_interrupted_values() {
        for payload in [br#"{"payload":"short-secret","short-key-secret":"x"}"#.as_slice(), b"short-secret", br#"{"token":"short-se"#] {
            let excerpt=redact_untrusted_excerpt(payload);
            assert!(!excerpt.contains("short-"),"{excerpt}");
            assert!(excerpt.contains("[redacted]"));
            assert!(excerpt.chars().count()<=200);
        }
    }

    #[cfg(unix)]
    #[test]
    fn auth_fifo_and_symlink_are_rejected_without_waiting_for_a_writer() {
        use std::os::unix::{ffi::OsStrExt,fs::{OpenOptionsExt,symlink}};
        let dir=tempfile::tempdir().unwrap();
        let fifo=dir.path().join("auth.fifo");
        let name=std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // This creates only the fixture FIFO, never opens a host credential.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(),0o600) },0);
        let path=fifo.clone();
        let (send,receive)=std::sync::mpsc::channel();
        let reader=std::thread::spawn(move || { let _=send.send(auth_file_secrets(&path)); });
        let result=receive.recv_timeout(std::time::Duration::from_secs(2));
        // Unblock and join even the unfixed implementation before asserting.
        let _rescue=if result.is_err() {
            Some(std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(&fifo).unwrap())
        } else { None };
        reader.join().unwrap();
        assert!(result.expect("auth open waited for a FIFO writer").is_err());
        let real=dir.path().join("real.json"); std::fs::write(&real,b"{\"token\":\"fixture-secret\"}").unwrap();
        let link=dir.path().join("linked.json"); symlink(&real,&link).unwrap();
        assert!(auth_file_secrets(&link).is_err());
    }

    #[cfg(not(unix))]
    #[test]
    fn file_auth_read_requires_a_qualified_open_boundary() {
        assert!(auth_file_secrets(std::path::Path::new("unused-auth.json")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn auth_file_read_is_bounded_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(&path, vec![b'a'; 65_537]).unwrap();
        assert!(auth_file_secrets(&path).is_err());
        std::fs::write(&path, b"{malformed json").unwrap();
        assert!(auth_file_secrets(&path).is_err());
    }
}

/// One log line for an untrusted ACP frame or submit payload.
/// Payload values are omitted; only bounded safe field/shape metadata remains.
pub(crate) fn redact_untrusted_excerpt(bytes: &[u8]) -> String {
    const LIMIT: usize = 200;
    let text = String::from_utf8_lossy(bytes);
    // These callers do not have an attempt credential denylist. Retain only
    // bounded protocol shape, never arbitrary short values or malformed tails.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return format!("[redacted] malformed JSON ({} bytes)",bytes.len());
    };
    fn shape(value: &serde_json::Value, depth: usize) -> serde_json::Value {
        use serde_json::{json, Value};
        if depth == 8 { return json!("[redacted]"); }
        match value {
            Value::Object(values) => Value::Object(values.iter().take(32).map(|(key,value)| {
                let safe = match key.as_str() {
                    "jsonrpc"|"id"|"method"|"params"|"result"|"error"|"code"|"message"|"data"|"update"|"sessionUpdate"|"content"|"type"|"text"|"score"|"toolCall"|"rawInput"|"_meta"|"refresh_token"|"access_token"|"api_key"|"authorization"|"token"|"password" => key.as_str(),
                    _ => "[redacted-field]",
                };
                (safe.to_owned(),shape(value,depth+1))
            }).collect()),
            Value::Array(values) => Value::Array(values.iter().take(32).map(|value|shape(value,depth+1)).collect()),
            Value::Null => Value::Null,
            _ => json!("[redacted]"),
        }
    }
    shape(&value,0).to_string().chars().take(LIMIT).collect()
}
