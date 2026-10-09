//! Per-attempt diagnostic trace. Observability only: nothing here changes
//! attempt behaviour, and every write is best effort (a trace I/O failure
//! never fails an attempt).
//!
//! Layout: `<data_dir>/roundtable/diag/<room_id>/<attempt_id>/` (0700) with
//! append-only JSON-lines files (0600), flushed per record so an
//! `attempt_timeout` still leaves everything observed so far:
//! - `gateway.jsonl`  inbound model requests and their upstream relay
//! - `acp.jsonl`      ACP frames (types, sizes, stop reasons; no content)
//! - `tools.jsonl`    roundtable tool calls, validation outcome, seal time
//! - `stderr.log`     agent stderr (redacted, size capped, one rotation)
//! - `snapshot.jsonl` process / in-flight state when the attempt is reaped
//!   without a normal ACP finish (attempt_timeout, stop)
//!
//! Redaction: every record is serialised, then every registered secret
//! (attempt bearer, attempt tool token, host upstream bearer, provider
//! credential, `CODEG_TOKEN`) is replaced, and bearer-looking values are
//! masked. Header values are only kept for a fixed non-sensitive allowlist.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub(crate) const REDACTED: &str = "[REDACTED]";
const STDERR_ROTATE_BYTES: u64 = 4 * 1024 * 1024;
const STDERR_LINE_LIMIT: usize = 8 * 1024;
const STDERR_KEY_EVENTS: u64 = 2000;
const STRING_FIELD_LIMIT: usize = 600;

#[derive(Debug, Clone)]
pub(crate) struct InFlight {
    pub method: String,
    pub path: String,
    pub started: Instant,
    pub stage: &'static str,
    pub body_bytes: u64,
}

pub(crate) struct AttemptTrace {
    dir: PathBuf,
    started: Instant,
    secrets: Mutex<Vec<String>>,
    files: Mutex<BTreeMap<&'static str, std::fs::File>>,
    next_request: AtomicU64,
    inflight: Mutex<BTreeMap<u64, InFlight>>,
    last_activity: Mutex<BTreeMap<&'static str, Instant>>,
    stderr_bytes: AtomicU64,
    stderr_key_events: AtomicU64,
    stderr_partial: Mutex<Vec<u8>>,
    stderr_file_bytes: Mutex<u64>,
}

