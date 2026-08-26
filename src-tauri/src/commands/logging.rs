//! Commands backing the Settings → Logs viewer.
//!
//! `*_core` functions hold the logic and are shared by the Tauri command
//! wrappers (desktop) and the Axum handlers (`web/handlers/logging.rs`). The
//! live viewer reads the in-memory ring buffer ([`get_recent_logs_core`]) and
//! live-tails `logs://appended`; on-disk files are listed
//! ([`list_log_files_core`]) and read for forensics/download
//! ([`read_log_file_core`]) separately.

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
#[cfg(feature = "tauri-runtime")]
use tauri::State;

use crate::app_error::AppCommandError;
use crate::db::service::app_metadata_service;
#[cfg(feature = "tauri-runtime")]
use crate::db::AppDatabase;
use crate::logging::hub::{level_rank, log_hub, LogRecord};
use crate::logging::{
    LogLevel, LogSettings, TargetDirective, LOGGING_LEVEL_KEY, LOG_SETTINGS_CHANGED_EVENT,
};
use crate::web::event_bridge::{emit_event, EventEmitter};

/// Metadata for one on-disk rotated log file.
#[derive(Debug, Clone, Serialize)]
pub struct LogFileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub modified_ms: u64,
}

/// Hard ceiling on a single `read_log_file` response. Daily files are uncapped
/// on disk and a chatty debug day can reach hundreds of MB; reading one whole
/// into a `String` over HTTP would spike server/browser memory. This bounds the
/// read to the newest slice (logs are append-only) — a memory-safety limit, not
/// a feature cap. Older history lives in the separate rotated daily files (and,
/// on desktop, the full file is reachable via "open folder").
const READ_LOG_MAX_BYTES: usize = 16 * 1024 * 1024;

/// What the Settings UI needs to render the capture controls: the persisted
/// global level and per-target overrides, plus whether `CODEG_LOG`/`RUST_LOG`
/// currently locks them (env owns the live level, so the UI shows them
/// read-only to avoid silent divergence).
#[derive(Debug, Clone, Serialize)]
pub struct LogSettingsView {
    pub level: LogLevel,
    pub targets: Vec<TargetDirective>,
    pub env_locked: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendTurnTracePhase {
    SendStarted,
    SendCompleted,
    PromptingFrame,
    LivePublished,
    BannerCommit,
    BannerPaint,
    FirstContent,
}

impl FrontendTurnTracePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::SendStarted => "send_started",
            Self::SendCompleted => "send_completed",
            Self::PromptingFrame => "prompting_frame",
            Self::LivePublished => "live_published",
            Self::BannerCommit => "banner_commit",
            Self::BannerPaint => "banner_paint",
            Self::FirstContent => "first_content",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendTurnTraceOutcome {
    Success,
    TurnBusy,
    ContinuationWaiting,
    ViewerOnly,
    Failed,
}

impl FrontendTurnTraceOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::TurnBusy => "turn_busy",
            Self::ContinuationWaiting => "continuation_waiting",
            Self::ViewerOnly => "viewer_only",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendTurnTrace {
    pub phase: FrontendTurnTracePhase,
    pub client_timestamp_ms: f64,
    #[serde(default)]
    pub context_key: Option<String>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<i64>,
    #[serde(default)]
    pub client_message_id: Option<String>,
    #[serde(default)]
    pub live_message_id: Option<String>,
    #[serde(default)]
    pub event_seq: Option<u64>,
    #[serde(default)]
    pub received_at_ms: Option<f64>,
    #[serde(default)]
    pub elapsed_ms: Option<f64>,
    #[serde(default)]
    pub outcome: Option<FrontendTurnTraceOutcome>,
    #[serde(default)]
    pub sink_registered: Option<bool>,
    #[serde(default)]
    pub canonical_accepted: Option<bool>,
    #[serde(default)]
    pub transcript_published: Option<bool>,
    #[serde(default)]
    pub has_live_transcript: Option<bool>,
}

pub fn record_frontend_turn_trace_core(trace: FrontendTurnTrace) -> Result<(), AppCommandError> {
    const MAX_CORRELATOR_BYTES: usize = 256;
    let invalid_time = |value: f64| !value.is_finite() || value < 0.0;
    if invalid_time(trace.client_timestamp_ms)
        || trace.received_at_ms.is_some_and(invalid_time)
        || trace.elapsed_ms.is_some_and(invalid_time)
    {
        return Err(AppCommandError::invalid_input(
            "Invalid frontend turn trace timing",
        ));
    }
    for value in [
        trace.context_key.as_deref(),
        trace.connection_id.as_deref(),
        trace.client_message_id.as_deref(),
        trace.live_message_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.len() > MAX_CORRELATOR_BYTES || value.chars().any(char::is_control) {
            return Err(AppCommandError::invalid_input(
                "Invalid frontend turn trace correlator",
            ));
        }
    }
    let optional_bool = |value: Option<bool>| match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "",
    };
    tracing::info!(
        target: "codeg_frontend_turn",
        phase = trace.phase.as_str(),
        client_timestamp_ms = trace.client_timestamp_ms,
        context_key = trace.context_key.as_deref().unwrap_or(""),
        connection_id = trace.connection_id.as_deref().unwrap_or(""),
        conversation_id = trace.conversation_id.unwrap_or_default(),
        client_message_id = trace.client_message_id.as_deref().unwrap_or(""),
        live_message_id = trace.live_message_id.as_deref().unwrap_or(""),
        event_seq = trace.event_seq.unwrap_or_default(),
        received_at_ms = trace.received_at_ms.unwrap_or_default(),
        elapsed_ms = trace.elapsed_ms.unwrap_or_default(),
        outcome = trace.outcome.map(FrontendTurnTraceOutcome::as_str).unwrap_or(""),
        sink_registered = optional_bool(trace.sink_registered),
        canonical_accepted = optional_bool(trace.canonical_accepted),
        transcript_published = optional_bool(trace.transcript_published),
        has_live_transcript = optional_bool(trace.has_live_transcript),
        "frontend turn milestone"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Core logic (shared by Tauri commands and web handlers)
// ---------------------------------------------------------------------------

/// Load the persisted level (defaulting to [`LogLevel::Info`]) plus whether an
/// env var currently locks it, for the Settings UI.
pub async fn get_log_settings_core(
    conn: &DatabaseConnection,
) -> Result<LogSettingsView, AppCommandError> {
    let settings = match app_metadata_service::get_value(conn, LOGGING_LEVEL_KEY)
        .await
        .map_err(AppCommandError::from)?
    {
        Some(raw) => serde_json::from_str::<LogSettings>(&raw).map_err(|e| {
            AppCommandError::configuration_invalid("Failed to parse stored log settings")
                .with_detail(e.to_string())
        })?,
        None => LogSettings::default(),
    };
    Ok(LogSettingsView {
        level: settings.level,
        targets: settings.targets,
        env_locked: crate::logging::init::env_level_is_set(),
    })
}

/// Persist [`LogSettings`], apply the new level live (reload handle), and
/// broadcast [`LOG_SETTINGS_CHANGED_EVENT`] so a Logs viewer in another window
/// / WS client converges.
pub async fn set_log_settings_core(
    conn: &DatabaseConnection,
    settings: LogSettings,
    emitter: &EventEmitter,
) -> Result<LogSettings, AppCommandError> {
    let serialized = serde_json::to_string(&settings).map_err(|e| {
        AppCommandError::invalid_input("Failed to serialize log settings")
            .with_detail(e.to_string())
    })?;
    app_metadata_service::upsert_value(conn, LOGGING_LEVEL_KEY, &serialized)
        .await
        .map_err(AppCommandError::from)?;
    // The env var (CODEG_LOG/RUST_LOG) owns the live level when set; persist the
    // choice for when it's later removed, but don't override it at runtime — the
    // UI locks the control in that case, so this also guards a stale client.
    if !crate::logging::init::env_level_is_set() {
        if let Some(hub) = log_hub() {
            hub.apply_settings(&settings);
        }
    }
    // Emit by reference (LogSettings is no longer Copy now that it carries a Vec).
    emit_event(emitter, LOG_SETTINGS_CHANGED_EVENT, &settings);
    Ok(settings)
}

/// Filtered, newest-`limit` slice of the in-memory ring buffer. Pure: no DB, no
/// emitter, no file I/O — returns empty when no hub is installed.
pub fn get_recent_logs_core(
    limit: usize,
    min_level: Option<LogLevel>,
    search: Option<&str>,
) -> Vec<LogRecord> {
    match log_hub() {
        Some(hub) => filter_recent(hub.snapshot(), limit, min_level, search),
        None => Vec::new(),
    }
}

/// Apply the viewer filters to an oldest-first record list: drop anything below
/// `min_level`, keep only records whose message or target contains `search`
/// (case-insensitive), then keep the newest `limit`.
pub(crate) fn filter_recent(
    records: Vec<LogRecord>,
    limit: usize,
    min_level: Option<LogLevel>,
    search: Option<&str>,
) -> Vec<LogRecord> {
    let min_rank = min_level.map(LogLevel::rank).unwrap_or(0);
    let needle = search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);

    let mut out: Vec<LogRecord> = records
        .into_iter()
        .filter(|r| level_rank(r.level) >= min_rank)
        .filter(|r| match &needle {
            Some(q) => r.message.to_lowercase().contains(q) || r.target.to_lowercase().contains(q),
            None => true,
        })
        .collect();
    if out.len() > limit {
        out.drain(0..out.len() - limit);
    }
    out
}

/// List `.log` files in the logs dir, newest first. Empty if the dir is absent.
pub fn list_log_files_core() -> Vec<LogFileInfo> {
    let dir = crate::paths::codeg_logs_root();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<LogFileInfo> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("log") {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            let meta = entry.metadata().ok()?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            Some(LogFileInfo {
                name,
                size_bytes: meta.len(),
                modified_ms,
            })
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.modified_ms));
    files
}