pub(crate) fn wall_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl AttemptTrace {
    /// Opens (creates) the attempt trace directory. Returns `None` when the
    /// directory cannot be created; callers then simply do not trace.
    pub(crate) fn open(
        data_dir: &Path,
        room: &str,
        attempt: &str,
        secrets: Vec<String>,
    ) -> Option<Self> {
        if !safe_component(room) || !safe_component(attempt) {
            return None;
        }
        let root = data_dir.join("roundtable").join("diag");
        let dir = root.join(room).join(attempt);
        std::fs::create_dir_all(&dir).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [root.clone(), root.join(room), dir.clone()] {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            }
        }
        let mut all = secrets;
        if let Ok(value) = std::env::var("CODEG_TOKEN") {
            all.push(value);
        }
        let trace = Self {
            dir,
            started: Instant::now(),
            secrets: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
            next_request: AtomicU64::new(1),
            inflight: Mutex::new(BTreeMap::new()),
            last_activity: Mutex::new(BTreeMap::new()),
            stderr_bytes: AtomicU64::new(0),
            stderr_key_events: AtomicU64::new(0),
            stderr_partial: Mutex::new(Vec::new()),
            stderr_file_bytes: Mutex::new(0),
        };
        for secret in all {
            trace.add_secret(secret);
        }
        trace.record(
            "acp",
            json!({"event":"trace_open","room_id":room,"attempt_id":attempt,"pid":std::process::id()}),
        );
        Some(trace)
    }

    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Registers another secret value. Short values are ignored because
    /// replacing them would corrupt unrelated text.
    pub(crate) fn add_secret(&self, secret: String) {
        if secret.len() < 8 {
            return;
        }
        let mut secrets = lock(&self.secrets);
        if !secrets.contains(&secret) {
            secrets.push(secret);
            // Longest first so a secret containing another is fully masked.
            secrets.sort_by_key(|item| std::cmp::Reverse(item.len()));
        }
    }

    pub(crate) fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn ms_since(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.started).as_millis()).unwrap_or(u64::MAX)
    }

    /// Masks registered secrets and bearer-looking tokens in `text`.
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in lock(&self.secrets).iter() {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
        mask_bearer_values(&out)
    }

    /// Appends one record to `<stream>.jsonl` with `t_ms` (monotonic since
    /// trace open) and `wall` (UTC). Never panics, never errors.
    pub(crate) fn record(&self, stream: &'static str, value: Value) {
        let mut object = match value {
            Value::Object(map) => map,
            other => {
                let mut map = Map::new();
                map.insert("value".into(), other);
                map
            }
        };
        let mut ordered = Map::new();
        ordered.insert("t_ms".into(), json!(self.elapsed_ms()));
        ordered.insert("wall".into(), json!(wall_now()));
        ordered.append(&mut object);
        let mut value = Value::Object(ordered);
        clip_strings(&mut value);
        let Ok(line) = serde_json::to_string(&value) else {
            return;
        };
        let mut line = self.redact(&line);
        line.push('\n');
        lock(&self.last_activity).insert(stream, Instant::now());
        self.append(stream_file(stream), line.as_bytes());
    }

    fn append(&self, file: &'static str, bytes: &[u8]) {
        let mut files = lock(&self.files);
        if !files.contains_key(file) {
            let Some(handle) = open_private(&self.dir.join(file)) else {
                return;
            };
            files.insert(file, handle);
        }
        if let Some(handle) = files.get_mut(file) {
            let _ = handle.write_all(bytes);
            let _ = handle.flush();
        }
    }

    // ---- gateway -------------------------------------------------------

    pub(crate) fn begin_request(&self, method: &str, path: &str) -> u64 {
        let seq = self.next_request.fetch_add(1, Ordering::Relaxed);
        lock(&self.inflight).insert(
            seq,
            InFlight {
                method: method.to_owned(),
                path: strip_query(path),
                started: Instant::now(),
                stage: "inbound",
                body_bytes: 0,
            },
        );
        seq
    }

    pub(crate) fn request_stage(&self, seq: u64, stage: &'static str, body_bytes: u64) {
        if let Some(entry) = lock(&self.inflight).get_mut(&seq) {
            entry.stage = stage;
            entry.body_bytes = body_bytes;
        }
    }

    pub(crate) fn end_request(&self, seq: u64) {
        lock(&self.inflight).remove(&seq);
    }

    pub(crate) fn inflight_snapshot(&self) -> Vec<Value> {
        lock(&self.inflight)
            .iter()
            .map(|(seq, entry)| {
                json!({
                    "seq": seq,
                    "method": entry.method,
                    "path": entry.path,
                    "stage": entry.stage,
                    "started_t_ms": self.ms_since(entry.started),
                    "elapsed_ms": u64::try_from(entry.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "body_bytes_so_far": entry.body_bytes,
                })
            })
            .collect()
    }

    pub(crate) fn idle_ms(&self) -> Value {
        let map = lock(&self.last_activity);
        let mut out = Map::new();
        for (stream, at) in map.iter() {
            out.insert(
                (*stream).to_owned(),
                json!(u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)),
            );
        }
        Value::Object(out)
    }

    // ---- stderr --------------------------------------------------------

    /// Accepts a raw stderr chunk. Lines are reassembled before redaction so
    /// a secret split across reads is still masked.
    pub(crate) fn stderr_chunk(&self, bytes: &[u8]) {
        let mut lines = Vec::new();
        {
            let mut partial = lock(&self.stderr_partial);
            partial.extend_from_slice(bytes);
            while let Some(pos) = partial.iter().position(|byte| *byte == b'\n') {
                lines.push(partial.drain(..=pos).collect::<Vec<u8>>());
            }
            if partial.len() > STDERR_LINE_LIMIT {
                let mut chunk: Vec<u8> = std::mem::take(&mut *partial);
                chunk.push(b'\n');
                lines.push(chunk);
            }
        }
        for line in lines {
            self.stderr_line(&line);
        }
    }

    /// Writes a pending partial stderr line (no trailing newline yet).
    pub(crate) fn stderr_flush_partial(&self) {
        let rest = std::mem::take(&mut *lock(&self.stderr_partial));
        if !rest.is_empty() {
            let mut rest = rest;
            rest.push(b'\n');
            self.stderr_line(&rest);
        }
    }

    pub(crate) fn stderr_finish(&self) {
        self.stderr_flush_partial();
        self.record(
            "acp",
            json!({"event":"stderr_eof","stderr_bytes":self.stderr_bytes.load(Ordering::Relaxed)}),
        );
    }

    fn stderr_line(&self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        let clean = self.redact(&strip_ansi(&text));
        let stamped = format!("{} +{}ms {}", wall_now(), self.elapsed_ms(), clean);
        let size = stamped.len() as u64;
        self.stderr_bytes.fetch_add(size, Ordering::Relaxed);
        {
            let mut current = lock(&self.stderr_file_bytes);
            if *current + size > STDERR_ROTATE_BYTES {
                self.rotate_stderr();
                *current = 0;
            }
            *current += size;
        }
        self.append("stderr.log", stamped.as_bytes());
        let lower = clean.to_ascii_lowercase();
        if [
            "retry",
            "backoff",
            "timeout",
            "timed out",
            "error",
            "429",
            "503",
            "truncat",
            "incomplete",
            "doom",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
            && self.stderr_key_events.fetch_add(1, Ordering::Relaxed) < STDERR_KEY_EVENTS
        {
            self.record(
                "acp",
                json!({"event":"stderr_key_line","line":clean.trim_end()}),
            );
        }
    }

    /// Keeps one rotation (`stderr.log.1`), so disk use stays under 2x the cap.
    fn rotate_stderr(&self) {
        let mut files = lock(&self.files);
        files.remove("stderr.log");
        let _ = std::fs::rename(self.dir.join("stderr.log"), self.dir.join("stderr.log.1"));
    }

    // ---- process snapshot ---------------------------------------------

    /// Records the agent process tree, its sockets, and in-flight gateway
    /// requests. Used when an attempt is reaped without a normal finish.
    pub(crate) fn snapshot(&self, reason: &str, root_pid: Option<u32>, extra: Value) {
        let processes = root_pid.map(process_tree).unwrap_or_default();
        self.record(
            "snapshot",
            json!({
                "event": "reap_snapshot",
                "reason": reason,
                "root_pid": root_pid,
                "processes": processes,
                "inflight_gateway": self.inflight_snapshot(),
                "idle_ms_by_stream": self.idle_ms(),
                "stderr_bytes": self.stderr_bytes.load(Ordering::Relaxed),
                "extra": extra,
            }),
        );
    }
}