/// Ensure the logs dir exists and return its absolute path. The caller (desktop
/// frontend) reveals it via the opener — mirrors `experts_open_central_dir`, so
/// no opener dependency or cfg-gating is needed here, and it compiles in server
/// mode too.
pub fn open_logs_dir_core() -> Result<String, AppCommandError> {
    let dir = crate::paths::codeg_logs_root();
    std::fs::create_dir_all(&dir).map_err(|e| {
        AppCommandError::io_error("Failed to create log directory").with_detail(e.to_string())
    })?;
    Ok(dir.to_string_lossy().into_owned())
}

/// Read a single on-disk log file (server-mode download / paginate). Returns
/// the newest `max_bytes` when capped (logs are append-only, so the tail is the
/// most recent). `name` must be a bare `.log` filename inside the logs dir;
/// traversal attempts are rejected.
pub fn read_log_file_core(name: &str, max_bytes: Option<usize>) -> Result<String, AppCommandError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains("..")
    {
        return Err(AppCommandError::invalid_input("Invalid log file name"));
    }
    if !name.ends_with(".log") {
        return Err(AppCommandError::invalid_input("Not a log file"));
    }

    let dir = crate::paths::codeg_logs_root();
    let path = dir.join(name);

    // Defense in depth: the resolved file's parent must be the logs dir itself,
    // so even a symlink planted under the dir can't redirect the read outside.
    let canonical_dir = std::fs::canonicalize(&dir).map_err(|e| {
        AppCommandError::io_error("Log directory unavailable").with_detail(e.to_string())
    })?;
    let canonical_file = std::fs::canonicalize(&path)
        .map_err(|e| AppCommandError::not_found("Log file not found").with_detail(e.to_string()))?;
    if canonical_file.parent() != Some(canonical_dir.as_path()) {
        return Err(AppCommandError::invalid_input(
            "Log file escapes the log directory",
        ));
    }

    // Read only the newest `cap` bytes (logs are append-only → tail is most
    // recent), clamped to READ_LOG_MAX_BYTES so a single download can't OOM.
    // We seek to the tail rather than reading the whole file into memory.
    use std::io::{Read, Seek, SeekFrom};
    let len = std::fs::metadata(&canonical_file)
        .map_err(|e| {
            AppCommandError::io_error("Failed to stat log file").with_detail(e.to_string())
        })?
        .len();
    let cap = max_bytes
        .map(|m| m.min(READ_LOG_MAX_BYTES))
        .unwrap_or(READ_LOG_MAX_BYTES) as u64;
    let start = len.saturating_sub(cap);

    let mut file = std::fs::File::open(&canonical_file).map_err(|e| {
        AppCommandError::io_error("Failed to read log file").with_detail(e.to_string())
    })?;
    if start > 0 {
        file.seek(SeekFrom::Start(start)).map_err(|e| {
            AppCommandError::io_error("Failed to seek log file").with_detail(e.to_string())
        })?;
    }
    let mut buf = Vec::new();
    file.take(len - start).read_to_end(&mut buf).map_err(|e| {
        AppCommandError::io_error("Failed to read log file").with_detail(e.to_string())
    })?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------------------------------------------------------------------