/// One inbound gateway request and its upstream relay. Milestones are
/// written as they happen; the closing `request_end` record is written on
/// `finish` or, if the request future is dropped (attempt timeout, reap),
/// from `Drop`, so a cancelled request still leaves a record.
pub(crate) struct RequestObs {
    trace: std::sync::Arc<AttemptTrace>,
    seq: u64,
    started: Instant,
    data: Map<String, Value>,
    upstream: Vec<Value>,
    current: Option<(Instant, Map<String, Value>)>,
    finished: bool,
}

impl RequestObs {
    pub(crate) fn begin(
        trace: std::sync::Arc<AttemptTrace>,
        method: &str,
        path: &str,
        headers: &axum::http::HeaderMap,
    ) -> Self {
        let seq = trace.begin_request(method, path);
        let mut data = Map::new();
        data.insert("seq".into(), json!(seq));
        data.insert("method".into(), json!(method));
        data.insert("path".into(), json!(strip_query(path)));
        data.insert("start_wall".into(), json!(wall_now()));
        trace.record(
            "gateway",
            json!({"event":"request_start","seq":seq,"method":method,"path":strip_query(path),"inbound_headers":redacted_headers(headers)}),
        );
        Self {
            trace,
            seq,
            started: Instant::now(),
            data,
            upstream: Vec::new(),
            current: None,
            finished: false,
        }
    }