// Tauri command wrappers (desktop only)
// ---------------------------------------------------------------------------

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub fn record_frontend_turn_trace(trace: FrontendTurnTrace) -> Result<(), AppCommandError> {
    record_frontend_turn_trace_core(trace)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_log_settings(
    db: State<'_, AppDatabase>,
) -> Result<LogSettingsView, AppCommandError> {
    get_log_settings_core(&db.conn).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_log_settings(
    settings: LogSettings,
    db: State<'_, AppDatabase>,
    app: tauri::AppHandle,
) -> Result<LogSettings, AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    set_log_settings_core(&db.conn, settings, &emitter).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_recent_logs(
    limit: usize,
    min_level: Option<LogLevel>,
    search: Option<String>,
) -> Result<Vec<LogRecord>, AppCommandError> {
    Ok(get_recent_logs_core(limit, min_level, search.as_deref()))
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_log_files() -> Result<Vec<LogFileInfo>, AppCommandError> {
    Ok(list_log_files_core())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_logs_dir() -> Result<String, AppCommandError> {
    open_logs_dir_core()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::hub::{log_hub, LogHub};
    use crate::logging::init::detached_reload_handle;
    use crate::logging::layer::BufferEmitLayer;
    use tracing_subscriber::prelude::*;

    fn rec(level: &'static str, target: &str, message: &str) -> LogRecord {
        LogRecord {
            seq: 0,
            timestamp_ms: 0,
            level,
            target: target.to_string(),
            message: message.to_string(),
            fields: std::collections::BTreeMap::new(),
            spans: Vec::new(),
        }
    }

    fn sample_frontend_turn_trace() -> FrontendTurnTrace {
        FrontendTurnTrace {
            phase: FrontendTurnTracePhase::PromptingFrame,
            client_timestamp_ms: 1_234.5,
            context_key: Some("tab-1".to_string()),
            connection_id: Some("conn-1".to_string()),
            conversation_id: Some(42),
            client_message_id: Some("message-1".to_string()),
            live_message_id: Some("live-1".to_string()),
            event_seq: Some(7),
            received_at_ms: Some(1_230.0),
            elapsed_ms: Some(4.5),
            outcome: None,
            sink_registered: Some(true),
            canonical_accepted: Some(true),
            transcript_published: Some(false),
            has_live_transcript: Some(false),
        }
    }

    #[test]
    fn frontend_turn_trace_enters_the_existing_log_hub_as_secret_safe_fields() {
        LogHub::install(detached_reload_handle());
        let before = log_hub()
            .unwrap()
            .snapshot()
            .into_iter()
            .filter(|record| record.target == "codeg_frontend_turn")
            .count();
        let subscriber = tracing_subscriber::Registry::default().with(BufferEmitLayer);

        tracing::subscriber::with_default(subscriber, || {
            record_frontend_turn_trace_core(sample_frontend_turn_trace()).unwrap();
        });

        let records: Vec<LogRecord> = log_hub()
            .unwrap()
            .snapshot()
            .into_iter()
            .filter(|record| record.target == "codeg_frontend_turn")
            .skip(before)
            .collect();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.level, "INFO");
        assert_eq!(record.message, "frontend turn milestone");
        assert_eq!(
            record.fields.get("phase").map(String::as_str),
            Some("prompting_frame")
        );
        assert_eq!(
            record.fields.get("conversation_id").map(String::as_str),
            Some("42")
        );
        assert_eq!(
            record.fields.get("sink_registered").map(String::as_str),
            Some("true")
        );
        assert!(!record.fields.keys().any(|key| matches!(
            key.as_str(),
            "prompt" | "prompt_text" | "content" | "content_text" | "text"
        )));
    }

    #[test]
    fn frontend_turn_trace_rejects_invalid_timing_and_oversized_correlators() {
        let mut non_finite = sample_frontend_turn_trace();
        non_finite.client_timestamp_ms = f64::INFINITY;
        assert!(record_frontend_turn_trace_core(non_finite).is_err());

        let mut oversized = sample_frontend_turn_trace();
        oversized.context_key = Some("x".repeat(257));
        assert!(record_frontend_turn_trace_core(oversized).is_err());
    }

    #[test]
    fn filter_recent_min_level_keeps_at_or_above() {
        let recs = vec![
            rec("DEBUG", "a", "d"),
            rec("INFO", "a", "i"),
            rec("WARN", "a", "w"),
            rec("ERROR", "a", "e"),
        ];
        let out = filter_recent(recs, 100, Some(LogLevel::Warn), None);
        let msgs: Vec<&str> = out.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, vec!["w", "e"]);
    }

    #[test]
    fn filter_recent_search_matches_message_and_target_case_insensitive() {
        let recs = vec![
            rec("INFO", "acp", "hello"),
            rec("INFO", "web", "world"),
            rec("INFO", "ACP", "other"),
        ];
        assert_eq!(
            filter_recent(recs.clone(), 100, None, Some("HELLO")).len(),
            1
        );
        // Both "acp" and "ACP" targets match (case-insensitive).
        assert_eq!(filter_recent(recs, 100, None, Some("acp")).len(), 2);
    }

    #[test]
    fn filter_recent_keeps_newest_limit() {
        let recs: Vec<LogRecord> = (0..10)
            .map(|i| rec("INFO", "a", &format!("m{i}")))
            .collect();
        let out = filter_recent(recs, 3, None, None);
        let msgs: Vec<String> = out.iter().map(|r| r.message.clone()).collect();
        assert_eq!(msgs, vec!["m7", "m8", "m9"]);
    }

    #[test]
    fn read_log_file_rejects_traversal_and_bad_names() {
        assert!(read_log_file_core("", None).is_err());
        assert!(read_log_file_core("../etc/passwd", None).is_err());
        assert!(read_log_file_core("sub/dir.log", None).is_err());
        assert!(read_log_file_core("..\\win.log", None).is_err());
        assert!(read_log_file_core("notalog.txt", None).is_err());
    }

    #[test]
    fn read_log_file_reads_full_and_tail() {
        let tmp = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("CODEG_HOME", None::<&str>),
                ("CODEG_DATA_DIR", Some(tmp.path().to_str().unwrap())),
            ],
            || {
                let logs = crate::paths::codeg_logs_root();
                std::fs::create_dir_all(&logs).unwrap();
                std::fs::write(logs.join("codeg.2026-01-01.log"), b"0123456789").unwrap();
                assert_eq!(
                    read_log_file_core("codeg.2026-01-01.log", None).unwrap(),
                    "0123456789"
                );
                // Capped read returns the newest tail.
                assert_eq!(
                    read_log_file_core("codeg.2026-01-01.log", Some(4)).unwrap(),
                    "6789"
                );
            },
        );
    }

    #[test]
    fn list_log_files_lists_only_dot_log() {
        let tmp = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("CODEG_HOME", None::<&str>),
                ("CODEG_DATA_DIR", Some(tmp.path().to_str().unwrap())),
            ],
            || {
                let logs = crate::paths::codeg_logs_root();
                std::fs::create_dir_all(&logs).unwrap();
                std::fs::write(logs.join("a.log"), b"a").unwrap();
                std::fs::write(logs.join("b.txt"), b"b").unwrap();
                let files = list_log_files_core();
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].name, "a.log");
            },
        );
    }

    #[test]
    fn webview2_internal_logs_are_excluded_from_ordinary_access() {
        let tmp = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("CODEG_HOME", None::<&str>),
                ("CODEG_DATA_DIR", Some(tmp.path().to_str().unwrap())),
            ],
            || {
                let logs = crate::paths::codeg_logs_root();
                let internal = logs.join("webview2-internal");
                std::fs::create_dir_all(&internal).unwrap();
                std::fs::write(logs.join("codeg.2026-08-08.log"), b"ordinary").unwrap();
                std::fs::write(
                    internal.join("webview2-42-20260808T010203Z.log"),
                    b"internal",
                )
                .unwrap();

                let listed = list_log_files_core();
                assert_eq!(
                    listed
                        .iter()
                        .map(|file| file.name.as_str())
                        .collect::<Vec<_>>(),
                    vec!["codeg.2026-08-08.log"]
                );
                assert_eq!(
                    read_log_file_core("codeg.2026-08-08.log", None).unwrap(),
                    "ordinary"
                );
                assert!(read_log_file_core(
                    "webview2-internal/webview2-42-20260808T010203Z.log",
                    None,
                )
                .is_err());
            },
        );
    }

    #[tokio::test]
    async fn log_settings_round_trip_persists_json() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        // Default when unset.
        assert_eq!(
            get_log_settings_core(&db.conn).await.unwrap().level,
            LogLevel::Info
        );
        // Persist + read back; Noop emitter avoids needing a broadcaster.
        set_log_settings_core(
            &db.conn,
            LogSettings {
                level: LogLevel::Debug,
                targets: Vec::new(),
            },
            &EventEmitter::Noop,
        )
        .await
        .unwrap();
        assert_eq!(
            get_log_settings_core(&db.conn).await.unwrap().level,
            LogLevel::Debug
        );
        let raw = app_metadata_service::get_value(&db.conn, LOGGING_LEVEL_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw, r#"{"level":"debug"}"#);
    }
}