    pub(crate) fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_owned(), value);
    }

    pub(crate) fn upstream_start(&mut self, origin: &str, request_bytes: usize) {
        self.close_current("superseded", None);
        let now = Instant::now();
        let mut map = Map::new();
        map.insert("origin".into(), json!(origin));
        map.insert("request_bytes".into(), json!(request_bytes));
        map.insert("send_t_ms".into(), json!(self.trace.ms_since(now)));
        map.insert("connect_t_ms".into(), json!(null));
        map.insert(
            "connect_note".into(),
            json!("reqwest does not expose connect completion; headers_ms includes connect+TLS+server wait"),
        );
        self.trace
            .request_stage(self.seq, "awaiting_upstream_headers", 0);
        self.trace.record(
            "gateway",
            json!({"event":"upstream_send","seq":self.seq,"origin":origin,"request_bytes":request_bytes}),
        );
        self.current = Some((now, map));
    }

    pub(crate) fn upstream_headers(&mut self, status: u16, headers: &reqwest::header::HeaderMap) {
        let seq = self.seq;
        let Some((sent, map)) = self.current.as_mut() else {
            return;
        };
        let now = Instant::now();
        let ttfb =
            u64::try_from(now.saturating_duration_since(*sent).as_millis()).unwrap_or(u64::MAX);
        let mut shown = Map::new();
        for (name, value) in headers {
            let text = if header_is_sensitive(name.as_str()) {
                REDACTED.to_owned()
            } else {
                value
                    .to_str()
                    .unwrap_or("<non-utf8>")
                    .chars()
                    .take(200)
                    .collect()
            };
            shown.insert(name.as_str().to_owned(), Value::String(text));
        }
        map.insert("status".into(), json!(status));
        map.insert("headers_t_ms".into(), json!(self.trace.ms_since(now)));
        map.insert("ttfb_headers_ms".into(), json!(ttfb));
        map.insert("response_headers".into(), Value::Object(shown.clone()));
        self.trace.request_stage(seq, "streaming_upstream_body", 0);
        self.trace.record(
            "gateway",
            json!({"event":"upstream_headers","seq":seq,"status":status,"ttfb_headers_ms":ttfb,"response_headers":shown}),
        );
    }

    pub(crate) fn upstream_chunk(&mut self, len: usize, total: usize) {
        let seq = self.seq;
        let Some((sent, map)) = self.current.as_mut() else {
            return;
        };
        let chunks = map.get("chunks").and_then(Value::as_u64).unwrap_or(0) + 1;
        map.insert("chunks".into(), json!(chunks));
        if chunks == 1 {
            let now = Instant::now();
            let first =
                u64::try_from(now.saturating_duration_since(*sent).as_millis()).unwrap_or(u64::MAX);
            map.insert("first_byte_t_ms".into(), json!(self.trace.ms_since(now)));
            map.insert("first_byte_ms".into(), json!(first));
            self.trace.record(
                "gateway",
                json!({"event":"upstream_first_byte","seq":seq,"first_byte_ms":first,"bytes":len}),
            );
        }
        self.trace
            .request_stage(seq, "streaming_upstream_body", total as u64);
    }

    pub(crate) fn upstream_end(
        &mut self,
        outcome: &str,
        error: Option<String>,
        body: Option<Value>,
    ) {
        if let Some((_, map)) = self.current.as_mut() {
            if let Some(body) = body {
                map.insert("body".into(), body);
            }
        }
        self.close_current(outcome, error);
    }

    fn close_current(&mut self, outcome: &str, error: Option<String>) {
        if let Some((sent, mut map)) = self.current.take() {
            let now = Instant::now();
            map.insert("outcome".into(), json!(outcome));
            map.insert("end_t_ms".into(), json!(self.trace.ms_since(now)));
            map.insert(
                "upstream_ms".into(),
                json!(
                    u64::try_from(now.saturating_duration_since(sent).as_millis())
                        .unwrap_or(u64::MAX)
                ),
            );
            if let Some(error) = error {
                map.insert("error".into(), json!(error));
            }
            self.upstream.push(Value::Object(map));
        }
    }

    pub(crate) fn finish(mut self, status: u16, response_bytes: usize, outcome: &str) {
        self.write_end(Some(status), response_bytes, outcome);
    }

    fn write_end(&mut self, status: Option<u16>, response_bytes: usize, outcome: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.close_current(
            if outcome == "dropped" {
                "dropped_in_flight"
            } else {
                outcome
            },
            None,
        );
        let mut record = Map::new();
        record.insert("event".into(), json!("request_end"));
        record.append(&mut self.data);
        record.insert("outcome".into(), json!(outcome));
        record.insert("status".into(), json!(status));
        record.insert("response_bytes".into(), json!(response_bytes));
        record.insert(
            "total_ms".into(),
            json!(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        record.insert(
            "upstream".into(),
            Value::Array(std::mem::take(&mut self.upstream)),
        );
        self.trace.end_request(self.seq);
        self.trace.record("gateway", Value::Object(record));
    }
}

impl Drop for RequestObs {
    fn drop(&mut self) {
        if !self.finished {
            self.write_end(None, 0, "dropped");
        }
    }
}

fn stream_file(stream: &'static str) -> &'static str {
    match stream {
        "gateway" => "gateway.jsonl",
        "acp" => "acp.jsonl",
        "tools" => "tools.jsonl",
        "snapshot" => "snapshot.jsonl",
        _ => "other.jsonl",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn open_private(path: &Path) -> Option<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    Some(file)
}

pub(crate) fn strip_query(path: &str) -> String {
    let path = path.split('#').next().unwrap_or(path);
    match path.split_once('?') {
        Some((head, query)) => {
            let keys: Vec<&str> = query
                .split('&')
                .map(|pair| pair.split('=').next().unwrap_or(""))
                .collect();
            format!("{head}?{}", keys.join("&"))
        }
        None => path.to_owned(),
    }
}

/// Header names whose values are never recorded.
pub(crate) fn header_is_sensitive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "key",
        "token",
        "secret",
        "auth",
        "credential",
        "password",
        "signature",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// Header map as `{name: value}` with sensitive values replaced. Values of
/// other headers are clipped; registered secrets are masked later anyway.
pub(crate) fn redacted_headers(headers: &axum::http::HeaderMap) -> Value {
    let mut out = Map::new();
    for (name, value) in headers {
        let shown = if header_is_sensitive(name.as_str()) {
            REDACTED.to_owned()
        } else {
            value
                .to_str()
                .unwrap_or("<non-utf8>")
                .chars()
                .take(200)
                .collect()
        };
        out.insert(name.as_str().to_owned(), Value::String(shown));
    }
    Value::Object(out)
}

fn mask_bearer_values(text: &str) -> String {
    // "Bearer <token>" in any casing; masks up to the next delimiter.
    let lower = text.to_ascii_lowercase();
    if !lower.contains("bearer ") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while let Some(found) = lower[index..].find("bearer ") {
        let start = index + found + "bearer ".len();
        out.push_str(&text[index..start]);
        let rest = &text[start..];
        let end = rest
            .find(|ch: char| {
                ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | '}' | ']' | '\\')
            })
            .unwrap_or(rest.len());
        if end > 0 {
            out.push_str(REDACTED);
        }
        index = start + end;
    }
    out.push_str(&text[index..]);
    out
}

fn clip_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > STRING_FIELD_LIMIT => {
            let mut cut = STRING_FIELD_LIMIT;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let total = text.len();
            text.truncate(cut);
            text.push_str(&format!("…(+{} bytes)", total - cut));
        }
        Value::Array(items) => items.iter_mut().for_each(clip_strings),
        Value::Object(map) => map.values_mut().for_each(clip_strings),
        _ => {}
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Summary of a buffered upstream response body: whether the stream ended
/// cleanly and what the final response object said. Never includes text.
pub(crate) fn classify_upstream_body(
    content_type: &str,
    body: &[u8],
    content_length: Option<u64>,
) -> Value {
    let length_matched = content_length.map(|expected| expected == body.len() as u64);
    if content_type.starts_with("text/event-stream") {
        let text = String::from_utf8_lossy(body);
        let mut events: BTreeMap<String, u64> = BTreeMap::new();
        let mut last_event = None::<String>;
        let mut terminal = None::<Value>;
        let mut done = false;
        let mut current_event = None::<String>;
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("event:") {
                current_event = Some(name.trim().to_owned());
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                done = true;
                continue;
            }
            let parsed: Option<Value> = serde_json::from_str(data).ok();
            let kind = parsed
                .as_ref()
                .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
                .or_else(|| current_event.clone())
                .unwrap_or_else(|| "data".into());
            *events.entry(kind.clone()).or_default() += 1;
            if matches!(
                kind.as_str(),
                "response.completed" | "response.incomplete" | "response.failed" | "error"
            ) {
                terminal = parsed.map(|value| summarize_terminal(&kind, &value));
            }
            last_event = Some(kind);
            current_event = None;
        }
        let terminal_kind = terminal
            .as_ref()
            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned));
        let clean = terminal_kind.as_deref() == Some("response.completed");
        json!({
            "format": "sse",
            "clean_end": clean,
            "end_state": match terminal_kind.as_deref() {
                Some("response.completed") => "completed",
                Some("response.incomplete") => "incomplete",
                Some("response.failed") => "failed",
                Some("error") => "error_event",
                _ if done => "done_without_terminal_event",
                _ => "truncated_no_terminal_event",
            },
            "done_marker": done,
            "event_counts": events,
            "last_event": last_event,
            "terminal": terminal,
            "content_length_matched": length_matched,
        })
    } else {
        let parsed: Option<Value> = serde_json::from_slice(body).ok();
        let status = parsed.as_ref().and_then(|value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        json!({
            "format": "json",
            "parsed": parsed.is_some(),
            "clean_end": parsed.is_some() && length_matched != Some(false),
            "end_state": if parsed.is_none() { "unparseable_or_truncated" } else { "complete_json" },
            "response_status": status,
            "usage": parsed.as_ref().and_then(|value| value.get("usage").cloned()),
            "error": parsed.as_ref().and_then(|value| value.get("error").cloned()),
            "content_length_matched": length_matched,
        })
    }
}

fn summarize_terminal(kind: &str, value: &Value) -> Value {
    let response = value.get("response").unwrap_or(value);
    let mut outputs = Vec::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("?");
        let mut summary = json!({"type": item_type});
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            summary["name"] = json!(name);
        }
        if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
            summary["arguments_bytes"] = json!(arguments.len());
        }
        if item_type == "message" {
            let text_bytes: usize = item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .map(str::len)
                .sum();
            summary["text_bytes"] = json!(text_bytes);
        }
        outputs.push(summary);
    }
    json!({
        "type": kind,
        "response_id": response.get("id"),
        "status": response.get("status"),
        "model": response.get("model"),
        "usage": response.get("usage"),
        "incomplete_details": response.get("incomplete_details"),
        "error": response.get("error").or_else(|| value.get("error")).or_else(|| value.get("message")),
        "output": outputs,
    })
}

/// Shape of a model request body without any content.
pub(crate) fn summarize_request_body(body: &[u8]) -> Value {
    let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
        return json!({"json": false});
    };
    let input = parsed.get("input");
    let mut input_types: BTreeMap<String, u64> = BTreeMap::new();
    if let Some(items) = input.and_then(Value::as_array) {
        for item in items {
            let kind = item
                .get("type")
                .and_then(Value::as_str)
                .or_else(|| item.get("role").and_then(Value::as_str))
                .unwrap_or("?");
            *input_types.entry(kind.to_owned()).or_default() += 1;
        }
    }
    json!({
        "json": true,
        "model": parsed.get("model"),
        "stream": parsed.get("stream"),
        "max_output_tokens": parsed.get("max_output_tokens"),
        "reasoning": parsed.get("reasoning"),
        "store": parsed.get("store"),
        "has_previous_response_id": parsed.get("previous_response_id").is_some(),
        "tools": parsed.get("tools").and_then(Value::as_array).map(Vec::len),
        "input_items": input.and_then(Value::as_array).map(Vec::len),
        "input_types": input_types,
        "instructions_bytes": parsed.get("instructions").and_then(Value::as_str).map(str::len),
    })
}

/// Agent process tree under `root` from /proc: state, wchan, threads and
/// socket fds per process.
pub(crate) fn process_tree(root: u32) -> Vec<Value> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(ppid) = proc_status_field(pid, "PPid").and_then(|value| value.parse().ok())
            {
                children.entry(ppid).or_default().push(pid);
            }
        }
    }
    let mut out = Vec::new();
    let mut queue = vec![(root, 0u32)];
    while let Some((pid, depth)) = queue.pop() {
        if out.len() >= 64 {
            break;
        }
        out.push(process_entry(pid, depth));
        for child in children.get(&pid).into_iter().flatten() {
            queue.push((*child, depth + 1));
        }
    }
    out
}

fn proc_status_field(pid: u32, field: &str) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix(field)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(|value| value.trim().to_owned())
    })
}

fn process_entry(pid: u32, depth: u32) -> Value {
    let alive = Path::new(&format!("/proc/{pid}")).exists();
    if !alive {
        return json!({"pid": pid, "depth": depth, "alive": false});
    }
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|text| text.trim().to_owned())
        .ok();
    let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).ok();
    let mut sockets = 0u64;
    let mut fds = 0u64;
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
        for entry in entries.flatten() {
            fds += 1;
            if std::fs::read_link(entry.path())
                .map(|target| target.to_string_lossy().starts_with("socket:"))
                .unwrap_or(false)
            {
                sockets += 1;
            }
        }
    }
    let tcp = std::fs::read_to_string(format!("/proc/{pid}/net/tcp"))
        .ok()
        .map(|text| tcp_state_counts(&text));
    let tcp6 = std::fs::read_to_string(format!("/proc/{pid}/net/tcp6"))
        .ok()
        .map(|text| tcp_state_counts(&text));
    json!({
        "pid": pid,
        "depth": depth,
        "alive": true,
        "comm": comm,
        "state": proc_status_field(pid, "State"),
        "threads": proc_status_field(pid, "Threads"),
        "vm_rss": proc_status_field(pid, "VmRSS"),
        "wchan": wchan,
        "fds": fds,
        "socket_fds": sockets,
        "netns_tcp_states": tcp,
        "netns_tcp6_states": tcp6,
    })
}

fn tcp_state_counts(text: &str) -> Value {
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    for line in text.lines().skip(1) {
        let state = line.split_whitespace().nth(3).unwrap_or("");
        let name = match state {
            "01" => "ESTABLISHED",
            "02" => "SYN_SENT",
            "06" => "TIME_WAIT",
            "08" => "CLOSE_WAIT",
            "0A" => "LISTEN",
            _ => "OTHER",
        };
        *counts.entry(name).or_default() += 1;
    }
    json!(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(dir: &Path) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
        text
    }

    #[test]
    fn secrets_and_bearers_never_reach_disk_and_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("CODEG_TOKEN", "codeg-token-value-1234567890");
        let trace = AttemptTrace::open(
            dir.path(),
            "room-1",
            "attempt-1",
            vec!["attempt-secret-abcdef".into(), "short".into()],
        )
        .unwrap();
        trace.add_secret("host-upstream-bearer-xyz".into());
        trace.record(
            "gateway",
            json!({
                "a": "x attempt-secret-abcdef y",
                "b": "Authorization: Bearer some-unregistered-token",
                "c": "codeg-token-value-1234567890",
                "d": "host-upstream-bearer-xyz",
            }),
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("authorization", "Bearer hdr-secret-1".parse().unwrap());
        headers.insert("x-api-key", "hdr-secret-2".parse().unwrap());
        headers.insert("x-xai-token-auth", "hdr-secret-3".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        trace.record("gateway", json!({"headers": redacted_headers(&headers)}));
        // A secret split across two stderr reads is still masked.
        trace.stderr_chunk(b"retrying with attempt-sec");
        trace.stderr_chunk(b"ret-abcdef now\nnext line Bearer zzz-token\n");
        trace.stderr_finish();
        let text = read_all(trace.dir());
        for leaked in [
            "attempt-secret-abcdef",
            "some-unregistered-token",
            "codeg-token-value-1234567890",
            "host-upstream-bearer-xyz",
            "hdr-secret-1",
            "hdr-secret-2",
            "hdr-secret-3",
            "zzz-token",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        assert!(text.contains("application/json"));
        assert!(text.contains("stderr_key_line"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for entry in std::fs::read_dir(trace.dir()).unwrap().flatten() {
                let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{:?}", entry.path());
            }
            let mode = std::fs::metadata(trace.dir()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        std::env::remove_var("CODEG_TOKEN");
    }

    #[test]
    fn unsafe_ids_do_not_open_a_trace() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AttemptTrace::open(dir.path(), "../x", "a", vec![]).is_none());
        assert!(AttemptTrace::open(dir.path(), "r", "a/b", vec![]).is_none());
    }

    #[test]
    fn sse_classification_distinguishes_completed_truncated_and_incomplete() {
        let completed = b"event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"usage\":{\"output_tokens\":5},\"output\":[{\"type\":\"function_call\",\"name\":\"use_tool\",\"arguments\":\"{}\"}]}}\n\n";
        let value = classify_upstream_body("text/event-stream", completed, None);
        assert_eq!(value["clean_end"], true);
        assert_eq!(value["end_state"], "completed");
        assert_eq!(value["terminal"]["output"][0]["name"], "use_tool");
        let truncated = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        let value = classify_upstream_body("text/event-stream", truncated, Some(10));
        assert_eq!(value["clean_end"], false);
        assert_eq!(value["end_state"], "truncated_no_terminal_event");
        assert_eq!(value["content_length_matched"], false);
        let incomplete = b"data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n";
        let value = classify_upstream_body("text/event-stream", incomplete, None);
        assert_eq!(value["end_state"], "incomplete");
        let json_body = br#"{"status":"completed","usage":{"input_tokens":3}}"#;
        let value =
            classify_upstream_body("application/json", json_body, Some(json_body.len() as u64));
        assert_eq!(value["clean_end"], true);
    }

    #[test]
    fn inflight_requests_appear_in_snapshot_until_ended() {
        let dir = tempfile::tempdir().unwrap();
        let trace = AttemptTrace::open(dir.path(), "r", "a", vec![]).unwrap();
        let seq = trace.begin_request("POST", "/v1/responses?api_key=zzz");
        trace.request_stage(seq, "awaiting_upstream_headers", 0);
        let inflight = trace.inflight_snapshot();
        assert_eq!(inflight.len(), 1);
        assert_eq!(inflight[0]["stage"], "awaiting_upstream_headers");
        assert_eq!(inflight[0]["path"], "/v1/responses?api_key");
        trace.snapshot("attempt_timeout", Some(std::process::id()), json!({}));
        let text = std::fs::read_to_string(trace.dir().join("snapshot.jsonl")).unwrap();
        assert!(text.contains("awaiting_upstream_headers"));
        assert!(text.contains("\"alive\":true"));
        trace.end_request(seq);
        assert!(trace.inflight_snapshot().is_empty());
    }

    #[test]
    fn request_summary_has_shape_but_no_content() {
        let body = br#"{"model":"grok-x","stream":true,"input":[{"role":"user","content":"SECRET PROMPT"},{"type":"function_call_output","output":"x"}],"tools":[{}],"instructions":"abc"}"#;
        let summary = summarize_request_body(body);
        assert_eq!(summary["model"], "grok-x");
        assert_eq!(summary["stream"], true);
        assert_eq!(summary["input_items"], 2);
        assert!(!summary.to_string().contains("SECRET PROMPT"));
    }
}
